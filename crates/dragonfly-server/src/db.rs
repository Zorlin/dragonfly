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
// Make re-exports public and correct the imported names
pub use dragonfly_common::models::{OsAssignmentRequest, RegisterResponse, ErrorResponse}; // Removed UpdateTagsRequest, corrected others
use crate::auth::{Credentials, Settings};
use crate::tinkerbell::WorkflowInfo;

// Global database pool
static DB_POOL: OnceCell<Pool<Sqlite>> = OnceCell::const_new();

// Initialize the database connection pool
pub async fn init_db() -> Result<SqlitePool> {
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
            id TEXT PRIMARY KEY NOT NULL,
            mac_address TEXT UNIQUE NOT NULL,
            ip_address TEXT,
            hostname TEXT,
            status TEXT NOT NULL,
            os_choice TEXT,
            os_installed TEXT,
            disks TEXT, -- JSON representation
            nameservers TEXT, -- JSON representation
            memorable_name TEXT,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            bmc_credentials TEXT, -- JSON representation
            installation_progress INTEGER DEFAULT 0,
            installation_step TEXT,
            last_deployment_duration INTEGER, -- Duration in seconds
            cpu_model TEXT,
            cpu_cores INTEGER,
            total_ram_bytes INTEGER, -- Store as u64 (INTEGER in SQLite)
            proxmox_vmid INTEGER,
            proxmox_node TEXT,
            is_proxmox_host BOOLEAN DEFAULT FALSE NOT NULL
        )
        "#,
    )
    .execute(&pool)
    .await?;
    
    // Create admin_credentials table
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS admin_credentials (
            id INTEGER PRIMARY KEY,
            username TEXT NOT NULL,
            password_hash TEXT NOT NULL,
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
    if let Err(_) = DB_POOL.set(pool.clone()) {
        return Err(anyhow!("Failed to set global database pool"));
    }
    
    info!("Database initialized successfully");
    Ok(pool)
}

// Get a reference to the database pool
// Make this public so handlers can access it
pub async fn get_pool() -> Result<&'static Pool<Sqlite>> {
    DB_POOL.get().ok_or_else(|| anyhow!("Database pool not initialized"))
}

// Register a new machine or update an existing one based on MAC address
pub async fn register_machine(req: &RegisterRequest) -> Result<Uuid> {
    let pool = get_pool().await?;
    let now = Utc::now();
    let now_str = now.to_rfc3339();

    // Use UUID v5 based on MAC address for deterministic ID
    let namespace = uuid::Uuid::NAMESPACE_DNS;
    let machine_id = uuid::Uuid::new_v5(&namespace, req.mac_address.as_bytes());
    
    // Generate memorable name
    let memorable_name = dragonfly_common::mac_to_words::mac_to_words_safe(&req.mac_address);
    
    // Serialize disks and nameservers
    let disks_json = serde_json::to_string(&req.disks).unwrap_or_else(|_| "[]".to_string());
    let nameservers_json = serde_json::to_string(&req.nameservers).unwrap_or_else(|_| "[]".to_string());

    // Determine initial/update status
    let current_status = if req.proxmox_vmid.is_some() || req.proxmox_node.is_some() {
        MachineStatus::ExistingOS 
    } else {
        MachineStatus::AwaitingAssignment
    };
    let status_json = serde_json::to_string(&current_status)?;

    // Determine if this is being registered as a Proxmox host
    let is_proxmox_host = req.proxmox_node.is_some() && req.proxmox_vmid.is_none();

    // Begin transaction
    let mut tx = pool.begin().await?;

    // Check if machine exists by MAC address
    let existing_machine_id: Option<String> = sqlx::query("SELECT id FROM machines WHERE mac_address = ?")
        .bind(&req.mac_address)
        .fetch_optional(&mut *tx)
        .await?
        .map(|row| row.get("id"));

    let returned_id = match existing_machine_id {
        Some(existing_id_str) => {
            // --- UPDATE existing machine --- 
            let existing_id = Uuid::parse_str(&existing_id_str)?;
            info!("Updating existing machine: ID={}, MAC={}", existing_id, req.mac_address);

            // Perform UPDATE
            sqlx::query(
                r#"
                UPDATE machines SET
                    ip_address = ?,
                    hostname = ?,
                    status = ?,
                    os_choice = ?,
                    os_installed = ?,
                    disks = ?,
                    nameservers = ?,
                    memorable_name = ?,
                    updated_at = ?,
                    cpu_model = ?,
                    cpu_cores = ?,
                    total_ram_bytes = ?,
                    proxmox_vmid = ?,
                    proxmox_node = ?,
                    is_proxmox_host = ? 
                WHERE id = ?
                "#,
            )
            .bind(&req.ip_address)
            .bind(req.hostname.as_deref()) 
            .bind(&status_json) // Always update status for simplicity now
            .bind(None::<String>) // os_choice - Resetting for now, maybe fetch existing later?
            .bind(None::<String>) // os_installed - Resetting for now, maybe fetch existing later?
            .bind(&disks_json) 
            .bind(&nameservers_json) 
            .bind(&memorable_name) // Update memorable name too
            .bind(&now_str) // updated_at
            .bind(req.cpu_model.as_deref())
            .bind(req.cpu_cores.map(|c| c as i64)) 
            .bind(req.total_ram_bytes.map(|r| r as i64)) 
            .bind(req.proxmox_vmid.map(|v| v as i64)) 
            .bind(req.proxmox_node.as_deref())
            .bind(is_proxmox_host) 
            .bind(existing_id.to_string())
            .execute(&mut *tx)
            .await?;
            
            existing_id // Return the existing ID
        }
        None => {
            // --- INSERT new machine --- 
            info!("Inserting new machine: ID={}, MAC={}", machine_id, req.mac_address);

            sqlx::query(
                r#"
                INSERT INTO machines (
                    id, mac_address, ip_address, hostname, status, os_choice, os_installed, 
                    disks, nameservers, memorable_name, created_at, updated_at, 
                    cpu_model, cpu_cores, total_ram_bytes, 
                    proxmox_vmid, proxmox_node, is_proxmox_host
                )
                VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                "#,
            )
            .bind(machine_id.to_string())
            .bind(&req.mac_address)
            .bind(&req.ip_address) 
            .bind(req.hostname.as_deref()) 
            .bind(&status_json) 
            .bind(None::<String>) // os_choice
            .bind(None::<String>) // os_installed
            .bind(&disks_json) 
            .bind(&nameservers_json) 
            .bind(memorable_name) 
            .bind(&now_str) // created_at
            .bind(&now_str) // updated_at
            .bind(req.cpu_model.as_deref())
            .bind(req.cpu_cores.map(|c| c as i64)) 
            .bind(req.total_ram_bytes.map(|r| r as i64)) 
            .bind(req.proxmox_vmid.map(|v| v as i64)) 
            .bind(req.proxmox_node.as_deref())
            .bind(is_proxmox_host) 
            .execute(&mut *tx)
            .await?;
            
            machine_id // Return the newly generated ID
        }
    };

    // Commit transaction
    tx.commit().await?;
    
    info!("Machine upsert complete: ID={}, MAC={}, IP={}, Hostname={:?}, ProxmoxNode={:?}, IsHost={}", 
          returned_id, req.mac_address, req.ip_address, req.hostname, req.proxmox_node, is_proxmox_host);
          
    Ok(returned_id)
}

// Fetch all machines from the database
pub async fn get_all_machines() -> Result<Vec<Machine>> {
    let pool = get_pool().await?;
    
    // Explicitly list all columns, including the new is_proxmox_host
    let rows = sqlx::query(
        r#"
        SELECT 
            id, mac_address, ip_address, hostname, status, os_choice, os_installed, 
            disks, nameservers, memorable_name, created_at, updated_at, bmc_credentials, 
            installation_progress, installation_step, last_deployment_duration, 
            cpu_model, cpu_cores, total_ram_bytes, 
            proxmox_vmid, proxmox_node, is_proxmox_host 
        FROM machines
        ORDER BY hostname, memorable_name, mac_address -- Add sorting for consistent order
        "#,
    )
    .fetch_all(pool)
    .await?;
    
    let mut machines = Vec::new();
    for row in rows {
        // Use the helper that includes hardware fields
        match map_row_to_machine_with_hardware(row) {
            Ok(machine) => machines.push(machine),
            Err(e) => {
                // Log the error but continue processing other rows
                error!("Failed to map row to machine: {}", e);
            }
        }
    }
    
    Ok(machines)
}

// Fetch a single machine by its ID
pub async fn get_machine_by_id(id: &Uuid) -> Result<Option<Machine>> {
    let pool = get_pool().await?;
    
    // Explicitly list all columns
    let result = sqlx::query(
        r#"
        SELECT 
               id, mac_address, ip_address, hostname, status, os_choice, os_installed, 
               disks, nameservers, memorable_name, created_at, updated_at, bmc_credentials, 
               installation_progress, installation_step, last_deployment_duration,
               cpu_model, cpu_cores, total_ram_bytes, 
               proxmox_vmid, proxmox_node, is_proxmox_host
        FROM machines 
        WHERE id = ?
        "#,
    )
    .bind(id.to_string())
    .fetch_optional(pool)
    .await?;
    
    if let Some(row) = result {
        let machine = map_row_to_machine_with_hardware(row)?;
        Ok(Some(machine))
    } else {
        Ok(None)
    }
}

// Fetch a single machine by its MAC address
pub async fn get_machine_by_mac(mac_address: &str) -> Result<Option<Machine>> {
    let pool = get_pool().await?;
    
    // Explicitly list all columns
    let result = sqlx::query(
        r#"
        SELECT 
               id, mac_address, ip_address, hostname, status, os_choice, os_installed, 
               disks, nameservers, memorable_name, created_at, updated_at, bmc_credentials, 
               installation_progress, installation_step, last_deployment_duration,
               cpu_model, cpu_cores, total_ram_bytes, 
               proxmox_vmid, proxmox_node, is_proxmox_host
        FROM machines 
        WHERE mac_address = ?
        "#,
    )
    .bind(mac_address)
    .fetch_optional(pool)
    .await?;
    
    if let Some(row) = result {
        let machine = map_row_to_machine_with_hardware(row)?;
        Ok(Some(machine))
    } else {
        Ok(None)
    }
}

// Fetch a single machine by its Proxmox VMID
pub async fn get_machine_by_proxmox_vmid(vmid: u32) -> Result<Option<Machine>> {
    let pool = get_pool().await?;
    
    // Explicitly list all columns
    let result = sqlx::query(
        r#"
        SELECT 
               id, mac_address, ip_address, hostname, status, os_choice, os_installed, 
               disks, nameservers, memorable_name, created_at, updated_at, bmc_credentials, 
               installation_progress, installation_step, last_deployment_duration,
               cpu_model, cpu_cores, total_ram_bytes, 
               proxmox_vmid, proxmox_node, is_proxmox_host
        FROM machines 
        WHERE proxmox_vmid = ?
        "#,
    )
    .bind(vmid as i64) // Convert u32 to i64 for SQLite
    .fetch_optional(pool)
    .await?;
    
    if let Some(row) = result {
        let machine = map_row_to_machine_with_hardware(row)?;
        Ok(Some(machine))
    } else {
        Ok(None)
    }
}

// Get machine by IP address
pub async fn get_machine_by_ip(ip_address: &str) -> Result<Option<Machine>> {
    let pool = get_pool().await?;
    
    let result = sqlx::query(
        r#"
        SELECT id, mac_address, ip_address, hostname, os_choice, os_installed, status, 
               disks, nameservers, created_at, updated_at, bmc_credentials, 
               installation_progress, installation_step, 
               -- Add new hardware columns
               cpu_model, cpu_cores, total_ram_bytes 
        FROM machines 
        WHERE ip_address = ?
        "#,
    )
    .bind(ip_address)
    .fetch_optional(pool)
    .await?;
    
    if let Some(row) = result {
        let machine = map_row_to_machine_with_hardware(row)?; // Use a new helper
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
    .bind(serde_json::to_string(&MachineStatus::InstallingOS)?)
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
    
    // Store the serialized enum value directly
    let status_json = serde_json::to_string(&status)?;
    
    let result = sqlx::query(
        r#"
        UPDATE machines 
        SET status = ?, updated_at = ? 
        WHERE id = ?
        "#,
    )
    .bind(status_json)
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

// Update BMC credentials for a machine
pub async fn update_bmc_credentials(id: &Uuid, credentials: &dragonfly_common::models::BmcCredentials) -> Result<bool> {
    let pool = get_pool().await?;
    let now = Utc::now();
    let now_str = now.to_rfc3339();
    
    // Convert credentials to JSON
    let credentials_json = serde_json::to_string(credentials)?;
    
    let result = sqlx::query(
        r#"
        UPDATE machines 
        SET bmc_credentials = ?, updated_at = ? 
        WHERE id = ?
        "#,
    )
    .bind(credentials_json)
    .bind(&now_str)
    .bind(id.to_string())
    .execute(pool)
    .await?;
    
    let success = result.rows_affected() > 0;
    if success {
        info!("BMC credentials updated for machine {}", id);
    } else {
        info!("No machine found with ID {} to update BMC credentials", id);
    }
    
    Ok(success)
}

// Update machine IP address
pub async fn update_ip_address(id: &Uuid, ip_address: &str) -> Result<bool> {
    let pool = get_pool().await?;
    let now = Utc::now();
    let now_str = now.to_rfc3339();
    
    let result = sqlx::query(
        r#"
        UPDATE machines 
        SET ip_address = ?, updated_at = ? 
        WHERE id = ?
        "#,
    )
    .bind(ip_address)
    .bind(&now_str)
    .bind(id.to_string())
    .execute(pool)
    .await?;
    
    let success = result.rows_affected() > 0;
    if success {
        info!("IP address updated for machine {}: {}", id, ip_address);
    } else {
        info!("No machine found with ID {} to update IP address", id);
    }
    
    Ok(success)
}

// Update machine MAC address
pub async fn update_mac_address(id: &Uuid, mac_address: &str) -> Result<bool> {
    let pool = get_pool().await?;
    let now = Utc::now();
    let now_str = now.to_rfc3339();
    
    // First check if a machine with this MAC address already exists
    let existing_machine = sqlx::query(
        r#"
        SELECT id FROM machines WHERE mac_address = ?
        "#,
    )
    .bind(mac_address)
    .fetch_optional(pool)
    .await?;
    
    if let Some(row) = existing_machine {
        let existing_id: String = row.get(0);
        if existing_id != id.to_string() {
            // MAC address is already in use by another machine
            return Err(anyhow!("MAC address is already in use by another machine"));
        }
    }
    
    let result = sqlx::query(
        r#"
        UPDATE machines 
        SET mac_address = ?, updated_at = ? 
        WHERE id = ?
        "#,
    )
    .bind(mac_address)
    .bind(&now_str)
    .bind(id.to_string())
    .execute(pool)
    .await?;
    
    let success = result.rows_affected() > 0;
    if success {
        info!("MAC address updated for machine {}: {}", id, mac_address);
    } else {
        info!("No machine found with ID {} to update MAC address", id);
    }
    
    Ok(success)
}

// Update machine DNS servers
pub async fn update_nameservers(id: &Uuid, nameservers: &[String]) -> Result<bool> {
    let pool = get_pool().await?;
    let now = Utc::now();
    let now_str = now.to_rfc3339();
    
    let nameservers_json = serde_json::to_string(nameservers)?;
    
    let result = sqlx::query(
        r#"
        UPDATE machines 
        SET nameservers = ?, updated_at = ?
        WHERE id = ?
        "#,
    )
    .bind(nameservers_json)
    .bind(now_str)
    .bind(id.to_string())
    .execute(pool)
    .await?;
    
    Ok(result.rows_affected() > 0)
}

// Helper function to parse status from string
fn parse_status(status_str: &str) -> MachineStatus {
    // First try to deserialize from JSON
    if let Ok(status) = serde_json::from_str::<MachineStatus>(status_str) {
        return status;
    }
    
    // Fallback for legacy data
    if status_str.starts_with("ExistingOS: ") || status_str == "Existing OS" {
        return MachineStatus::ExistingOS;
    }
    
    match status_str {
        "AwaitingAssignment" => MachineStatus::AwaitingAssignment,
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
    let dt = chrono::DateTime::parse_from_rfc3339(datetime_str)
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or_else(|_| Utc::now());
    
    // Format without subsecond precision and then parse back
    let formatted = dt.format("%Y-%m-%d %H:%M:%S UTC").to_string();
    chrono::DateTime::parse_from_str(&formatted, "%Y-%m-%d %H:%M:%S %Z")
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or(dt)
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
    
    // Check if bmc_credentials column exists
    let result = sqlx::query(
        r#"
        SELECT COUNT(*) AS count FROM pragma_table_info('machines') WHERE name = 'bmc_credentials'
        "#,
    )
    .fetch_one(pool)
    .await?;
    
    let column_exists: i64 = result.get(0);
    
    // Add bmc_credentials column if it doesn't exist
    if column_exists == 0 {
        info!("Adding bmc_credentials column to machines table");
        sqlx::query(
            r#"
            ALTER TABLE machines ADD COLUMN bmc_credentials TEXT
            "#,
        )
        .execute(pool)
        .await?;
    }
    
    // Check if installation_progress column exists
    let result = sqlx::query(
        r#"
        SELECT COUNT(*) AS count FROM pragma_table_info('machines') WHERE name = 'installation_progress'
        "#,
    )
    .fetch_one(pool)
    .await?;
    
    let column_exists: i64 = result.get(0);
    
    // Add installation_progress column if it doesn't exist
    if column_exists == 0 {
        info!("Adding installation_progress column to machines table");
        sqlx::query(
            r#"
            ALTER TABLE machines ADD COLUMN installation_progress INTEGER DEFAULT 0
            "#,
        )
        .execute(pool)
        .await?;
    }
    
    // Check if installation_step column exists
    let result = sqlx::query(
        r#"
        SELECT COUNT(*) AS count FROM pragma_table_info('machines') WHERE name = 'installation_step'
        "#,
    )
    .fetch_one(pool)
    .await?;
    
    let column_exists: i64 = result.get(0);
    
    // Add installation_step column if it doesn't exist
    if column_exists == 0 {
        info!("Adding installation_step column to machines table");
        sqlx::query(
            r#"
            ALTER TABLE machines ADD COLUMN installation_step TEXT
            "#,
        )
        .execute(pool)
        .await?;
    }
    
    // Check if last_deployment_duration column exists
    let result = sqlx::query(
        r#"
        SELECT COUNT(*) AS count FROM pragma_table_info('machines') WHERE name = 'last_deployment_duration'
        "#,
    )
    .fetch_one(pool)
    .await?;
    
    let duration_column_exists: i64 = result.get(0);
    
    // Add last_deployment_duration column if it doesn't exist
    if duration_column_exists == 0 {
        info!("Adding last_deployment_duration column to machines table");
        sqlx::query(
            r#"
            ALTER TABLE machines ADD COLUMN last_deployment_duration INTEGER
            "#,
        )
        .execute(pool)
        .await?;
    }
    
    // Check if default_os column exists in app_settings table
    let result = sqlx::query(
        r#"
        SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='app_settings'
        "#,
    )
    .fetch_one(pool)
    .await?;
    
    let table_exists: i64 = result.get(0);
    
    if table_exists > 0 {
        // Table exists, check for the column
        let result = sqlx::query(
            r#"
            SELECT COUNT(*) AS count FROM pragma_table_info('app_settings') WHERE name = 'default_os'
            "#,
        )
        .fetch_one(pool)
        .await?;
        
        let column_exists: i64 = result.get(0);
        
        // Add default_os column if it doesn't exist
        if column_exists == 0 {
            info!("Adding default_os column to app_settings table");
            sqlx::query(
                r#"
                ALTER TABLE app_settings ADD COLUMN default_os TEXT
                "#,
            )
            .execute(pool)
            .await?;
        }
    }
    
    // Check if setup_completed column exists in app_settings table
    let result = sqlx::query(
        r#"
        SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='app_settings'
        "#,
    )
    .fetch_one(pool)
    .await?;
    
    let table_exists: i64 = result.get(0);
    
    if table_exists > 0 {
        // Check if setup_completed column exists
        let result = sqlx::query(
            r#"
            SELECT COUNT(*) AS count FROM pragma_table_info('app_settings') WHERE name = 'setup_completed'
            "#,
        )
        .fetch_one(pool)
        .await?;
        
        let column_exists: i64 = result.get(0);
        
        // Add setup_completed column if it doesn't exist
        if column_exists == 0 {
            info!("Adding setup_completed column to app_settings table");
            sqlx::query(
                r#"
                ALTER TABLE app_settings ADD COLUMN setup_completed BOOLEAN NOT NULL DEFAULT 0
                "#,
            )
            .execute(pool)
            .await?;
        }
    }
    
    // Add cpu_model column if it doesn't exist
    let result = sqlx::query("SELECT COUNT(*) FROM pragma_table_info('machines') WHERE name = 'cpu_model'").fetch_one(pool).await?;
    let column_exists: i64 = result.get(0);
    if column_exists == 0 {
        info!("Adding cpu_model column to machines table");
        sqlx::query("ALTER TABLE machines ADD COLUMN cpu_model TEXT").execute(pool).await?;
    }

    // Add cpu_cores column if it doesn't exist
    let result = sqlx::query("SELECT COUNT(*) FROM pragma_table_info('machines') WHERE name = 'cpu_cores'").fetch_one(pool).await?;
    let column_exists: i64 = result.get(0);
    if column_exists == 0 {
        info!("Adding cpu_cores column to machines table");
        sqlx::query("ALTER TABLE machines ADD COLUMN cpu_cores INTEGER").execute(pool).await?;
    }

    // Add total_ram_bytes column if it doesn't exist
    let result = sqlx::query("SELECT COUNT(*) FROM pragma_table_info('machines') WHERE name = 'total_ram_bytes'").fetch_one(pool).await?;
    let column_exists: i64 = result.get(0);
    if column_exists == 0 {
        info!("Adding total_ram_bytes column to machines table");
        sqlx::query("ALTER TABLE machines ADD COLUMN total_ram_bytes INTEGER").execute(pool).await?;
    }
    
    // Add proxmox_vmid column if it doesn't exist
    let result = sqlx::query("SELECT COUNT(*) FROM pragma_table_info('machines') WHERE name = 'proxmox_vmid'").fetch_one(pool).await?;
    let column_exists: i64 = result.get(0);
    if column_exists == 0 {
        info!("Adding proxmox_vmid column to machines table");
        sqlx::query("ALTER TABLE machines ADD COLUMN proxmox_vmid INTEGER").execute(pool).await?;
    }
    
    // Add proxmox_node column if it doesn't exist
    let result = sqlx::query("SELECT COUNT(*) FROM pragma_table_info('machines') WHERE name = 'proxmox_node'").fetch_one(pool).await?;
    let column_exists: i64 = result.get(0);
    if column_exists == 0 {
        info!("Adding proxmox_node column to machines table");
        sqlx::query("ALTER TABLE machines ADD COLUMN proxmox_node TEXT").execute(pool).await?;
    }
    
    // Add memorable_name column if it doesn't exist
    let result = sqlx::query("SELECT COUNT(*) FROM pragma_table_info('machines') WHERE name = 'memorable_name'").fetch_one(pool).await?;
    let column_exists: i64 = result.get(0);
    if column_exists == 0 {
        info!("Adding memorable_name column to machines table");
        sqlx::query("ALTER TABLE machines ADD COLUMN memorable_name TEXT").execute(pool).await?;
    }
    
    // Check if is_proxmox_host column exists
    let result = sqlx::query(
        r#"
        SELECT COUNT(*) AS count FROM pragma_table_info('machines') WHERE name = 'is_proxmox_host'
        "#,
    )
    .fetch_one(pool)
    .await?;
    
    let column_exists: i64 = result.get(0);
    
    // Add is_proxmox_host column if it doesn't exist
    if column_exists == 0 {
        info!("Adding is_proxmox_host column to machines table");
        sqlx::query(
            r#"
            ALTER TABLE machines ADD COLUMN is_proxmox_host BOOLEAN DEFAULT FALSE NOT NULL
            "#,
        )
        .execute(pool)
        .await?;

        // Backfill: Set is_proxmox_host = TRUE for existing machines that look like hosts
        info!("Backfilling is_proxmox_host flag for existing potential Proxmox hosts...");
        let backfill_result = sqlx::query(
            r#"
            UPDATE machines 
            SET is_proxmox_host = TRUE 
            WHERE proxmox_node IS NOT NULL AND proxmox_vmid IS NULL
            "#
        )
        .execute(pool)
        .await?;
        info!("Backfill complete. Updated {} rows.", backfill_result.rows_affected());
    }
    
    Ok(())
}

// Delete a machine by ID
pub async fn delete_machine(id: &Uuid) -> Result<bool> {
    let pool = get_pool().await?;
    
    let result = sqlx::query(
        r#"
        DELETE FROM machines 
        WHERE id = ?
        "#,
    )
    .bind(id.to_string())
    .execute(pool)
    .await?;
    
    let success = result.rows_affected() > 0;
    if success {
        info!("Machine deleted from database: {}", id);
    } else {
        info!("No machine found with ID {} to delete", id);
    }
    
    Ok(success)
}

// Get admin credentials from database
pub async fn get_admin_credentials() -> Result<Option<Credentials>> {
    let pool = get_pool().await?;
    
    let row = sqlx::query(
        r#"
        SELECT username, password_hash FROM admin_credentials ORDER BY id DESC LIMIT 1
        "#,
    )
    .fetch_optional(pool)
    .await?;
    
    if let Some(row) = row {
        let username: String = row.get(0);
        let password_hash: String = row.get(1);
        
        Ok(Some(Credentials {
            username,
            password: None,
            password_hash,
        }))
    } else {
        Ok(None)
    }
}

// Save admin credentials to database
pub async fn save_admin_credentials(credentials: &Credentials) -> Result<()> {
    // Make sure the database pool is initialized
    let pool = get_pool().await?;
    let now = Utc::now();
    let now_str = now.to_rfc3339();
    
    // Use a transaction to ensure atomicity
    let mut tx = pool.begin().await?;
    
    // Check if credentials already exist
    let existing = sqlx::query("SELECT COUNT(*) FROM admin_credentials")
        .fetch_one(&mut *tx)
        .await?;
    
    let count: i64 = existing.get(0);
    
    if count > 0 {
        // Update existing credentials
        sqlx::query(
            r#"
            UPDATE admin_credentials 
            SET username = ?, password_hash = ?, updated_at = ?
            WHERE id = (SELECT id FROM admin_credentials ORDER BY id DESC LIMIT 1)
            "#,
        )
        .bind(&credentials.username)
        .bind(&credentials.password_hash)
        .bind(&now_str)
        .execute(&mut *tx)
        .await?;
        
        info!("Updated existing admin credentials for user: {}", credentials.username);
    } else {
        // Insert new credentials
        sqlx::query(
            r#"
            INSERT INTO admin_credentials (username, password_hash, created_at, updated_at)
            VALUES (?, ?, ?, ?)
            "#,
        )
        .bind(&credentials.username)
        .bind(&credentials.password_hash)
        .bind(&now_str)
        .bind(&now_str)
        .execute(&mut *tx)
        .await?;
        
        info!("Created new admin credentials for user: {}", credentials.username);
    }
    
    // Commit the transaction
    tx.commit().await?;
    
    // Verify the save worked by retrieving the credentials again
    match get_admin_credentials().await {
        Ok(Some(_)) => {
            info!("Successfully verified admin credentials were saved");
            Ok(())
        },
        _ => {
            error!("Failed to verify admin credentials were saved - this is a critical error!");
            Err(anyhow!("Failed to verify admin credentials were saved"))
        }
    }
}

// Get application settings from database
pub async fn get_app_settings() -> Result<Settings> {
    let pool = get_pool().await?;
    
    // First, make sure the settings table exists
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS app_settings (
            id INTEGER PRIMARY KEY CHECK (id = 1), -- Only one settings record allowed
            require_login BOOLEAN NOT NULL,
            default_os TEXT,
            setup_completed BOOLEAN NOT NULL DEFAULT 0,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        )
        "#,
    )
    .execute(pool)
    .await?;
    
    // Try to get settings
    let row = sqlx::query(
        r#"
        SELECT require_login, default_os, setup_completed FROM app_settings WHERE id = 1
        "#,
    )
    .fetch_optional(pool)
    .await?;
    
    // Start with default settings and make it mutable
    let mut settings = Settings::default();
    
    if let Some(row) = row {
        // Update settings from the fetched row
        settings.require_login = row.get::<bool, _>("require_login");
        settings.default_os = row.get::<Option<String>, _>("default_os");
        settings.setup_completed = row.get::<bool, _>("setup_completed");
        
        // Load admin credentials separately to populate those fields in the default settings struct
        // Note: This might introduce a small inconsistency if DB ops fail between here and AppState creation,
        // but it resolves the immediate panic. A better approach might involve restructuring Settings.
        if let Ok(Some(creds)) = get_admin_credentials().await {
            settings.admin_username = creds.username;
            settings.admin_password_hash = creds.password_hash;
        }
    } else {
        // No settings found, insert defaults for app_settings table
        info!("No settings found in app_settings table, inserting defaults.");
        let now = Utc::now();
        let now_str = now.to_rfc3339();
        
        sqlx::query(
            r#"
            INSERT INTO app_settings (id, require_login, default_os, setup_completed, created_at, updated_at)
            VALUES (1, ?, ?, ?, ?, ?)
            "#,
        )
        .bind(settings.require_login)    // Use defaults (now accessible)
        .bind(&settings.default_os)       // Use defaults (now accessible)
        .bind(settings.setup_completed)  // Use defaults (now accessible)
        .bind(&now_str)
        .bind(&now_str)
        .execute(pool)
        .await?;
    }
    
    // Return the potentially modified settings struct
    Ok(settings)
}

// Save application settings to database
pub async fn save_app_settings(settings: &Settings) -> Result<()> {
    let pool = get_pool().await?;
    let now = Utc::now();
    let now_str = now.to_rfc3339();
    
    // Update existing settings or insert if they don't exist (upsert pattern)
    sqlx::query(
        r#"
        INSERT INTO app_settings (id, require_login, default_os, setup_completed, created_at, updated_at)
        VALUES (1, ?, ?, ?, ?, ?)
        ON CONFLICT (id) DO UPDATE SET
        require_login = excluded.require_login,
        default_os = excluded.default_os,
        setup_completed = excluded.setup_completed,
        updated_at = excluded.updated_at
        "#,
    )
    .bind(settings.require_login)
    .bind(&settings.default_os)
    .bind(settings.setup_completed)
    .bind(&now_str)
    .bind(&now_str)
    .execute(pool)
    .await?;
    
    Ok(())
}

// Update installation progress
pub async fn update_installation_progress(id: &Uuid, progress: u8, step: Option<&str>) -> Result<bool> {
    let pool = get_pool().await?;
    let now = Utc::now();
    let now_str = now.to_rfc3339();
    
    // Use different query paths based on whether step is provided
    let result = if let Some(step_value) = step {
        sqlx::query(
            r#"
            UPDATE machines 
            SET installation_progress = ?, installation_step = ?, updated_at = ? 
            WHERE id = ?
            "#,
        )
        .bind(progress as i64)
        .bind(step_value)
        .bind(&now_str)
        .bind(id.to_string())
        .execute(pool)
        .await?
    } else {
        sqlx::query(
            r#"
            UPDATE machines 
            SET installation_progress = ?, updated_at = ? 
            WHERE id = ?
            "#,
        )
        .bind(progress as i64)
        .bind(&now_str)
        .bind(id.to_string())
        .execute(pool)
        .await?
    };
    
    let success = result.rows_affected() > 0;
    if success {
        if let Some(step_value) = step {
            info!("Installation progress updated for machine {}: {}% ({})", id, progress, step_value);
        } else {
            info!("Installation progress updated for machine {}: {}%", id, progress);
        }
    } else {
        info!("No machine found with ID {} to update installation progress", id);
    }
    
    Ok(success)
}

// Update machine in the database
pub async fn update_machine(machine: &Machine) -> Result<bool> {
    let pool = get_pool().await?;
    
    // Serialize the status enum to JSON for storage
    let status_json = serde_json::to_string(&machine.status)?;
    let nameservers_json = serde_json::to_string(&machine.nameservers)?;
    let disks_json = serde_json::to_string(&machine.disks)?;

    // Log the update attempt with detailed info, including hardware
    info!("Updating machine {} in database: status={:?}, cpu={:?}, cores={:?}, ram={:?}", 
          machine.id, machine.status, machine.cpu_model, machine.cpu_cores, machine.total_ram_bytes);
    
    // Create a plain SQL query to update the machine, including hardware fields
    let query = "
        UPDATE machines SET 
            hostname = $1, 
            ip_address = $2, 
            mac_address = $3, 
            nameservers = $4,
            status = $5,
            disks = $6,
            os_choice = $7,
            updated_at = $8,
            last_deployment_duration = $9,
            -- Add hardware fields
            cpu_model = $10,
            cpu_cores = $11,
            total_ram_bytes = $12
        WHERE id = $13
    ";
    
    // Execute the update query with explicit type annotation for SqlitePool
    let result = sqlx::query::<sqlx::Sqlite>(query)
        .bind(machine.hostname.as_deref())
        .bind(&machine.ip_address)
        .bind(&machine.mac_address)
        .bind(&nameservers_json)
        .bind(&status_json)
        .bind(&disks_json)
        .bind(machine.os_choice.as_deref())
        .bind(machine.updated_at) // Use the timestamp from the input machine struct
        .bind(machine.last_deployment_duration)
        // Bind hardware fields
        .bind(machine.cpu_model.as_deref())
        .bind(machine.cpu_cores.map(|c| c as i64)) // Map Option<u32> to Option<i64>
        .bind(machine.total_ram_bytes.map(|r| r as i64)) // Map Option<u64> to Option<i64>
        // Bind ID last
        .bind(machine.id)
        .execute(pool)
        .await;
        
    match result {
        Ok(result) => {
            let rows_affected = result.rows_affected();
            info!("Database update for machine {} affected {} rows", machine.id, rows_affected);
            Ok(rows_affected > 0)
        },
        Err(e) => {
            error!("Failed to update machine in database: {}", e);
            Err(anyhow::anyhow!("Database error: {}", e))
        }
    }
}

// Add a new type for template timing data
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct TemplateTiming {
    pub template_name: String,
    pub action_name: String,
    pub durations: Vec<u64>,
}

// Save template timing data to database
pub async fn save_template_timing(template_name: &str, action_name: &str, durations: &[u64]) -> Result<bool> {
    const MAX_TIMING_HISTORY: usize = 50; // Keep only the last 50 runs of timing data
    
    let pool = get_pool().await?;
    
    info!("Saving timing data for template {}, action {}", template_name, action_name);
    
    // Limit the durations to the most recent MAX_TIMING_HISTORY entries
    let limited_durations = if durations.len() > MAX_TIMING_HISTORY {
        &durations[durations.len() - MAX_TIMING_HISTORY..]
    } else {
        durations
    };
    
    // Convert durations to JSON
    let durations_json = serde_json::to_string(limited_durations)?;
    
    // Create a plain SQL query to insert or update timing data
    let query = "
        INSERT INTO template_timings (template_name, action_name, durations)
        VALUES ($1, $2, $3)
        ON CONFLICT (template_name, action_name) 
        DO UPDATE SET durations = $3
    ";
    
    // Execute the query
    let result = sqlx::query::<sqlx::Sqlite>(query)
        .bind(template_name)
        .bind(action_name)
        .bind(durations_json)
        .execute(pool)
        .await?;
    
    Ok(result.rows_affected() > 0)
}

// Load all template timing data from database
pub async fn load_template_timings() -> Result<Vec<TemplateTiming>> {
    let pool = get_pool().await?;
    
    info!("Loading all template timing data");
    
    // Create a plain SQL query to select all timing data
    let query = "
        SELECT template_name, action_name, durations FROM template_timings
    ";
    
    // Execute the query
    let rows = sqlx::query::<sqlx::Sqlite>(query)
        .fetch_all(pool)
        .await?;
    
    // Convert rows to TemplateTiming structs
    let mut timings = Vec::new();
    for row in rows {
        let template_name: String = row.get(0);
        let action_name: String = row.get(1);
        let durations_json: String = row.get(2);
        
        // Parse durations from JSON
        let durations: Vec<u64> = serde_json::from_str(&durations_json)?;
        
        timings.push(TemplateTiming {
            template_name,
            action_name,
            durations,
        });
    }
    
    Ok(timings)
}

// Initialize database schema for template timing data
pub async fn init_timing_tables() -> Result<()> {
    let pool = get_pool().await?;
    
    info!("Initializing template timing tables");
    
    // Create table for template timings if it doesn't exist
    let create_table_query = "
        CREATE TABLE IF NOT EXISTS template_timings (
            template_name TEXT NOT NULL,
            action_name TEXT NOT NULL,
            durations TEXT NOT NULL,
            PRIMARY KEY (template_name, action_name)
        )
    ";
    
    sqlx::query::<sqlx::Sqlite>(create_table_query)
        .execute(pool)
        .await?;
    
    Ok(())
}

// Get statistics about the template timing database
pub async fn get_timing_database_stats() -> Result<(usize, usize, usize)> {
    let pool = get_pool().await?;
    
    // Count the number of templates
    let template_count_result = sqlx::query::<sqlx::Sqlite>(
        "SELECT COUNT(DISTINCT template_name) FROM template_timings"
    )
    .fetch_one(pool)
    .await?;
    
    let template_count: i64 = template_count_result.get(0);
    
    // Count the total number of template/action combinations
    let action_count_result = sqlx::query::<sqlx::Sqlite>(
        "SELECT COUNT(*) FROM template_timings"
    )
    .fetch_one(pool)
    .await?;
    
    let action_count: i64 = action_count_result.get(0);
    
    // Calculate the total number of timing entries
    let rows = sqlx::query::<sqlx::Sqlite>(
        "SELECT durations FROM template_timings"
    )
    .fetch_all(pool)
    .await?;
    
    let mut total_entries = 0;
    for row in rows {
        let durations_json: String = row.get(0);
        if let Ok(durations) = serde_json::from_str::<Vec<u64>>(&durations_json) {
            total_entries += durations.len();
        }
    }
    
    Ok((template_count as usize, action_count as usize, total_entries))
}

pub async fn store_completed_workflow(machine_id: &Uuid, workflow_info: &WorkflowInfo) -> Result<()> {
    let pool = get_pool().await?;
    
    // Store workflow info as JSON
    let workflow_json = serde_json::to_string(workflow_info)?;
    let machine_id_str = machine_id.to_string();
    
    // Store with current timestamp using SQLite's datetime('now')
    sqlx::query!(
        "INSERT INTO completed_workflows (machine_id, workflow_info, completed_at) VALUES ($1, $2, datetime('now'))",
        machine_id_str,
        workflow_json
    )
    .execute(pool)
    .await?;
    
    Ok(())
}

pub async fn get_completed_workflow(machine_id: &Uuid) -> Result<Option<(WorkflowInfo, chrono::DateTime<chrono::Utc>)>> {
    let pool = get_pool().await?;
    let machine_id_str = machine_id.to_string();
    
    // Get workflow info only if completed within the last minute
    let record = sqlx::query!(
        "SELECT workflow_info, completed_at FROM completed_workflows 
         WHERE machine_id = $1 
         AND completed_at > datetime('now', '-1 minute')
         ORDER BY completed_at DESC LIMIT 1",
        machine_id_str
    )
    .fetch_optional(pool)
    .await?;
    
    if let Some(record) = record {
        let workflow_info: WorkflowInfo = serde_json::from_str(&record.workflow_info)?;
        // Parse the SQLite datetime string into chrono::DateTime<Utc>
        let completed_at = chrono::DateTime::parse_from_rfc3339(&format!("{}Z", record.completed_at.to_string().replace(" ", "T")))?
            .with_timezone(&chrono::Utc);
        Ok(Some((workflow_info, completed_at)))
    } else {
        Ok(None)
    }
}

// Get all machines with a specific status
pub async fn get_machines_by_status(status: dragonfly_common::models::MachineStatus) -> Result<Vec<dragonfly_common::models::Machine>> {
    let pool = get_pool().await?;
    
    // Convert the status to a JSON string for comparison
    let status_json = serde_json::to_string(&status)?;
    
    // Use regular query instead of query macro to avoid compile-time verification issues
    let rows = sqlx::query(
        "SELECT * FROM machines WHERE status = ?"
    )
    .bind(status_json)
    .fetch_all(pool)
    .await?;
    
    let mut machines = Vec::with_capacity(rows.len());
    for row in rows {
        machines.push(map_row_to_machine_with_hardware(row)?);
    }
    
    Ok(machines)
}

// NEW helper function to map a row including hardware info
fn map_row_to_machine_with_hardware(row: sqlx::sqlite::SqliteRow) -> Result<Machine> {
    use sqlx::Row;
    
    let id: String = row.try_get("id")?;
    let mac_address: String = row.try_get("mac_address")?;
    let status_str: String = row.try_get("status")?;
    let disks_json: Option<String> = row.try_get("disks")?;
    let nameservers_json: Option<String> = row.try_get("nameservers")?;
    let bmc_credentials_json: Option<String> = row.try_get("bmc_credentials")?;
    let last_deployment_duration: Option<i64> = row.try_get("last_deployment_duration").ok();
    
    // Map hardware info (use try_get for Option types)
    let cpu_model: Option<String> = row.try_get("cpu_model")?;
    let cpu_cores_i64: Option<i64> = row.try_get("cpu_cores")?;
    let cpu_cores: Option<u32> = cpu_cores_i64.map(|c| c as u32);
    let total_ram_bytes_i64: Option<i64> = row.try_get("total_ram_bytes")?;
    let total_ram_bytes: Option<u64> = total_ram_bytes_i64.map(|r| r as u64);
    
    // Map Proxmox specific fields
    let proxmox_vmid_i64: Option<i64> = row.try_get("proxmox_vmid").ok();
    let proxmox_vmid: Option<u32> = proxmox_vmid_i64.map(|vmid| vmid as u32);
    let proxmox_node: Option<String> = row.try_get("proxmox_node").ok();
    let memorable_name: Option<String> = row.try_get("memorable_name").ok();
    
    // Generate memorable name from MAC address if not already stored
    let memorable_name = memorable_name.unwrap_or_else(|| 
        dragonfly_common::mac_to_words::mac_to_words_safe(&mac_address)
    );
    
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
    
    // Deserialize BMC credentials if present
    let bmc_credentials = if let Some(json) = bmc_credentials_json {
        serde_json::from_str::<dragonfly_common::models::BmcCredentials>(&json).ok()
    } else {
        None
    };
    
    // Parse status
    let status = parse_status(&status_str);
    
    let os_choice: Option<String> = row.try_get("os_choice")?;
    
    let created_at_str: String = row.try_get("created_at")?;
    let updated_at_str: String = row.try_get("updated_at")?;
    
    Ok(dragonfly_common::models::Machine {
        id: Uuid::parse_str(&id).unwrap_or_default(),
        mac_address,
        ip_address: row.try_get("ip_address")?,
        hostname: row.try_get("hostname")?,
        os_choice,
        os_installed: row.try_get("os_installed")?,
        status,
        disks,
        nameservers,
        created_at: parse_datetime(&created_at_str),
        updated_at: parse_datetime(&updated_at_str),
        memorable_name: Some(memorable_name),
        bmc_credentials,
        installation_progress: row.try_get::<Option<i64>, _>("installation_progress").unwrap_or(None).unwrap_or(0) as u8,
        installation_step: row.try_get("installation_step")?,
        last_deployment_duration,
        // Add hardware fields
        cpu_model,
        cpu_cores,
        total_ram_bytes,
        // Add Proxmox fields
        proxmox_vmid,
        proxmox_node,
        is_proxmox_host: row.try_get("is_proxmox_host")?,
    })
}

// ---- START TAGS FUNCTIONS ----

// STUB: Get machine tags
pub async fn get_machine_tags(id: &Uuid) -> Result<Vec<String>> {
    info!("STUB: Called get_machine_tags for machine {}", id);
    // TODO: Implement database logic to fetch tags for the given machine ID.
    // This will likely require schema changes (e.g., a separate machine_tags table or a tags column in machines).
    Ok(vec!["stub_tag".to_string()]) // Return dummy data for now
}

// STUB: Update machine tags
pub async fn update_machine_tags(id: &Uuid, tags: &[String]) -> Result<bool> {
    info!("STUB: Called update_machine_tags for machine {} with tags: {:?}", id, tags);
    // TODO: Implement database logic to update tags for the given machine ID.
    // This will likely involve deleting existing tags and inserting the new ones.
    // Requires schema changes.
    Ok(true) // Assume success for now
}

// ---- END TAGS FUNCTIONS ----

// Update setup completion status
pub async fn mark_setup_completed(completed: bool) -> Result<()> {
    let pool = get_pool().await?;
    let now = Utc::now();
    let now_str = now.to_rfc3339();
    
    // First make sure the settings table exists
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS app_settings (
            id INTEGER PRIMARY KEY CHECK (id = 1),
            require_login BOOLEAN NOT NULL DEFAULT 0,
            default_os TEXT,
            setup_completed BOOLEAN NOT NULL DEFAULT 0,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        )
        "#,
    )
    .execute(pool)
    .await?;
    
    // Check if settings record exists
    let result = sqlx::query("SELECT COUNT(*) FROM app_settings WHERE id = 1")
        .fetch_one(pool)
        .await?;
    
    let count: i64 = result.get(0);
    
    if count > 0 {
        // Update existing record
        sqlx::query(
            r#"
            UPDATE app_settings 
            SET setup_completed = ?, updated_at = ?
            WHERE id = 1
            "#,
        )
        .bind(completed)
        .bind(&now_str)
        .execute(pool)
        .await?;
    } else {
        // Create a new record with default values and the specified setup_completed
        sqlx::query(
            r#"
            INSERT INTO app_settings (id, require_login, default_os, setup_completed, created_at, updated_at)
            VALUES (1, 0, NULL, ?, ?, ?)
            "#,
        )
        .bind(completed)
        .bind(&now_str)
        .bind(&now_str)
        .execute(pool)
        .await?;
    }
    
    info!("Setup completion status set to: {}", completed);
    Ok(())
}

// Check if setup has been completed
pub async fn is_setup_completed() -> Result<bool> {
    let pool = get_pool().await?;
    
    // First make sure the settings table exists
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS app_settings (
            id INTEGER PRIMARY KEY CHECK (id = 1),
            require_login BOOLEAN NOT NULL DEFAULT 0,
            default_os TEXT,
            setup_completed BOOLEAN NOT NULL DEFAULT 0,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        )
        "#,
    )
    .execute(pool)
    .await?;
    
    // Try to get the setup_completed value
    let result = sqlx::query("SELECT setup_completed FROM app_settings WHERE id = 1")
        .fetch_optional(pool)
        .await?;
    
    if let Some(row) = result {
        let completed: bool = row.get(0);
        Ok(completed)
    } else {
        // No settings found, setup is not completed
        Ok(false)
    }
}

// Check if the database exists by checking the standard installation path
pub async fn database_exists() -> bool {
    let db_path = "/var/lib/dragonfly/sqlite.db";
    Path::new(db_path).exists()
}

/// Gets all machines with Proxmox information (vmid or node is not null)
pub async fn get_proxmox_machines() -> Result<Vec<Machine>> {
    let pool = get_pool().await?;
    
    let rows = sqlx::query(
        "SELECT * FROM machines WHERE proxmox_vmid IS NOT NULL OR proxmox_node IS NOT NULL ORDER BY hostname ASC"
    )
    .fetch_all(pool)
    .await?;
    
    let mut machines = Vec::new();
    for row in rows {
        let machine = map_row_to_machine_with_hardware(row)?;
        machines.push(machine);
    }
    
    Ok(machines)
} 