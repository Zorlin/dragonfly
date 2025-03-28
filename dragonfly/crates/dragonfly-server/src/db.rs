use anyhow::{anyhow, Result};
use chrono::Utc;
use sqlx::{Pool, Sqlite, SqlitePool, Row};
use tokio::sync::OnceCell;
use tracing::{error, info};
use uuid::Uuid;
use std::fs::{File, OpenOptions};
use std::path::Path;
use serde_json;

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
            os_installed TEXT,
            status TEXT NOT NULL,
            disks TEXT, -- JSON array of disk info
            nameservers TEXT, -- JSON array of nameservers
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        )
        "#,
    )
    .execute(&pool)
    .await?;
    
    // Run database migrations
    migrate_db(&pool).await?;
    
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
    let pool = get_pool().await?;
    let now = Utc::now();
    let now_str = now.to_rfc3339();
    
    // First check if a machine with this MAC address already exists
    let existing_machine = sqlx::query(
        r#"
        SELECT id FROM machines WHERE mac_address = ?
        "#,
    )
    .bind(&req.mac_address)
    .fetch_optional(pool)
    .await?;
    
    if let Some(row) = existing_machine {
        // Machine already exists, update it
        let machine_id_str: String = row.get(0);
        let machine_id = Uuid::parse_str(&machine_id_str)?;
        
        // Serialize disks and nameservers as JSON
        let disks_json = serde_json::to_string(&req.disks)?;
        let nameservers_json = serde_json::to_string(&req.nameservers)?;
        
        // Update the existing machine's IP, hostname, disks, and nameservers
        sqlx::query(
            r#"
            UPDATE machines 
            SET ip_address = ?, hostname = ?, disks = ?, nameservers = ?, updated_at = ?
            WHERE id = ?
            "#,
        )
        .bind(&req.ip_address)
        .bind(&req.hostname)
        .bind(&disks_json)
        .bind(&nameservers_json)
        .bind(&now_str)
        .bind(machine_id.to_string())
        .execute(pool)
        .await?;
        
        info!("Updated existing machine with ID: {}", machine_id);
        return Ok(machine_id);
    }
    
    // Machine doesn't exist, create a new one
    let machine_id = Uuid::new_v4();
    
    // Serialize disks and nameservers as JSON
    let disks_json = serde_json::to_string(&req.disks)?;
    let nameservers_json = serde_json::to_string(&req.nameservers)?;
    
    // Insert the new machine
    let result = sqlx::query(
        r#"
        INSERT INTO machines (id, mac_address, ip_address, hostname, os_choice, os_installed, status, disks, nameservers, created_at, updated_at)
        VALUES (?, ?, ?, ?, NULL, NULL, ?, ?, ?, ?, ?)
        "#,
    )
    .bind(machine_id.to_string())
    .bind(&req.mac_address)
    .bind(&req.ip_address)
    .bind(&req.hostname)
    .bind(MachineStatus::ReadyForAdoption.to_string())
    .bind(&disks_json)
    .bind(&nameservers_json)
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
        SELECT id, mac_address, ip_address, hostname, os_choice, os_installed, status, disks, nameservers, created_at, updated_at 
        FROM machines
        "#,
    )
    .fetch_all(pool)
    .await?;
    
    let mut machines = Vec::new();
    for row in rows {
        let id: String = row.get(0);
        let mac_address: String = row.get(1);
        let status_str: String = row.get(6);
        let disks_json: Option<String> = row.get(7);
        let nameservers_json: Option<String> = row.get(8);
        
        // Generate memorable name from MAC address
        let memorable_name = dragonfly_common::mac_to_words::mac_to_words_safe(&mac_address);
        
        // Deserialize disks and nameservers from JSON or use empty vectors if null
        let mut disks = if let Some(json) = disks_json {
            serde_json::from_str::<Vec<dragonfly_common::models::DiskInfo>>(&json).unwrap_or_else(|_| Vec::new())
        } else {
            Vec::new()
        };
        
        // Calculate precise disk sizes with 2 decimal places
        for disk in &mut disks {
            if disk.size_bytes > 1099511627776 {
                disk.calculated_size = Some(format!("{:.2} TB", disk.size_bytes as f64 / 1099511627776.0));
            } else if disk.size_bytes > 1073741824 {
                disk.calculated_size = Some(format!("{:.2} GB", disk.size_bytes as f64 / 1073741824.0));
            } else if disk.size_bytes > 1048576 {
                disk.calculated_size = Some(format!("{:.2} MB", disk.size_bytes as f64 / 1048576.0));
            } else if disk.size_bytes > 1024 {
                disk.calculated_size = Some(format!("{:.2} KB", disk.size_bytes as f64 / 1024.0));
            } else {
                disk.calculated_size = Some(format!("{} bytes", disk.size_bytes));
            }
        }
        
        let nameservers = if let Some(json) = nameservers_json {
            serde_json::from_str::<Vec<String>>(&json).unwrap_or_else(|_| Vec::new())
        } else {
            Vec::new()
        };
        
        // Parse status and ensure os_choice is set when we have ExistingOS
        let status = parse_status(&status_str);
        
        // os_choice is separate from status now
        let os_choice: Option<String> = row.get(4);
        
        machines.push(Machine {
            id: Uuid::parse_str(&id).unwrap_or_default(),
            mac_address,
            ip_address: row.get(2),
            hostname: row.get(3),
            os_choice,
            os_installed: row.get(5),
            status,
            disks,
            nameservers,
            created_at: parse_datetime(&row.get::<String, _>(9)),
            updated_at: parse_datetime(&row.get::<String, _>(10)),
            memorable_name: Some(memorable_name),
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
        SELECT id, mac_address, ip_address, hostname, os_choice, os_installed, status, disks, nameservers, created_at, updated_at 
        FROM machines 
        WHERE id = ?
        "#,
    )
    .bind(id.to_string())
    .fetch_optional(pool)
    .await?;
    
    if let Some(row) = result {
        let id: String = row.get(0);
        let mac_address: String = row.get(1);
        let status_str: String = row.get(6);
        let disks_json: Option<String> = row.get(7);
        let nameservers_json: Option<String> = row.get(8);
        
        // Generate memorable name from MAC address
        let memorable_name = dragonfly_common::mac_to_words::mac_to_words_safe(&mac_address);
        
        // Deserialize disks and nameservers from JSON or use empty vectors if null
        let mut disks = if let Some(json) = disks_json {
            serde_json::from_str::<Vec<dragonfly_common::models::DiskInfo>>(&json).unwrap_or_else(|_| Vec::new())
        } else {
            Vec::new()
        };
        
        // Calculate precise disk sizes with 2 decimal places
        for disk in &mut disks {
            if disk.size_bytes > 1099511627776 {
                disk.calculated_size = Some(format!("{:.2} TB", disk.size_bytes as f64 / 1099511627776.0));
            } else if disk.size_bytes > 1073741824 {
                disk.calculated_size = Some(format!("{:.2} GB", disk.size_bytes as f64 / 1073741824.0));
            } else if disk.size_bytes > 1048576 {
                disk.calculated_size = Some(format!("{:.2} MB", disk.size_bytes as f64 / 1048576.0));
            } else if disk.size_bytes > 1024 {
                disk.calculated_size = Some(format!("{:.2} KB", disk.size_bytes as f64 / 1024.0));
            } else {
                disk.calculated_size = Some(format!("{} bytes", disk.size_bytes));
            }
        }
        
        let nameservers = if let Some(json) = nameservers_json {
            serde_json::from_str::<Vec<String>>(&json).unwrap_or_else(|_| Vec::new())
        } else {
            Vec::new()
        };
        
        // Parse status and ensure os_choice is set when we have ExistingOS
        let status = parse_status(&status_str);
        
        // os_choice is separate from status now
        let os_choice: Option<String> = row.get(4);
        
        Ok(Some(Machine {
            id: Uuid::parse_str(&id).unwrap_or_default(),
            mac_address,
            ip_address: row.get(2),
            hostname: row.get(3),
            os_choice,
            os_installed: row.get(5),
            status,
            disks,
            nameservers,
            created_at: parse_datetime(&row.get::<String, _>(9)),
            updated_at: parse_datetime(&row.get::<String, _>(10)),
            memorable_name: Some(memorable_name),
        }))
    } else {
        Ok(None)
    }
}

// Get machine by MAC address
pub async fn get_machine_by_mac(mac_address: &str) -> Result<Option<Machine>> {
    let pool = get_pool().await?;
    
    let result = sqlx::query(
        r#"
        SELECT id, mac_address, ip_address, hostname, os_choice, os_installed, status, disks, nameservers, created_at, updated_at 
        FROM machines 
        WHERE mac_address = ?
        "#,
    )
    .bind(mac_address)
    .fetch_optional(pool)
    .await?;
    
    if let Some(row) = result {
        let id: String = row.get(0);
        let mac_address: String = row.get(1);
        let status_str: String = row.get(6);
        let disks_json: Option<String> = row.get(7);
        let nameservers_json: Option<String> = row.get(8);
        
        // Generate memorable name from MAC address
        let memorable_name = dragonfly_common::mac_to_words::mac_to_words_safe(&mac_address);
        
        // Deserialize disks and nameservers from JSON or use empty vectors if null
        let mut disks = if let Some(json) = disks_json {
            serde_json::from_str::<Vec<dragonfly_common::models::DiskInfo>>(&json).unwrap_or_else(|_| Vec::new())
        } else {
            Vec::new()
        };
        
        // Calculate precise disk sizes with 2 decimal places
        for disk in &mut disks {
            if disk.size_bytes > 1099511627776 {
                disk.calculated_size = Some(format!("{:.2} TB", disk.size_bytes as f64 / 1099511627776.0));
            } else if disk.size_bytes > 1073741824 {
                disk.calculated_size = Some(format!("{:.2} GB", disk.size_bytes as f64 / 1073741824.0));
            } else if disk.size_bytes > 1048576 {
                disk.calculated_size = Some(format!("{:.2} MB", disk.size_bytes as f64 / 1048576.0));
            } else if disk.size_bytes > 1024 {
                disk.calculated_size = Some(format!("{:.2} KB", disk.size_bytes as f64 / 1024.0));
            } else {
                disk.calculated_size = Some(format!("{} bytes", disk.size_bytes));
            }
        }
        
        let nameservers = if let Some(json) = nameservers_json {
            serde_json::from_str::<Vec<String>>(&json).unwrap_or_else(|_| Vec::new())
        } else {
            Vec::new()
        };
        
        // Parse status and ensure os_choice is set when we have ExistingOS
        let status = parse_status(&status_str);
        
        // os_choice is separate from status now
        let os_choice: Option<String> = row.get(4);
        
        Ok(Some(Machine {
            id: Uuid::parse_str(&id).unwrap_or_default(),
            mac_address,
            ip_address: row.get(2),
            hostname: row.get(3),
            os_choice,
            os_installed: row.get(5),
            status,
            disks,
            nameservers,
            created_at: parse_datetime(&row.get::<String, _>(9)),
            updated_at: parse_datetime(&row.get::<String, _>(10)),
            memorable_name: Some(memorable_name),
        }))
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
    .bind(MachineStatus::InstallingOS.to_string())
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
    
    // We no longer need special handling for ExistingOS since the OS info is in os_installed
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

// Update machine hostname
pub async fn update_hostname(id: &Uuid, hostname: &str) -> Result<bool> {
    let pool = get_pool().await?;
    let now = Utc::now();
    let now_str = now.to_rfc3339();
    
    let result = sqlx::query(
        r#"
        UPDATE machines 
        SET hostname = ?, updated_at = ? 
        WHERE id = ?
        "#,
    )
    .bind(hostname)
    .bind(&now_str)
    .bind(id.to_string())
    .execute(pool)
    .await?;
    
    let success = result.rows_affected() > 0;
    if success {
        info!("Hostname updated for machine {}: {}", id, hostname);
    } else {
        info!("No machine found with ID {} to update hostname", id);
    }
    
    Ok(success)
}

// Update OS installed on machine
pub async fn update_os_installed(id: &Uuid, os_installed: &str) -> Result<bool> {
    let pool = get_pool().await?;
    let now = Utc::now();
    let now_str = now.to_rfc3339();
    
    let result = sqlx::query(
        r#"
        UPDATE machines 
        SET os_installed = ?, updated_at = ? 
        WHERE id = ?
        "#,
    )
    .bind(os_installed)
    .bind(&now_str)
    .bind(id.to_string())
    .execute(pool)
    .await?;
    
    let success = result.rows_affected() > 0;
    if success {
        info!("OS installed updated for machine {}: {}", id, os_installed);
    } else {
        info!("No machine found with ID {} to update OS installed", id);
    }
    
    Ok(success)
}

// Helper function to parse status from string
fn parse_status(status_str: &str) -> MachineStatus {
    if status_str.starts_with("ExistingOS: ") || status_str == "Existing OS" {
        return MachineStatus::ExistingOS;
    }
    
    match status_str {
        "ReadyForAdoption" => MachineStatus::ReadyForAdoption,
        "InstallingOS" => MachineStatus::InstallingOS,
        "Ready" => MachineStatus::Ready,
        "Offline" => MachineStatus::Offline,
        s if s.starts_with("Error: ") => {
            let message = s.trim_start_matches("Error: ").to_string();
            MachineStatus::Error(message)
        },
        _ => MachineStatus::Error(format!("Unknown status: {}", status_str)),
    }
}

// Helper function to parse datetime from string
fn parse_datetime(datetime_str: &str) -> chrono::DateTime<Utc> {
    chrono::DateTime::parse_from_rfc3339(datetime_str)
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or_else(|_| Utc::now())
}

// Apply database migrations
async fn migrate_db(pool: &Pool<Sqlite>) -> Result<()> {
    // Check if os_installed column exists
    let result = sqlx::query(
        r#"
        SELECT COUNT(*) AS count FROM pragma_table_info('machines') WHERE name = 'os_installed'
        "#,
    )
    .fetch_one(pool)
    .await?;
    
    let column_exists: i64 = result.get(0);
    
    // Add os_installed column if it doesn't exist
    if column_exists == 0 {
        info!("Adding os_installed column to machines table");
        sqlx::query(
            r#"
            ALTER TABLE machines ADD COLUMN os_installed TEXT
            "#,
        )
        .execute(pool)
        .await?;
        
        // If we have ExistingOS machines, update their os_installed field
        let existing_os_machines = sqlx::query(
            r#"
            SELECT id, status FROM machines WHERE status LIKE 'ExistingOS:%' OR status = 'Existing OS'
            "#,
        )
        .fetch_all(pool)
        .await?;
        
        let now = Utc::now();
        let now_str = now.to_rfc3339();
        
        for row in existing_os_machines {
            let id: String = row.get(0);
            let status_str: String = row.get(1);
            let os = if status_str.starts_with("ExistingOS: ") {
                status_str.trim_start_matches("ExistingOS: ").to_string()
            } else {
                "Unknown".to_string() // Fallback for "Existing OS" format
            };
            
            info!("Setting os_installed for machine {} to {}", id, os);
            sqlx::query(
                r#"
                UPDATE machines 
                SET os_installed = ?, updated_at = ?, status = ? 
                WHERE id = ?
                "#,
            )
            .bind(os)
            .bind(&now_str)
            .bind("Existing OS") // Update to the new format
            .bind(id)
            .execute(pool)
            .await?;
        }
    }
    
    Ok(())
} 