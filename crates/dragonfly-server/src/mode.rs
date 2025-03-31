use std::process::Command;
use std::path::{Path, PathBuf};
use tokio::sync::watch;
use tokio::signal::unix::{signal, SignalKind};
use anyhow::{Result, Context, anyhow};
use tracing::{info, error, warn};
use tracing_appender;
use tracing_subscriber;
use tracing_subscriber::{fmt, prelude::*, EnvFilter};
use dirs;
use std::os::unix::fs::PermissionsExt;
use nix::libc;
use std::str;

// The different deployment modes
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeploymentMode {
    Simple,
    Flight,
    Swarm,
}

impl DeploymentMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            DeploymentMode::Simple => "simple",
            DeploymentMode::Flight => "flight",
            DeploymentMode::Swarm => "swarm",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "simple" => Some(DeploymentMode::Simple),
            "flight" => Some(DeploymentMode::Flight),
            "swarm" => Some(DeploymentMode::Swarm),
            _ => None,
        }
    }
}

// Constants for file paths
const MODE_DIR: &str = "/etc/dragonfly";
const MODE_FILE: &str = "/etc/dragonfly/mode";
const SYSTEMD_UNIT_FILE: &str = "/etc/systemd/system/dragonfly.service";
const K3S_CONFIG_DIR: &str = "/etc/dragonfly/k3s";
const EXECUTABLE_TARGET_PATH: &str = "/usr/local/bin/dragonfly";
const HANDOFF_READY_FILE: &str = "/var/lib/dragonfly/handoff_ready";

// Get the current mode (or None if not set)
pub async fn get_current_mode() -> Result<Option<DeploymentMode>> {
    // Check if the mode file exists
    if !Path::new(MODE_FILE).exists() {
        return Ok(None);
    }

    // Read the mode file
    let content = tokio::fs::read_to_string(MODE_FILE)
        .await
        .context("Failed to read mode file")?;
    
    // Parse the mode
    let mode = DeploymentMode::from_str(content.trim());
    
    Ok(mode)
}

// Save the current mode
pub async fn save_mode(mode: DeploymentMode, already_elevated: bool) -> Result<()> {
    // First check if the mode is already set to the requested value
    if let Ok(Some(current_mode)) = get_current_mode().await {
        if current_mode == mode {
            info!("Deployment mode already set to {}, no changes needed", mode.as_str());
            return Ok(());
        }
    }
    
    // Create the directory if it doesn't exist
    let dir = Path::new(MODE_DIR);
    if !dir.exists() {
        let result = tokio::fs::create_dir_all(dir).await;
        if let Err(e) = result {
            info!("Failed to create mode directory with regular permissions: {}", e);
            
            // Check if we're on macOS and try with graphical sudo
            if is_macos() && !already_elevated {
                info!("Using admin privileges to create mode directory");
                
                // Create a properly escaped command that works with osascript
                // The double quotes around the shell command need to be properly escaped
                let script = format!(
                    r#"do shell script "mkdir -p {0} && echo '{1}' > {2} && chmod 755 {0}" with administrator privileges with prompt "Dragonfly needs permission to save your deployment mode""#,
                    MODE_DIR, mode.as_str(), MODE_FILE
                );
                
                let osa_output = Command::new("osascript")
                    .arg("-e")
                    .arg(&script)
                    .output()
                    .context("Failed to execute osascript for sudo prompt")?;
                    
                if !osa_output.status.success() {
                    let stderr = String::from_utf8_lossy(&osa_output.stderr);
                    return Err(anyhow!("Failed to create mode directory with admin privileges: {}", stderr));
                }
                
                info!("Mode directory created and mode set to: {}", mode.as_str());
                return Ok(());
            } else if !already_elevated {
                // Try with sudo on Linux
                info!("Using sudo to create mode directory");
                let sudo_mkdir = Command::new("sudo")
                    .args(["mkdir", "-p", MODE_DIR])
                    .output()
                    .context("Failed to create mode directory with sudo")?;
                    
                if !sudo_mkdir.status.success() {
                    return Err(anyhow!("Failed to create mode directory with sudo: {}", 
                        String::from_utf8_lossy(&sudo_mkdir.stderr)));
                }
                
                // Now write the mode file with sudo
                let echo_cmd = format!("echo {} | sudo tee {} > /dev/null", mode.as_str(), MODE_FILE);
                let sudo_write = Command::new("sh")
                    .arg("-c")
                    .arg(&echo_cmd)
                    .output()
                    .context("Failed to write mode file with sudo")?;
                    
                if !sudo_write.status.success() {
                    return Err(anyhow!("Failed to write mode file with sudo: {}", 
                        String::from_utf8_lossy(&sudo_write.stderr)));
                }
                
                // Set permissions
                let _ = Command::new("sudo")
                    .args(["chmod", "755", MODE_DIR])
                    .output();
                    
                let _ = Command::new("sudo")
                    .args(["chmod", "644", MODE_FILE])
                    .output();
                
                info!("Mode directory created and mode set to: {}", mode.as_str());
                return Ok(());
            } else {
                return Err(anyhow!("Failed to create mode directory and already attempted elevation: {}", e));
            }
        }
    }

    // Write the mode file
    tokio::fs::write(MODE_FILE, mode.as_str())
        .await
        .context("Failed to write mode file")?;
    
    info!("Deployment mode set to: {}", mode.as_str());
    
    Ok(())
}

// Add a platform detection function
fn is_macos() -> bool {
    // Use synchronous check
    std::env::consts::OS == "macos" || std::env::consts::OS == "darwin"
}

// Generate systemd socket unit for Simple mode
pub async fn generate_systemd_socket_unit(
    service_name: &str,
    description: &str
) -> Result<()> {
    // Create the socket file for socket activation
    let socket_content = format!(
        r#"[Unit]
Description={} Socket
Documentation=https://github.com/your-repo/dragonfly

[Socket]
ListenStream=3000
# Accept=no means we're using socket activation
Accept=no
SocketUser=root
SocketMode=0666

[Install]
WantedBy=sockets.target
"#,
        description
    );

    // Write the socket file
    let socket_file = format!("/etc/systemd/system/{}.socket", service_name);
    tokio::fs::write(&socket_file, socket_content)
        .await
        .context("Failed to write systemd socket file")?;
    
    info!("Generated systemd socket unit file: {}", socket_file);
    
    Ok(())
}

// Generate systemd service unit for Simple mode
pub async fn generate_systemd_unit(
    service_name: &str, 
    exec_path: &str, 
    description: &str
) -> Result<()> {
    // First create the socket unit
    generate_systemd_socket_unit(service_name, description).await?;
    
    // Now create the service file
    let unit_content = format!(
        r#"[Unit]
Description={}
Documentation=https://github.com/your-repo/dragonfly
After=network.target
Requires={}.socket

[Service]
Type=notify
Environment="DRAGONFLY_SERVICE=1"
ExecStart={}
# Don't restart immediately; add a short delay
Restart=on-failure
RestartSec=1
# Make sure the service starts only when the socket is ready
# This ensures proper socket activation
WatchdogSec=10

# Hardening options
ProtectSystem=full
ProtectHome=read-only
PrivateTmp=true
NoNewPrivileges=true

[Install]
WantedBy=multi-user.target
"#,
        description, service_name, exec_path
    );

    // Write the unit file
    let unit_file = format!("/etc/systemd/system/{}.service", service_name);
    tokio::fs::write(&unit_file, unit_content)
        .await
        .context("Failed to write systemd unit file")?;

    // Reload systemd to recognize the new unit
    let output = Command::new("systemctl")
        .arg("daemon-reload")
        .output()
        .context("Failed to reload systemd")?;

    if !output.status.success() {
        warn!("Failed to reload systemd: {}", String::from_utf8_lossy(&output.stderr));
    }

    info!("Generated systemd service unit file: {}", unit_file);
    
    Ok(())
}

// Generate launchd plist for macOS
pub async fn generate_launchd_plist(
    service_name: &str,
    exec_path: &str,
    description: &str
) -> Result<()> {
    // Create a more macOS-friendly service name
    let label = format!("com.dragonfly.{}", service_name);
    
    let plist_content = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{}</string>
    </array>
    <key>EnvironmentVariables</key>
    <dict>
        <key>DRAGONFLY_SERVICE</key>
        <string>1</string>
    </dict>
    <key>Sockets</key>
    <dict>
        <key>Listeners</key>
        <dict>
            <key>SockServiceName</key>
            <string>3000</string>
            <key>SockType</key>
            <string>stream</string>
            <key>SockFamily</key>
            <string>IPv4</string>
        </dict>
    </dict>
    <key>KeepAlive</key>
    <true/>
    <key>RunAtLoad</key>
    <true/>
    <key>StandardErrorPath</key>
    <string>/var/log/dragonfly/dragonfly.log</string>
    <key>StandardOutPath</key>
    <string>/var/log/dragonfly/dragonfly.log</string>
    <key>WorkingDirectory</key>
    <string>/</string>
    <key>ProcessType</key>
    <string>Background</string>
    <key>ThrottleInterval</key>
    <integer>5</integer>
    <key>Description</key>
    <string>{}</string>
    <key>LSUIElement</key>
    <true/>
</dict>
</plist>
"#,
        label, exec_path, description
    );
    
    // Ensure the necessary directories exist
    let home = std::env::var("HOME").context("Failed to get user home directory")?;
    let launch_agents_dir = format!("{}/Library/LaunchAgents", home);
    tokio::fs::create_dir_all(&launch_agents_dir).await.ok();
    
    // Write the plist file
    let plist_path = format!("{}/{}.plist", launch_agents_dir, label);
    tokio::fs::write(&plist_path, plist_content)
        .await
        .context("Failed to write launchd plist file")?;
    
    info!("Generated launchd plist file: {}", plist_path);
    
    Ok(())
}

// Ensure log directory exists with proper permissions
pub fn ensure_log_directory() -> Result<String, anyhow::Error> {
    let log_dir = if cfg!(target_os = "macos") {
        // ~/Library/Logs/Dragonfly
        dirs::home_dir()
            .ok_or_else(|| anyhow!("Could not find home directory"))?
            .join("Library/Logs/Dragonfly")
    } else if cfg!(target_os = "linux") {
        // /var/log/dragonfly
        PathBuf::from("/var/log/dragonfly")
    } else {
        // Default to ~/.dragonfly/logs for other systems
        dirs::home_dir()
            .ok_or_else(|| anyhow!("Could not find home directory"))?
            .join(".dragonfly/logs")
    };

    let log_dir_str = log_dir.to_str()
        .ok_or_else(|| anyhow!("Log directory path is not valid UTF-8"))?
        .to_string();

    if !log_dir.exists() {
        match std::fs::create_dir_all(&log_dir) {
            Ok(_) => {
                info!("Created log directory: {}", log_dir.display());
                #[cfg(target_os = "linux")]
                {
                    if !has_root_privileges() {
                        warn!("Log directory created, but running without root. Cannot set ownership/permissions for /var/log/dragonfly. Logs might not be writable.");
                    } else {
                        let current_uid = unsafe { libc::getuid() };
                        let current_gid = unsafe { libc::getgid() };
                        match nix::unistd::chown(log_dir.as_path(), Some(current_uid.into()), Some(current_gid.into())) {
                            Ok(_) => info!("Set ownership of log directory to current user ({}:{})", current_uid, current_gid),
                            Err(e) => warn!("Failed to set ownership of log directory {}: {}. This might be okay if already owned correctly.", log_dir.display(), e),
                        }
                        match std::fs::set_permissions(&log_dir, std::fs::Permissions::from_mode(0o775)) {
                            Ok(_) => info!("Set permissions of log directory to 775"),
                            Err(e) => warn!("Failed to set permissions for log directory {}: {}", log_dir.display(), e),
                        }
                    }
                }
            }
            Err(e) => {
                if !log_dir.exists() {
                    return Err(anyhow!("Failed to create log directory {}: {}", log_dir.display(), e));
                } else {
                    warn!("Log directory {} already existed or was created concurrently.", log_dir.display());
                }
            }
        }
    }

    Ok(log_dir_str)
}

// Start the service via service manager
#[cfg(unix)]
pub fn start_service() -> Result<()> {
    if is_macos() {
        // For macOS, use launchctl to start the service
        info!("Starting dragonfly launchd service...");
        let service_name = "com.dragonfly.dragonfly";
        
        // First make sure the service is loaded (this will handle socket creation)
        let home = std::env::var("HOME").context("Failed to get user home directory")?;
        let plist_path = format!("{}/Library/LaunchAgents/{}.plist", home, service_name);
        
        // Unload first in case it's already loaded
        let _ = Command::new("launchctl")
            .args(["unload", &plist_path])
            .output();
            
        // Load the service
        let load_output = Command::new("launchctl")
            .args(["load", "-w", &plist_path])
            .output()
            .context("Failed to load launchd service")?;
            
        if !load_output.status.success() {
            let stderr = String::from_utf8_lossy(&load_output.stderr);
            return Err(anyhow!("Failed to load launchd service: {}", stderr));
        }
        
        // Now start the service
        let output = Command::new("launchctl")
            .args(["start", service_name])
            .output()
            .context("Failed to start launchd service")?;
            
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow!("Failed to start launchd service: {}", stderr));
        }
        
        info!("Service started successfully");
    } else {
        // For Linux, use systemctl to start the socket and service
        info!("Starting dragonfly systemd socket and service...");
        
        // Enable and start the socket first
        let socket_enable = Command::new("systemctl")
            .args(["enable", "dragonfly.socket"])
            .output()
            .context("Failed to enable systemd socket")?;
            
        if !socket_enable.status.success() {
            let stderr = String::from_utf8_lossy(&socket_enable.stderr);
            warn!("Failed to enable systemd socket: {}", stderr);
        }
        
        // Enable the service too
        let service_enable = Command::new("systemctl")
            .args(["enable", "dragonfly.service"])
            .output()
            .context("Failed to enable systemd service")?;
            
        if !service_enable.status.success() {
            let stderr = String::from_utf8_lossy(&service_enable.stderr);
            warn!("Failed to enable systemd service: {}", stderr);
        }
        
        // Start the socket first for socket activation
        let socket_output = Command::new("systemctl")
            .args(["start", "dragonfly.socket"])
            .output()
            .context("Failed to start systemd socket")?;
            
        if !socket_output.status.success() {
            let stderr = String::from_utf8_lossy(&socket_output.stderr);
            return Err(anyhow!("Failed to start systemd socket: {}", stderr));
        }
        
        // Start the service
        let output = Command::new("systemctl")
            .args(["start", "dragonfly.service"])
            .output()
            .context("Failed to start systemd service")?;
            
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow!("Failed to start systemd service: {}", stderr));
        }
        
        info!("Socket and service started successfully");
    }
    
    // Exit this process now that the service is started
    info!("Exiting current process as the service is now running in the background");
    std::process::exit(0);
}

// Stub for non-Unix platforms
#[cfg(not(unix))]
pub fn start_service() -> Result<()> {
    warn!("Service management is not supported on this platform");
    Ok(())
}

// Helper function to ensure /var/lib/dragonfly directory exists and is owned by the current user on macOS
async fn ensure_var_lib_ownership() -> Result<()> {
    // Only needed on macOS
    if !is_macos() {
        return Ok(());
    }
    
    let var_lib_dir = PathBuf::from("/var/lib/dragonfly");
    
    // First try to create the directory if it doesn't exist
    if !var_lib_dir.exists() {
        info!("Creating /var/lib/dragonfly directory");
        
        // Try to create with regular permissions first
        if let Err(e) = tokio::fs::create_dir_all(&var_lib_dir).await {
            info!("Failed to create /var/lib/dragonfly directly, using elevated permissions: {}", e);
            
            // Need to use admin privileges
            let user = std::env::var("USER").context("Failed to get current username")?;
            let script = format!(
                r#"do shell script "mkdir -p '{}' && chown '{}' '{}' && chmod 755 '{}'" with administrator privileges with prompt \"Dragonfly needs permission to create data directory\""#,
                var_lib_dir.display(),
                user,
                var_lib_dir.display(),
                var_lib_dir.display()
            );
            
            let osa_output_result = Command::new("osascript")
                .arg("-e")
                .arg(&script)
                .output()
                .context("Failed to execute osascript for sudo prompt");

            // Handle the result of the command execution
            match osa_output_result {
                Ok(osa_output) => {
                    if !osa_output.status.success() {
                        let stderr_str = String::from_utf8_lossy(&osa_output.stderr);
                        warn!("Failed to create and chown /var/lib/dragonfly: {}", stderr_str);
                        // Continue anyway since this is not critical
                    } else {
                        info!("Created and set ownership of /var/lib/dragonfly to user {}", user);
                    }
                }
                Err(e) => {
                    warn!("Error executing osascript for directory creation: {}", e);
                }
            }
        } else {
            // Directory was created, now set ownership
            let user = std::env::var("USER").context("Failed to get current username")?;
            let script = format!(
                r#"do shell script "chown '{}' '{}'" with administrator privileges with prompt \"Dragonfly needs permission to set ownership of data directory\""#,
                user,
                var_lib_dir.display()
            );
            
            let osa_output = Command::new("osascript")
                .arg("-e")
                .arg(&script)
                .output();
            
            if let Ok(output) = osa_output {
                if output.status.success() {
                    info!("Set ownership of /var/lib/dragonfly to user {}", user);
                } else {
                    let stderr_str = String::from_utf8_lossy(&output.stderr);
                    warn!("Failed to set ownership of /var/lib/dragonfly: {}", stderr_str);
                    // Continue anyway since this is not critical
                }
            } else if let Err(e) = osa_output {
                 warn!("Error executing osascript for ownership setting: {}", e);
            }
        }
    } else {
        // Directory already exists, just ensure ownership
        let user = std::env::var("USER").context("Failed to get current username")?;
        
        // Check current ownership
        let stat_output = Command::new("stat")
            .args(["-f", "%Su", var_lib_dir.to_str().unwrap()])
            .output();
            
        if let Ok(output) = stat_output {
            let current_owner = String::from_utf8_lossy(&output.stdout).trim().to_string();
            
            if current_owner != user {
                info!("Changing ownership of /var/lib/dragonfly from {} to {}", current_owner, user);
                
                let script = format!(
                    r#"do shell script "chown -R '{}' '{}'" with administrator privileges with prompt \"Dragonfly needs permission to set ownership of data directory\""#,
                    user,
                    var_lib_dir.display()
                );
                
                let osa_output = Command::new("osascript")
                    .arg("-e")
                    .arg(&script)
                    .output();
                
                if let Ok(output) = osa_output {
                    if output.status.success() {
                        info!("Set ownership of /var/lib/dragonfly to user {}", user);
                    } else {
                        let stderr_str = String::from_utf8_lossy(&output.stderr);
                        warn!("Failed to set ownership of /var/lib/dragonfly: {}", stderr_str);
                        // Continue anyway since this is not critical
                    }
                 } else if let Err(e) = osa_output {
                     warn!("Error executing osascript for ownership change: {}", e);
                 }
            } else {
                info!("/var/lib/dragonfly is already owned by user {}", user);
            }
        } else if let Err(e) = stat_output {
             warn!("Error executing stat command for ownership check: {}", e);
        }
    }
    
    Ok(())
}

// Check if the current process has root privileges
fn has_root_privileges() -> bool {
    #[cfg(unix)]
    {
        // Check if we can access a typically root-only directory
        if let Ok(uid) = std::process::Command::new("id")
            .args(["-u"])
            .output()
        {
            if let Ok(uid_str) = String::from_utf8(uid.stdout) {
                if let Ok(uid_num) = uid_str.trim().parse::<u32>() {
                    return uid_num == 0;
                }
            }
        }
        
        // Fallback to checking if we can write to a protected directory
        std::fs::metadata("/root").is_ok()
    }
    
    #[cfg(not(unix))]
    {
        // On other platforms, always return false
        return false;
    }
}

// Configure the system for Simple mode
pub async fn configure_simple_mode() -> Result<()> {
    info!("Configuring system for Simple mode");
    
    // First, check if we're already in Simple mode to avoid unnecessary work
    if let Ok(Some(current_mode)) = get_current_mode().await {
        if current_mode == DeploymentMode::Simple {
            info!("System is already configured for Simple mode");
            return Ok(());
        }
    }
    
    // Get the path to the current executable
    let current_exec_path = std::env::current_exe()
        .context("Failed to get current executable path")?;
    
    // Initialize logger
    let log_dir_path = ensure_log_directory()?;
    let mut used_elevation = false;
    
    // Copy executable to /usr/local/bin if needed
    let target_path = Path::new(EXECUTABLE_TARGET_PATH);
    if !target_path.exists() {
        info!("Copying executable to {}", EXECUTABLE_TARGET_PATH);
        
        // Check if we're on macOS
        if is_macos() {
            // For macOS, try a normal copy first
            match Command::new("cp")
                .args([&current_exec_path.to_string_lossy(), EXECUTABLE_TARGET_PATH])
                .output()
            {
                Ok(output) if output.status.success() => {
                    info!("Executable copied to {}", EXECUTABLE_TARGET_PATH);
                    
                    // Set executable permissions
                    if let Ok(chmod_output) = Command::new("chmod")
                        .args(["+x", EXECUTABLE_TARGET_PATH])
                        .output()
                    {
                        if !chmod_output.status.success() {
                            warn!("Failed to set executable permissions: {}", 
                                  String::from_utf8_lossy(&chmod_output.stderr));
                        }
                    }
                },
                _ => {
                    info!("Need elevated permissions to copy executable to {}", EXECUTABLE_TARGET_PATH);
                    used_elevation = true;

                    // Need to use sudo, with one command that does everything:
                    // 1. Copy executable
                    // 2. Set executable permissions
                    // 3. Create mode directory and set mode file
                    let source_path = current_exec_path.to_string_lossy().replace("'", "'\\''");
                    
                    // Build a script that does everything we need with a single privilege elevation
                    let script = format!(
                        r#"do shell script "cp '{}' '{}' && chmod +x '{}' && mkdir -p {} && echo {} > {} && chmod 755 {} && mkdir -p '{}' && chmod 755 '{}'" with administrator privileges with prompt \"Dragonfly needs permission to configure Simple mode\""#,
                        source_path,
                        EXECUTABLE_TARGET_PATH,
                        EXECUTABLE_TARGET_PATH,
                        MODE_DIR,
                        DeploymentMode::Simple.as_str(),
                        MODE_FILE,
                        MODE_DIR,
                        log_dir_path,
                        log_dir_path
                    );
                    
                    let osa_output = Command::new("osascript")
                        .arg("-e")
                        .arg(&script)
                        .output()
                        .context("Failed to execute osascript for sudo prompt")?;
                        
                    if !osa_output.status.success() {
                        let stderr = String::from_utf8_lossy(&osa_output.stderr);
                        return Err(anyhow!("Failed to configure with admin privileges: {}", stderr));
                    }
                    
                    info!("System configured with admin privileges");
                }
            }
        } else {
            // For Linux, try with sudo if regular copy fails
            match tokio::fs::copy(&current_exec_path, EXECUTABLE_TARGET_PATH).await {
                Ok(_) => {
                    info!("Executable copied to {}", EXECUTABLE_TARGET_PATH);
                    
                    // Set executable permissions
                    let chmod_output = Command::new("chmod")
                        .args(["+x", EXECUTABLE_TARGET_PATH])
                        .output()
                        .context("Failed to set executable permissions")?;
                        
                    if !chmod_output.status.success() {
                        warn!("Failed to set executable permissions: {}", 
                              String::from_utf8_lossy(&chmod_output.stderr));
                    }
                },
                Err(e) => {
                    info!("Need elevated permissions to copy executable to {}: {}", EXECUTABLE_TARGET_PATH, e);
                    used_elevation = true;

                    // Try with pkexec first (graphical sudo)
                    let pkexec_available = Command::new("which")
                        .arg("pkexec")
                        .output()
                        .map(|output| output.status.success())
                        .unwrap_or(false);
                        
                    if pkexec_available {
                        info!("Trying with pkexec for graphical sudo prompt");
                        
                        // Do everything in one command
                        let script = format!(
                            "pkexec sh -c 'cp \"{}\" \"{}\" && chmod +x \"{}\" && mkdir -p {} && echo {} > {} && chmod 755 {} && mkdir -p {} && chmod 755 {}'",
                            current_exec_path.display(),
                            EXECUTABLE_TARGET_PATH,
                            EXECUTABLE_TARGET_PATH,
                            MODE_DIR,
                            DeploymentMode::Simple.as_str(),
                            MODE_FILE,
                            MODE_DIR,
                            log_dir_path,
                            log_dir_path
                        );
                        
                        let pkexec_output = Command::new("sh")
                            .arg("-c")
                            .arg(&script)
                            .output();
                            
                        match pkexec_output {
                            Ok(output) if output.status.success() => {
                                info!("System configured with pkexec");
                            },
                            _ => {
                                info!("pkexec failed or was cancelled, trying regular sudo");
                                
                                // Try with regular sudo, doing everything in one command
                                let sudo_script = format!(
                                    "sudo sh -c 'cp \"{}\" \"{}\" && chmod +x \"{}\" && mkdir -p {} && echo {} > {} && chmod 755 {} && mkdir -p {} && chmod 755 {}'",
                                    current_exec_path.display(),
                                    EXECUTABLE_TARGET_PATH,
                                    EXECUTABLE_TARGET_PATH,
                                    MODE_DIR,
                                    DeploymentMode::Simple.as_str(),
                                    MODE_FILE,
                                    MODE_DIR,
                                    log_dir_path,
                                    log_dir_path
                                );
                                
                                let sudo_output = Command::new("sh")
                                    .arg("-c")
                                    .arg(&sudo_script)
                                    .output()
                                    .context("Failed to execute sudo command")?;
                                    
                                if !sudo_output.status.success() {
                                    let stderr = String::from_utf8_lossy(&sudo_output.stderr);
                                    return Err(anyhow!("Failed to configure with sudo: {}", stderr));
                                }
                                
                                info!("System configured with sudo");
                            }
                        }
                    } else {
                        // Just use regular sudo
                        let sudo_script = format!(
                            "sudo sh -c 'cp \"{}\" \"{}\" && chmod +x \"{}\" && mkdir -p {} && echo {} > {} && chmod 755 {} && mkdir -p {} && chmod 755 {}'",
                            current_exec_path.display(),
                            EXECUTABLE_TARGET_PATH,
                            EXECUTABLE_TARGET_PATH,
                            MODE_DIR,
                            DeploymentMode::Simple.as_str(),
                            MODE_FILE,
                            MODE_DIR,
                            log_dir_path,
                            log_dir_path
                        );
                        
                        let sudo_output = Command::new("sh")
                            .arg("-c")
                            .arg(&sudo_script)
                            .output()
                            .context("Failed to execute sudo command")?;
                            
                        if !sudo_output.status.success() {
                            let stderr = String::from_utf8_lossy(&sudo_output.stderr);
                            return Err(anyhow!("Failed to configure with sudo: {}", stderr));
                        }
                        
                        info!("System configured with sudo");
                    }
                }
            }
        }
    } else {
        info!("Executable already exists at {}", EXECUTABLE_TARGET_PATH);
    }
    
    // Use the target path for service configuration if it exists, otherwise use current path
    let exec_path = if target_path.exists() {
        target_path.to_path_buf()
    } else {
        current_exec_path
    };
    
    // Create log directory before setting up services if not already handled by elevated commands
    if !used_elevation {
        let log_dir = "/var/log/dragonfly";
        if !std::path::Path::new(log_dir).exists() {
            // Create directory with appropriate permissions
            if let Err(e) = tokio::fs::create_dir_all(log_dir).await {
                warn!("Could not create log directory {}: {}", log_dir, e);
                // Try with sudo if normal creation fails
                if is_macos() {
                     let _ = Command::new("osascript")
                        .arg("-e")
                        .arg(format!(r#"do shell script "mkdir -p '{}' && chmod 755 '{}'" with administrator privileges"#, log_dir, log_dir))
                        .output();
                } else {
                    let _ = Command::new("sudo")
                        .args(["mkdir", "-p", log_dir])
                        .output();
                    let _ = Command::new("sudo")
                        .args(["chmod", "755", log_dir])
                        .output();
                }
            }
        }
        info!("Log directory ready at {}", log_dir);
    }
    
    // Check if we're on macOS
    if is_macos() {
        // Ensure /var/lib/dragonfly exists and is owned by the current user
        if let Err(e) = ensure_var_lib_ownership().await {
            warn!("Failed to ensure /var/lib/dragonfly ownership: {}", e);
            // Non-critical, continue anyway
        }
        
        // Generate the launchd plist for macOS
        info!("Setting up launchd service for macOS");
        generate_launchd_plist(
            "dragonfly", 
            exec_path.to_str().unwrap(), 
            "Dragonfly Simple Mode"
        ).await?;
        
        // Load the service
        let service_name = "com.dragonfly.dragonfly";
        let home = std::env::var("HOME").context("Failed to get user home directory")?;
        let plist_path = format!("{}/Library/LaunchAgents/{}.plist", home, service_name);
        
        // Unload first in case it's already loaded
        let _ = Command::new("launchctl")
            .args(["unload", &plist_path])
            .output();
            
        // Load the service and set to run on login
        let output = Command::new("launchctl")
            .args(["load", "-w", &plist_path])
            .output()
            .context("Failed to load launchd service")?;
            
        if !output.status.success() {
            warn!("Failed to load launchd service: {}", String::from_utf8_lossy(&output.stderr));
        } else {
            info!("Launchd service loaded and set to start on boot");
            
            // The notification shown by macOS about login items
            info!("Dragonfly has been added to Login Items and appears in System Settings > General > Login Items");
        }
        
        // Create a directory for data storage
        let data_dir = PathBuf::from(&home).join(".dragonfly");
        tokio::fs::create_dir_all(&data_dir).await.ok();
    } else {
        // Generate the systemd socket and service units for Linux
        info!("Setting up systemd socket and service for Linux");
        generate_systemd_unit(
            "dragonfly", 
            exec_path.to_str().unwrap(), 
            "Dragonfly Simple Mode"
        ).await?;
        
        // Enable the socket first (for socket activation)
        info!("Enabling systemd socket and service");
        let socket_enable = Command::new("systemctl")
            .args(["enable", "dragonfly.socket"])
            .output()
            .context("Failed to enable dragonfly.socket")?;

        if !socket_enable.status.success() {
            warn!("Failed to enable dragonfly.socket: {}", String::from_utf8_lossy(&socket_enable.stderr));
        } else {
            info!("Systemd socket enabled successfully");
        }
        
        // Enable the service
        let service_enable = Command::new("systemctl")
            .args(["enable", "dragonfly.service"])
            .output()
            .context("Failed to enable dragonfly.service")?;

        if !service_enable.status.success() {
            warn!("Failed to enable dragonfly.service: {}", String::from_utf8_lossy(&service_enable.stderr));
        } else {
            info!("Systemd service enabled successfully");
        }
        
        // Start the socket first (this is important for socket activation)
        let socket_start = Command::new("systemctl")
            .args(["start", "dragonfly.socket"])
            .output()
            .context("Failed to start dragonfly.socket")?;
            
        if !socket_start.status.success() {
            warn!("Failed to start dragonfly.socket: {}", String::from_utf8_lossy(&socket_start.stderr));
        } else {
            info!("Systemd socket started successfully");
        }
        
        // Now start the service
        let service_start = Command::new("systemctl")
            .args(["start", "dragonfly.service"])
            .output()
            .context("Failed to start dragonfly.service")?;
            
        if !service_start.status.success() {
            warn!("Failed to start dragonfly.service: {}", String::from_utf8_lossy(&service_start.stderr));
        } else {
            info!("Systemd service started successfully");
        }
        
        // Create a directory for data storage
        let data_dir = PathBuf::from("/var/lib/dragonfly");
        tokio::fs::create_dir_all(&data_dir).await.ok();
    }
    
    // Save the mode if we haven't already done it with elevated privileges
    if !used_elevation {
        if is_macos() {
            // We've already tried directly, now use osascript
            let script = format!(
                r#"do shell script "mkdir -p {0} && echo '{1}' > {2} && chmod 755 {0}" with administrator privileges with prompt "Dragonfly needs permission to save your deployment mode""#,
                MODE_DIR, DeploymentMode::Simple.as_str(), MODE_FILE
            );
            
            let osa_output = Command::new("osascript")
                .arg("-e")
                .arg(&script)
                .output()
                .context("Failed to execute osascript for sudo prompt")?;
                
            if !osa_output.status.success() {
                let stderr = String::from_utf8_lossy(&osa_output.stderr);
                return Err(anyhow!("Failed to create mode directory with admin privileges: {}", stderr));
            }
            
            info!("Mode set to simple");
        } else {
            // Use the regular save_mode function for Linux
            save_mode(DeploymentMode::Simple, false).await?;
        }
    }
    
    info!("System configured for Simple mode. Dragonfly will run as a service on startup with a status bar icon.");
    info!("Logs will be written to {}/dragonfly.log", log_dir_path);
    info!("Starting service now...");
    
    // Start the service via the service manager (which will exit this process)
    start_service()?;
    
    Ok(())
}

// Start the handoff server for Flight mode
pub async fn start_handoff_listener(mut shutdown_rx: watch::Receiver<()>) -> Result<()> {
    // Set up a signal handler for SIGUSR1
    let mut sigusr1 = signal(SignalKind::user_defined1())
        .context("Failed to install SIGUSR1 handler")?;
    
    let handoff_file = PathBuf::from(HANDOFF_READY_FILE);
    
    info!("Starting handoff listener");
    
    tokio::select! {
        // Wait for the handoff file to be created
        _ = async {
            loop {
                if tokio::fs::metadata(&handoff_file).await.is_ok() {
                    info!("Handoff file detected - initiating handoff");
                    
                    // Read the content to get the pid if available
                    if let Ok(content) = tokio::fs::read_to_string(&handoff_file).await {
                        if let Ok(pid) = content.trim().parse::<i32>() {
                            info!("Sending ACK to k3s pod with pid {}", pid);
                            // Send ACK to the k3s pod if pid is available
                            let _ = Command::new("kill")
                                .args(["-SIGUSR2", &pid.to_string()])
                                .output();
                        }
                    }
                    
                    // Remove the handoff file
                    let _ = tokio::fs::remove_file(&handoff_file).await;
                    
                    break;
                }
                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
            }
        } => {
            info!("Handoff initiated by file - gracefully shutting down");
            return Ok(());
        },
        
        // Wait for SIGUSR1 signal
        _ = sigusr1.recv() => {
            info!("Received SIGUSR1 signal - initiating handoff");
            
            // ACK the signal by writing to a file
            let _ = tokio::fs::write(handoff_file, format!("{}", std::process::id()))
                .await
                .context("Failed to write handoff ACK file");
                
            return Ok(());
        },
        
        // Wait for shutdown signal
        _ = shutdown_rx.changed() => {
            info!("Shutdown received - terminating handoff listener");
            return Ok(());
        }
    }
    
    // Remove the unreachable code
}

// Configure the system for Flight mode
pub async fn configure_flight_mode() -> Result<()> {
    info!("Configuring system for Flight mode");
    
    // First, check if we're already in Flight mode to avoid unnecessary work
    if let Ok(Some(current_mode)) = get_current_mode().await {
        if current_mode == DeploymentMode::Flight {
            info!("System is already configured for Flight mode");
            return Ok(());
        }
    }
    
    // Track if we've used elevated privileges
    let mut used_elevation = false;
    
    // Create the k3s config directory
    if let Err(e) = tokio::fs::create_dir_all(K3S_CONFIG_DIR).await {
        // Try with sudo
        info!("Need elevated permissions to create k3s config directory: {}", e);
        
        if is_macos() {
            // Use osascript for macOS
            let script = format!(
                r#"do shell script "mkdir -p {} && mkdir -p {} && echo {} > {} && chmod 755 {}" with administrator privileges with prompt \"Dragonfly needs permission to configure Flight mode\""#,
                K3S_CONFIG_DIR,
                MODE_DIR,
                DeploymentMode::Flight.as_str(),
                MODE_FILE,
                MODE_DIR
            );
            
            let osa_output = Command::new("osascript")
                .arg("-e")
                .arg(&script)
                .output()
                .context("Failed to execute osascript for sudo prompt")?;
                
            if !osa_output.status.success() {
                let stderr = String::from_utf8_lossy(&osa_output.stderr);
                return Err(anyhow!("Failed to create directories with admin privileges: {}", stderr));
            }
            
            info!("Directories created and mode set with admin privileges");
            used_elevation = true;
        } else {
            // Use sudo for Linux
            let sudo_script = format!(
                "sudo sh -c 'mkdir -p {} && mkdir -p {} && echo {} > {} && chmod 755 {}'",
                K3S_CONFIG_DIR,
                MODE_DIR,
                DeploymentMode::Flight.as_str(),
                MODE_FILE,
                MODE_DIR
            );
            
            let sudo_output = Command::new("sh")
                .arg("-c")
                .arg(&sudo_script)
                .output()
                .context("Failed to execute sudo command")?;
                
            if !sudo_output.status.success() {
                let stderr = String::from_utf8_lossy(&sudo_output.stderr);
                return Err(anyhow!("Failed to create directories with sudo: {}", stderr));
            }
            
            info!("Directories created and mode set with sudo");
            used_elevation = true;
        }
    }
    
    // Ensure /var/lib/dragonfly exists and is owned by the current user on macOS
    if let Err(e) = ensure_var_lib_ownership().await {
        warn!("Failed to ensure /var/lib/dragonfly ownership: {}", e);
        // Non-critical, continue anyway
    }
    
    // Download HookOS artifacts before starting k3s deployment
    info!("Checking HookOS artifacts...");
    tokio::spawn(async {
        // First check if artifacts exist
        use crate::api;
        
        if !api::check_hookos_artifacts().await {
            info!("HookOS artifacts not found. Downloading HookOS artifacts...");
            match api::download_hookos_artifacts("v0.10.0").await {
                Ok(_) => info!("HookOS artifacts downloaded successfully"),
                Err(e) => warn!("Failed to download HookOS artifacts: {}", e),
            }
        } else {
            info!("HookOS artifacts already exist");
        }
    });
    
    // Spawn a process to deploy k3s and initiate handoff
    tokio::spawn(async move {
        info!("Starting background k3s deployment for Flight mode");
        
        let result = deploy_k3s_and_handoff().await;
        
        if let Err(e) = result {
            error!("Failed to deploy k3s: {}", e);
        }
    });
    
    // Save the mode if it hasn't been done already with elevated privileges
    if !used_elevation {
        if is_macos() {
            info!("Saving mode directly using admin privileges on macOS");
            let script = format!(
                r#"do shell script "mkdir -p {0} && echo '{1}' > {2} && chmod 755 {0}" with administrator privileges with prompt "Dragonfly needs permission to save your deployment mode""#,
                MODE_DIR, DeploymentMode::Flight.as_str(), MODE_FILE
            );
            
            let osa_output = Command::new("osascript")
                .arg("-e")
                .arg(&script)
                .output()
                .context("Failed to execute osascript for sudo prompt")?;
                
            if !osa_output.status.success() {
                let stderr = String::from_utf8_lossy(&osa_output.stderr);
                return Err(anyhow!("Failed to create mode directory with admin privileges: {}", stderr));
            }
            
            info!("Mode set to flight");
        } else {
            // Use the regular save_mode function for Linux
            save_mode(DeploymentMode::Flight, false).await?;
        }
    }
    
    // Download HookOS for PXE booting (in background)
    tokio::spawn(async move {
        match crate::api::download_hookos_artifacts("v0.10.0").await {
            Ok(_) => info!("HookOS artifacts downloaded successfully for Flight mode"),
            Err(e) => warn!("Failed to download HookOS artifacts: {}", e),
        }
    });
    
    info!("System configured for Flight mode. K3s deployment started in background.");
    
    // Start the service via service manager instead of daemonizing
    start_service()?;
    
    Ok(())
}

// Deploy k3s and initiate handoff
pub async fn deploy_k3s_and_handoff() -> Result<()> {
    // Get the current process ID for ACK
    let my_pid = std::process::id();
    
    // Check if we're on macOS
    if is_macos() {
        // macOS-specific code for using k3s in Docker
        info!("Running on macOS, setting up k3s in Docker");
        
        // Check if Docker is installed and running
        let docker_running = Command::new("docker")
            .args(["info"])
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false);
            
        if !docker_running {
            return Err(anyhow!("Docker is not installed or not running. Please install and start Docker Desktop."));
        }
        
        // Check if k3s container is already running
        let k3s_exists = Command::new("docker")
            .args(["ps", "-q", "--filter", "name=k3s-server"])
            .output()
            .map(|output| !String::from_utf8_lossy(&output.stdout).trim().is_empty())
            .unwrap_or(false);
        
        if !k3s_exists {
            // Run k3s in Docker
            info!("Starting k3s in Docker");
            let run_output = Command::new("docker")
                .args([
                    "run", "--name", "k3s-server", 
                    "-d", "--privileged",
                    "-p", "6443:6443",       // Kubernetes API
                    "-p", "80:80",           // HTTP
                    "-p", "443:443",         // HTTPS
                    "-p", "8080:8080",       // Tinkerbell Hook service
                    "-p", "69:69/udp",       // TFTP (for PXE)
                    "-p", "53:53/udp",       // DNS
                    "-p", "67:67/udp",       // DHCP
                    "-v", "k3s-server:/var/lib/rancher/k3s",
                    "--restart", "always",
                    "rancher/k3s:latest", "server", "--disable", "traefik"
                ])
                .output()
                .context("Failed to start k3s Docker container")?;
                
            if !run_output.status.success() {
                let stderr = String::from_utf8_lossy(&run_output.stderr);
                return Err(anyhow!("Failed to start k3s in Docker: {}", stderr));
            }
            
            // Wait for k3s to start
            info!("Waiting for k3s container to initialize...");
            tokio::time::sleep(tokio::time::Duration::from_secs(15)).await;
        } else {
            info!("k3s Docker container already exists");
            
            // Check if it's running
            let is_running = Command::new("docker")
                .args(["ps", "-q", "--filter", "name=k3s-server", "--filter", "status=running"])
                .output()
                .map(|output| !String::from_utf8_lossy(&output.stdout).trim().is_empty())
                .unwrap_or(false);
                
            if !is_running {
                // Start the container if it exists but isn't running
                info!("Starting existing k3s container");
                let start_output = Command::new("docker")
                    .args(["start", "k3s-server"])
                    .output()
                    .context("Failed to start existing k3s container")?;
                    
                if !start_output.status.success() {
                    let stderr = String::from_utf8_lossy(&start_output.stderr);
                    return Err(anyhow!("Failed to start existing k3s container: {}", stderr));
                }
                
                // Wait for k3s to start
                tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;
            }
        }
        
        // Copy the kubeconfig from the container
        info!("Extracting kubeconfig from k3s container");
        let home = std::env::var("HOME").context("Failed to get home directory")?;
        let kubeconfig_dir = format!("{}/.kube", home);
        tokio::fs::create_dir_all(&kubeconfig_dir).await.ok(); // Ignore error if dir exists
        
        // First, copy the kubeconfig file
        let copy_cmd = format!(
            "docker cp k3s-server:/etc/rancher/k3s/k3s.yaml {}/.kube/config",
            home
        );

        let copy_output = Command::new("sh")
            .arg("-c")
            .arg(&copy_cmd)
            .output()
            .context("Failed to copy kubeconfig from container")?;
            
        if !copy_output.status.success() {
            let stderr = String::from_utf8_lossy(&copy_output.stderr);
            return Err(anyhow!("Failed to copy kubeconfig from container: {}", stderr));
        }

        // Then, modify the kubeconfig file using sed
        // macOS uses BSD sed which works differently than GNU sed
        let sed_cmd = format!(
            "sed -i '' 's/127.0.0.1/kubernetes.docker.internal/g' {}/.kube/config",
            home
        );

        let sed_output = Command::new("sh")
            .arg("-c")
            .arg(&sed_cmd)
            .output()
            .context("Failed to update kubeconfig server address")?;
            
        if !sed_output.status.success() {
            let stderr = String::from_utf8_lossy(&sed_output.stderr);
            info!("Warning when updating kubeconfig: {}", stderr);
            // This is non-fatal, the user might need to manually edit the file
        }
        
        // Set KUBECONFIG environment variable for kubectl and helm
        let kubeconfig_path = format!("{}/.kube/config", home);
        std::env::set_var("KUBECONFIG", &kubeconfig_path);
        info!("Set KUBECONFIG environment variable to: {}", kubeconfig_path);

        // Add kubernetes.docker.internal to /etc/hosts if not already there
        let hosts_check = Command::new("grep")
            .args(["kubernetes.docker.internal", "/etc/hosts"])
            .output();
            
        if hosts_check.map(|output| !output.status.success()).unwrap_or(true) {
            info!("Adding kubernetes.docker.internal to /etc/hosts");
            let hosts_cmd = "echo '127.0.0.1 kubernetes.docker.internal' | sudo tee -a /etc/hosts";
            let _ = Command::new("sh")
                .arg("-c")
                .arg(hosts_cmd)
                .output();
            // We don't check for errors here because it might require sudo
            // The user can add this manually if needed
        }
        
        // Install Helm if needed
        info!("Installing Helm if needed");
        install_helm().await?;
        
        // Install Tinkerbell stack (would normally happen here)
        info!("Installing Tinkerbell stack");
        // This would call tinkerbell stack installation function
    } else {
        // Linux native k3s installation
        info!("Installing k3s for Flight mode (Linux native)");
        
        // Check if k3s is already installed
        let k3s_installed = Path::new("/etc/rancher/k3s/k3s.yaml").exists() && 
                          check_service_running("k3s").await;
        
        if !k3s_installed {
            // Install k3s
            info!("Installing k3s (single-node)");
            let script = r#"curl -sfL https://get.k3s.io | INSTALL_K3S_EXEC='--disable traefik' sh -"#;
            let output = Command::new("sh")
                .arg("-c")
                .arg(script)
                .output()
                .context("Failed to execute k3s installation script")?;
                
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                return Err(anyhow!("k3s installation failed: {}", stderr));
            }
            
            // Wait for k3s to start
            tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
        } else {
            info!("k3s is already installed, skipping installation");
        }
        
        // Verify k3s is running
        if !check_service_running("k3s").await {
            // Try to restart k3s
            info!("Starting k3s service");
            let restart_output = Command::new("systemctl")
                .args(["restart", "k3s"])
                .output()
                .context("Failed to restart k3s service")?;
                
            if !restart_output.status.success() {
                let stderr = String::from_utf8_lossy(&restart_output.stderr);
                return Err(anyhow!("Failed to restart k3s service: {}", stderr));
            }
            
            // Wait for the service to start
            tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;
            
            // Check again
            if !check_service_running("k3s").await {
                return Err(anyhow!("k3s service failed to start after installation"));
            }
        }
        
        // Configure kubectl
        info!("Configuring kubectl");
        let kubeconfig_path = configure_kubectl().await?;
        
        // Wait for node to be ready
        info!("Waiting for Kubernetes node to become ready");
        wait_for_node_ready(&kubeconfig_path).await?;
        
        // Install helm if needed
        info!("Installing Helm if needed");
        install_helm().await?;
        
        // Install Tinkerbell stack
        info!("Installing Tinkerbell stack");
        // This would normally call the tinkerbell stack installation function
    }
    
    // Write the handoff ready file with our PID
    tokio::fs::write(HANDOFF_READY_FILE, format!("{}", my_pid))
        .await
        .context("Failed to write handoff ready file")?;
    
    info!("K3s deployment completed - handoff ready file created");
    
    // Set up a signal handler for SIGUSR2 (ACK)
    let mut sigusr2 = signal(SignalKind::user_defined2())
        .context("Failed to install SIGUSR2 handler")?;
    
    // Wait for ACK or timeout
    let ack_received = tokio::select! {
        _ = sigusr2.recv() => {
            info!("Received ACK from Rust server - handoff successful");
            true
        },
        _ = tokio::time::sleep(tokio::time::Duration::from_secs(10)) => {
            warn!("No ACK received from Rust server within timeout - continuing anyway");
            false
        }
    };
    
    // If no ACK received, it might mean the Rust server is already terminated
    if !ack_received {
        // Check if the handoff file still exists and remove it
        if Path::new(HANDOFF_READY_FILE).exists() {
            let _ = tokio::fs::remove_file(HANDOFF_READY_FILE).await;
        }
    }
    
    // Start the server in k3s
    info!("Starting Dragonfly server in k3s");
    
    // TODO: Add code to start server in k3s
    
    Ok(())
}

// Helper function to check if a service is running
async fn check_service_running(service_name: &str) -> bool {
    // Check if we're on macOS
    if is_macos() {
        // For macOS, use launchctl
        let service_name = format!("com.dragonfly.{}", service_name);
        let output = Command::new("launchctl")
            .args(["list", &service_name])
            .output();
            
        match output {
            Ok(output) => {
                output.status.success() && !String::from_utf8_lossy(&output.stdout).is_empty()
            },
            Err(_) => false,
        }
    } else {
        // For Linux, use systemctl
        let output = Command::new("systemctl")
            .args(["is-active", service_name])
            .output();
            
        match output {
            Ok(output) => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                stdout.trim() == "active"
            },
            Err(_) => false,
        }
    }
}

// Helper function to configure kubectl
async fn configure_kubectl() -> Result<PathBuf> {
    let source_path = PathBuf::from("/etc/rancher/k3s/k3s.yaml");
    let dest_path = std::env::current_dir()?.join("k3s.yaml");

    // Check if the destination file already exists and is valid
    if dest_path.exists() {
        // Test if the existing config works
        let test_result = Command::new("kubectl")
            .args(["--kubeconfig", dest_path.to_str().unwrap(), "cluster-info"])
            .output();
            
        if let Ok(output) = test_result {
            if output.status.success() {
                return Ok(dest_path);
            }
        }
    }

    // Wait for k3s to create the config file
    let mut attempts = 0;
    while !source_path.exists() && attempts < 12 {
        tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
        attempts += 1;
    }
    
    if !source_path.exists() {
        return Err(anyhow!("k3s config file not found after 60 seconds"));
    }

    // Determine if sudo is needed by checking if we can read the file directly
    // This avoids using libc directly for better musl compatibility
    let needs_sudo = match tokio::fs::metadata(&source_path).await {
        Ok(_) => false, // If we can stat the file, we likely have access
        Err(_) => true,  // If we can't, we likely need sudo
    };

    // Copy the file
    let cp_cmd = format!(
        "{} cp {} {}",
        if needs_sudo { "sudo" } else { "" },
        source_path.display(),
        dest_path.display()
    );
    
    let cp_output = Command::new("sh")
        .arg("-c")
        .arg(cp_cmd.trim())
        .output()
        .context("Failed to copy k3s.yaml")?;
        
    if !cp_output.status.success() {
        return Err(anyhow!("Failed to copy k3s.yaml: {}", 
            String::from_utf8_lossy(&cp_output.stderr)));
    }

    // Get current user for chown
    let user = std::env::var("SUDO_USER") // If run with sudo, chown to the original user
        .or_else(|_| std::env::var("USER")) // Otherwise, use current user
        .context("Could not determine user for chown")?;

    // Change ownership
    let chown_cmd = format!(
        "{} chown {} {}",
        if needs_sudo { "sudo" } else { "" },
        user,
        dest_path.display()
    );
    
    let chown_output = Command::new("sh")
        .arg("-c")
        .arg(chown_cmd.trim())
        .output()
        .context("Failed to chown k3s.yaml")?;
        
    if !chown_output.status.success() {
        return Err(anyhow!("Failed to change ownership of k3s.yaml: {}", 
            String::from_utf8_lossy(&chown_output.stderr)));
    }

    Ok(dest_path)
}

// Helper function to wait for the node to be ready
async fn wait_for_node_ready(kubeconfig_path: &PathBuf) -> Result<()> {
    let max_wait = std::time::Duration::from_secs(300); // 5 minutes timeout
    let start_time = std::time::Instant::now();
    
    let mut node_ready = false;
    let mut coredns_ready = false;

    while start_time.elapsed() < max_wait {
        // Check if the node is ready
        if !node_ready {
            let output_result = Command::new("kubectl")
                .args(["get", "nodes", "--no-headers"])
                .env("KUBECONFIG", kubeconfig_path)
                .output();

            if let Ok(output) = output_result {
                let stdout = String::from_utf8_lossy(&output.stdout);
                
                if output.status.success() && 
                   stdout.contains(" Ready") && 
                   !stdout.contains("NotReady") {
                    info!("Kubernetes node is ready");
                    node_ready = true;
                }
            }
        }

        // Check if CoreDNS is ready
        if node_ready && !coredns_ready {
            let coredns_exists_result = Command::new("kubectl")
                .args(["get", "pods", "-n", "kube-system", "-l", "k8s-app=kube-dns", "--no-headers"])
                .env("KUBECONFIG", kubeconfig_path)
                .output();
                
            if let Ok(output) = &coredns_exists_result {
                if output.status.success() && !String::from_utf8_lossy(&output.stdout).trim().is_empty() {
                    let coredns_status = Command::new("kubectl")
                        .args(["get", "pods", "-n", "kube-system", "-l", "k8s-app=kube-dns", 
                               "-o", "jsonpath='{.items[*].status.conditions[?(@.type==\"Ready\")].status}'"])
                        .env("KUBECONFIG", kubeconfig_path)
                        .output();
                        
                    if let Ok(status) = coredns_status {
                        let status_str = String::from_utf8_lossy(&status.stdout)
                            .trim()
                            .trim_matches('\'')
                            .to_string();
                            
                        if status_str.contains("True") {
                            info!("CoreDNS is ready");
                            coredns_ready = true;
                        }
                    }
                }
            }
        }

        // Exit if both are ready
        if node_ready && coredns_ready {
            return Ok(());
        }

        // Wait before checking again
        tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
    }

    // If we get here, we timed out
    Err(anyhow!("Timed out waiting for Kubernetes node to become ready"))
}

// Helper function to install Helm
async fn install_helm() -> Result<()> {
    // Check if Helm is already installed
    let helm_installed = Command::new("helm")
        .args(["version", "--short"])
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false);
        
    if helm_installed {
        info!("Helm is already installed");
        return Ok(());
    }
    
    info!("Installing Helm");
    let script = r#"curl -sSL https://raw.githubusercontent.com/helm/helm/main/scripts/get-helm-3 | bash"#;
    let output = Command::new("sh")
        .arg("-c")
        .arg(script)
        .output()
        .context("Failed to execute Helm installation script")?;
        
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!("Helm installation failed: {}", stderr));
    }
    
    info!("Helm installed successfully");
    Ok(())
}

// Configure the system for Swarm mode
pub async fn configure_swarm_mode() -> Result<()> {
    info!("Configuring system for Swarm mode");
    
    // First, check if we're already in Swarm mode to avoid unnecessary work
    if let Ok(Some(current_mode)) = get_current_mode().await {
        if current_mode == DeploymentMode::Swarm {
            info!("System is already configured for Swarm mode");
            return Ok(());
        }
    }
    
    // Track if we've used elevated privileges
    let mut used_elevation = false;
    
    // Ensure /var/lib/dragonfly exists and is owned by the current user on macOS
    if let Err(e) = ensure_var_lib_ownership().await {
        warn!("Failed to ensure /var/lib/dragonfly ownership: {}", e);
        // Non-critical, continue anyway
    }
    
    // TODO: Implement swarm mode configuration
    
    // Save the mode if it hasn't been done with elevated privileges
    if !used_elevation {
        if is_macos() {
            info!("Saving mode directly using admin privileges on macOS");
            let script = format!(
                r#"do shell script "mkdir -p {0} && echo '{1}' > {2} && chmod 755 {0}" with administrator privileges with prompt "Dragonfly needs permission to save your deployment mode""#,
                MODE_DIR, DeploymentMode::Swarm.as_str(), MODE_FILE
            );
            
            let osa_output = Command::new("osascript")
                .arg("-e")
                .arg(&script)
                .output()
                .context("Failed to execute osascript for sudo prompt")?;
                
            if !osa_output.status.success() {
                let stderr_str = String::from_utf8_lossy(&osa_output.stderr);
                return Err(anyhow::anyhow!("Failed to create mode directory with admin privileges: {}", stderr_str));
            }
            
            info!("Mode set to swarm");
        } else {
            // Use the regular save_mode function for Linux
            save_mode(DeploymentMode::Swarm, false).await?;
        }
    }
    
    info!("System configured for Swarm mode.");
    
    // Start the service via service manager instead of daemonizing
    start_service()?;
    
    Ok(())
}

fn setup_logging(log_dir: &str) -> Result<(), anyhow::Error> {
    // Combine log directory and file name
    let log_path = Path::new(log_dir).join("dragonfly.log");
    
    // Create a non-blocking writer to the log file
    let file_appender = tracing_appender::rolling::daily(log_dir, "dragonfly.log");
    let (non_blocking_writer, _guard) = tracing_appender::non_blocking(file_appender);

    // Build the subscriber
    tracing_subscriber::registry()
        .with(fmt::layer().with_writer(non_blocking_writer))
        .with(fmt::layer().with_writer(std::io::stdout)) // Also log to stdout
        .with(EnvFilter::from_default_env() // Read RUST_LOG from environment
            .add_directive("info".parse()?) // Default level is info
            .add_directive("tower_http=warn".parse()?) // Quieter HTTP logs
            .add_directive("minijinja=warn".parse()?) // Quieter template logs
        )
        .init();
        
    // Log the path where logs are being written
    info!("Logging initialized. Log file: {}", log_path.display());

    Ok(())
} 