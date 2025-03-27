use anyhow::{anyhow, Result};
use chrono::Utc;
use sqlx::{Pool, Sqlite, SqlitePool, Row};
use tokio::sync::OnceCell;
use tracing::{error, info};
use uuid::Uuid;
use std::fs::{File, OpenOptions};
use std::path::Path;

use dragonfly_common::models::{Machine, MachineStatus, RegisterRequest};

// Global database pool
static DB_POOL: OnceCell<Pool<Sqlite>> = OnceCell::const_new();

// Initialize the database connection pool
pub async fn init_db() -> Result<()> {
    // Create or open the SQLite database file
    let db_path = "sqlite.db";
    
    // Check if the database file exists and create it if not
    if !Path::new(db_path).exists() {
        info!("Database file doesn't exist, creating it");
        match File::create(db_path) {
            Ok(_) => info!("Created database file: {}", db_path),
            Err(e) => return Err(anyhow!("Failed to create database file: {}", e)),
        }
    }
    
    // Ensure we have correct permissions
    match OpenOptions::new()
        .read(true)
        .write(true)
        .open(db_path)
    {
        Ok(_) => info!("Verified database file is readable and writeable"),
        Err(e) => return Err(anyhow!("Failed to open database file with read/write permissions: {}", e)),
    }
    
    info!("Attempting to open database at: {}", db_path);
    let pool = SqlitePool::connect(&format!("sqlite:{}", db_path)).await?;
    
    // Create tables if they don't exist
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS machines (
            id TEXT PRIMARY KEY,
            mac_address TEXT UNIQUE NOT NULL,
            ip_address TEXT NOT NULL,
            hostname TEXT,
            os_choice TEXT,
            status TEXT NOT NULL,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        )
        "#,
    )
    .execute(&pool)
    .await?;
    
    // Store the pool globally
    if let Err(_) = DB_POOL.set(pool) {
        return Err(anyhow!("Failed to set global database pool"));
    }
    
    info!("Database initialized successfully");
    Ok(())
}

// Get a reference to the database pool
async fn get_pool() -> Result<&'static Pool<Sqlite>> {
    DB_POOL.get().ok_or_else(|| anyhow!("Database pool not initialized"))
}

// Register a new machine
pub async fn register_machine(req: &RegisterRequest) -> Result<Uuid> {
    let machine_id = Uuid::new_v4();
    let now = Utc::now();
    let now_str = now.to_rfc3339();
    
    let pool = get_pool().await?;
    
    // Insert the new machine
    let result = sqlx::query(
        r#"
        INSERT INTO machines (id, mac_address, ip_address, hostname, os_choice, status, created_at, updated_at)
        VALUES (?, ?, ?, ?, NULL, ?, ?, ?)
        "#,
    )
    .bind(machine_id.to_string())
    .bind(&req.mac_address)
    .bind(&req.ip_address)
    .bind(&req.hostname)
    .bind(MachineStatus::Registered.to_string())
    .bind(&now_str)
    .bind(&now_str)
    .execute(pool)
    .await;
    
    match result {
        Ok(_) => {
            info!("Machine registered with ID: {}", machine_id);
            Ok(machine_id)
        }
        Err(e) => {
            error!("Failed to register machine: {}", e);
            Err(anyhow!("Failed to register machine: {}", e))
        }
    }
}

// Get all machines
pub async fn get_all_machines() -> Result<Vec<Machine>> {
    let pool = get_pool().await?;
    
    let rows = sqlx::query(
        r#"
        SELECT id, mac_address, ip_address, hostname, os_choice, status, created_at, updated_at 
        FROM machines
        "#,
    )
    .fetch_all(pool)
    .await?;
    
    let mut machines = Vec::new();
    for row in rows {
        let id: String = row.get(0);
        let status: String = row.get(5);
        
        machines.push(Machine {
            id: Uuid::parse_str(&id).unwrap_or_default(),
            mac_address: row.get(1),
            ip_address: row.get(2),
            hostname: row.get(3),
            os_choice: row.get(4),
            status: parse_status(&status),
            created_at: parse_datetime(&row.get::<String, _>(6)),
            updated_at: parse_datetime(&row.get::<String, _>(7)),
        });
    }
    
    info!("Retrieved {} machines", machines.len());
    Ok(machines)
}

// Get machine by ID
pub async fn get_machine_by_id(id: &Uuid) -> Result<Option<Machine>> {
    let pool = get_pool().await?;
    
    let result = sqlx::query(
        r#"
        SELECT id, mac_address, ip_address, hostname, os_choice, status, created_at, updated_at 
        FROM machines 
        WHERE id = ?
        "#,
    )
    .bind(id.to_string())
    .fetch_optional(pool)
    .await?;
    
    if let Some(row) = result {
        let id: String = row.get(0);
        let status: String = row.get(5);
        
        let machine = Machine {
            id: Uuid::parse_str(&id).unwrap_or_default(),
            mac_address: row.get(1),
            ip_address: row.get(2),
            hostname: row.get(3),
            os_choice: row.get(4),
            status: parse_status(&status),
            created_at: parse_datetime(&row.get::<String, _>(6)),
            updated_at: parse_datetime(&row.get::<String, _>(7)),
        };
        
        Ok(Some(machine))
    } else {
        Ok(None)
    }
}

// Assign OS to a machine
pub async fn assign_os(id: &Uuid, os_choice: &str) -> Result<bool> {
    let pool = get_pool().await?;
    let now = Utc::now();
    let now_str = now.to_rfc3339();
    
    let result = sqlx::query(
        r#"
        UPDATE machines 
        SET os_choice = ?, status = ?, updated_at = ? 
        WHERE id = ?
        "#,
    )
    .bind(os_choice)
    .bind(MachineStatus::InstallingOs.to_string())
    .bind(&now_str)
    .bind(id.to_string())
    .execute(pool)
    .await?;
    
    let success = result.rows_affected() > 0;
    if success {
        info!("OS assigned to machine {}: {}", id, os_choice);
    } else {
        info!("No machine found with ID {} to assign OS", id);
    }
    
    Ok(success)
}

// Update machine status
pub async fn update_status(id: &Uuid, status: MachineStatus) -> Result<bool> {
    let pool = get_pool().await?;
    let now = Utc::now();
    let now_str = now.to_rfc3339();
    
    let result = sqlx::query(
        r#"
        UPDATE machines 
        SET status = ?, updated_at = ? 
        WHERE id = ?
        "#,
    )
    .bind(status.to_string())
    .bind(&now_str)
    .bind(id.to_string())
    .execute(pool)
    .await?;
    
    let success = result.rows_affected() > 0;
    if success {
        info!("Status updated for machine {}: {:?}", id, status);
    } else {
        info!("No machine found with ID {} to update status", id);
    }
    
    Ok(success)
}

// Helper function to parse status from string
fn parse_status(status_str: &str) -> MachineStatus {
    match status_str {
        "Registered" => MachineStatus::Registered,
        "AwaitingOsAssignment" => MachineStatus::AwaitingOsAssignment,
        "InstallingOs" => MachineStatus::InstallingOs,
        "Ready" => MachineStatus::Ready,
        s if s.starts_with("Error:") => {
            let message = s.trim_start_matches("Error: ").to_string();
            MachineStatus::Error(message)
        },
        _ => MachineStatus::Error("Unknown status".to_string()),
    }
}

// Helper function to parse datetime from string
fn parse_datetime(datetime_str: &str) -> chrono::DateTime<Utc> {
    chrono::DateTime::parse_from_rfc3339(datetime_str)
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or_else(|_| Utc::now())
} 