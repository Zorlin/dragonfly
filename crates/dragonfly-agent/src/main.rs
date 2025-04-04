use reqwest::Client;
use anyhow::{Result, Context};
use dragonfly_common::models::{MachineStatus, DiskInfo, Machine, RegisterRequest, RegisterResponse, StatusUpdateRequest, OsInstalledUpdateRequest};
use std::env;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use clap::Parser;
use tracing::{info, error, warn};
// Use wildcard import for sysinfo to bring traits into scope
use sysinfo::*;
use serde_json;

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Run in setup mode (initial PXE boot)
    #[arg(long)]
    setup: bool,

    /// Attempt kexec into existing OS when in setup mode (requires --setup)
    #[arg(long, requires = "setup")]
    kexec: bool,

    /// Server URL (default: http://localhost:3000)
    #[arg(long)]
    server: Option<String>,

    /// Tinkerbell IPXE URL (default: http://10.7.1.30:8080/hookos.ipxe)
    #[arg(long, default_value = "http://10.7.1.30:8080/hookos.ipxe")]
    ipxe_url: String,
}

// Enhanced OS detection with support for more distributions
fn detect_os() -> Result<(String, String)> {
    // Try to detect OS using os-release file first (most Linux distributions)
    if let Ok(os_info) = detect_os_from_release_file() {
        return Ok(os_info);
    }
    
    // Try to use lsb_release command (available on many Linux distributions)
    if let Ok(os_info) = detect_os_from_lsb_release() {
        return Ok(os_info);
    }
    
    // Try to detect specific distributions
    if let Ok(os_info) = detect_specific_distros() {
        return Ok(os_info);
    }
    
    // Fallback to sysinfo for generic OS information
    let _system = System::new();
    let os_name = System::name().unwrap_or_else(|| "Unknown".to_string());
    let os_version = System::os_version().unwrap_or_else(|| "Unknown".to_string());
    
    Ok((os_name, os_version))
}

fn detect_os_from_lsb_release() -> Result<(String, String)> {
    if let Ok(output) = Command::new("lsb_release")
        .args(["-a"])
        .output()
    {
        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let mut name = String::new();
            let mut version = String::new();
            
            for line in stdout.lines() {
                if line.starts_with("Distributor ID:") {
                    name = line.trim_start_matches("Distributor ID:").trim().to_string();
                } else if line.starts_with("Release:") {
                    version = line.trim_start_matches("Release:").trim().to_string();
                }
            }
            
            if !name.is_empty() {
                return Ok((name, version));
            }
        }
    }
    
    anyhow::bail!("Could not detect OS using lsb_release")
}

fn detect_specific_distros() -> Result<(String, String)> {
    // Check for Ubuntu first (since it would otherwise be detected as Debian)
    if Path::new("/etc/lsb-release").exists() {
        let lsb_content = fs::read_to_string("/etc/lsb-release")?;
        if lsb_content.contains("Ubuntu") {
            let mut ubuntu_version = String::new();
            // Extract Ubuntu version
            for line in lsb_content.lines() {
                if line.starts_with("DISTRIB_RELEASE=") {
                    ubuntu_version = line.trim_start_matches("DISTRIB_RELEASE=")
                        .trim_matches('"')
                        .to_string();
                    break;
                }
            }
            return Ok(("Ubuntu".to_string(), ubuntu_version));
        }
    }
    
    // Debian specific detection
    if Path::new("/etc/debian_version").exists() {
        let version = fs::read_to_string("/etc/debian_version")?
            .trim()
            .to_string();
        
        return Ok(("Debian".to_string(), version));
    }
    
    // Red Hat based systems
    if Path::new("/etc/redhat-release").exists() {
        let content = fs::read_to_string("/etc/redhat-release")?;
        let content = content.trim();
        
        // Extract name and version using regex-like parsing
        if content.contains("CentOS") {
            // Example: "CentOS Linux release 7.9.2009 (Core)"
            if let Some(version_start) = content.find("release ") {
                let version_str = &content[version_start + 8..];
                if let Some(version_end) = version_str.find(' ') {
                    return Ok(("CentOS".to_string(), version_str[..version_end].to_string()));
                } else {
                    return Ok(("CentOS".to_string(), version_str.to_string()));
                }
            }
            return Ok(("CentOS".to_string(), "Unknown".to_string()));
        } else if content.contains("Red Hat Enterprise Linux") || content.contains("RHEL") {
            // Example: "Red Hat Enterprise Linux release 8.5 (Ootpa)"
            if let Some(version_start) = content.find("release ") {
                let version_str = &content[version_start + 8..];
                if let Some(version_end) = version_str.find(' ') {
                    return Ok(("RHEL".to_string(), version_str[..version_end].to_string()));
                } else {
                    return Ok(("RHEL".to_string(), version_str.to_string()));
                }
            }
            return Ok(("RHEL".to_string(), "Unknown".to_string()));
        } else if content.contains("Fedora") {
            // Example: "Fedora release 35 (Thirty Five)"
            if let Some(version_start) = content.find("release ") {
                let version_str = &content[version_start + 8..];
                if let Some(version_end) = version_str.find(' ') {
                    return Ok(("Fedora".to_string(), version_str[..version_end].to_string()));
                } else {
                    return Ok(("Fedora".to_string(), version_str.to_string()));
                }
            }
            return Ok(("Fedora".to_string(), "Unknown".to_string()));
        }
    }
    
    // SUSE based systems
    if Path::new("/etc/SuSE-release").exists() || Path::new("/etc/SUSE-release").exists() {
        let suse_file = if Path::new("/etc/SuSE-release").exists() {
            "/etc/SuSE-release"
        } else {
            "/etc/SUSE-release"
        };
        
        let content = fs::read_to_string(suse_file)?;
        let first_line = content.lines().next().unwrap_or("");
        
        if first_line.contains("openSUSE") {
            // Extract version
            for line in content.lines() {
                if line.starts_with("VERSION = ") {
                    let version = line.trim_start_matches("VERSION = ").to_string();
                    return Ok(("openSUSE".to_string(), version));
                }
            }
            return Ok(("openSUSE".to_string(), "Unknown".to_string()));
        } else if first_line.contains("SUSE Linux Enterprise") {
            // Extract version
            for line in content.lines() {
                if line.starts_with("VERSION = ") {
                    let version = line.trim_start_matches("VERSION = ").to_string();
                    return Ok(("SLES".to_string(), version));
                }
            }
            return Ok(("SLES".to_string(), "Unknown".to_string()));
        }
    }
    
    // Alpine Linux
    if Path::new("/etc/alpine-release").exists() {
        let version = fs::read_to_string("/etc/alpine-release")?.trim().to_string();
        return Ok(("Alpine".to_string(), version));
    }
    
    // Arch Linux
    if Path::new("/etc/arch-release").exists() {
        return Ok(("Arch Linux".to_string(), "Rolling".to_string()));
    }
    
    anyhow::bail!("Could not detect specific distribution")
}

fn detect_os_from_release_file() -> Result<(String, String)> {
    // Check /etc/os-release first
    let os_release_path = Path::new("/etc/os-release");
    if os_release_path.exists() {
        let content = fs::read_to_string(os_release_path)?;
        return parse_os_release(&content);
    }
    
    // Check /usr/lib/os-release as fallback
    let usr_lib_path = Path::new("/usr/lib/os-release");
    if usr_lib_path.exists() {
        let content = fs::read_to_string(usr_lib_path)?;
        return parse_os_release(&content);
    }
    
    // If we get here, we couldn't find os-release file
    anyhow::bail!("Could not find os-release file")
}

fn parse_os_release(content: &str) -> Result<(String, String)> {
    let mut name = String::new();
    let mut version = String::new();
    
    for line in content.lines() {
        if line.starts_with("NAME=") {
            name = line.trim_start_matches("NAME=")
                .trim_matches('"')
                .to_string();
        } else if line.starts_with("VERSION_ID=") {
            version = line.trim_start_matches("VERSION_ID=")
                .trim_matches('"')
                .to_string();
        }
    }
    
    if name.is_empty() {
        name = "Unknown".to_string();
    }
    
    if version.is_empty() {
        version = "Unknown".to_string();
    }
    
    Ok((name, version))
}

// Detect disks on the system
fn detect_disks() -> Vec<DiskInfo> {
    let mut disks = Vec::new();
    
    // Use lsblk to get disk information
    if let Ok(output) = Command::new("lsblk")
        .args(["-b", "-d", "-n", "-o", "NAME,SIZE,MODEL"])
        .output()
    {
        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            
            for line in stdout.lines() {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 2 {
                    let device_name = parts[0].trim();
                    
                    // Skip loop, ram devices, etc.
                    if device_name.starts_with("loop") || device_name.starts_with("ram") {
                        continue;
                    }
                    
                    let device = format!("/dev/{}", device_name);
                    
                    // Parse size - defaults to 0 if parsing fails
                    let size_bytes = parts[1].parse::<u64>().unwrap_or(0);
                    
                    // Get model if available (parts 2 onwards joined)
                    let model = if parts.len() > 2 {
                        Some(parts[2..].join(" "))
                    } else {
                        None
                    };
                    
                    disks.push(DiskInfo {
                        device,
                        size_bytes,
                        model,
                        calculated_size: None,
                    });
                }
            }
        }
    }
    
    // If lsblk failed, try with fdisk as a fallback
    if disks.is_empty() {
        if let Ok(output) = Command::new("fdisk")
            .args(["-l"])
            .output()
        {
            if output.status.success() {
                let stdout = String::from_utf8_lossy(&output.stdout);
                
                for line in stdout.lines() {
                    if line.contains("Disk /dev/") && !line.contains("loop") && !line.contains("ram") {
                        // Example: "Disk /dev/sda: 20 GiB, 21474836480 bytes, 41943040 sectors"
                        let parts: Vec<&str> = line.split(": ").collect();
                        if parts.len() >= 2 {
                            let device = parts[0].trim_start_matches("Disk ").trim().to_string();
                            
                            // Extract size in bytes if available
                            let size_info = parts[1];
                            let size_bytes = if let Some(start) = size_info.find(", ") {
                                if let Some(end) = size_info[start + 2..].find(" bytes") {
                                    size_info[start + 2..start + 2 + end].replace(",", "").parse::<u64>().unwrap_or(0)
                                } else {
                                    0
                                }
                            } else {
                                0
                            };
                            
                            disks.push(DiskInfo {
                                device,
                                size_bytes,
                                model: None, // fdisk doesn't provide model info
                                calculated_size: None,
                            });
                        }
                    }
                }
            }
        }
    }
    
    tracing::info!("Detected {} disks", disks.len());
    for disk in &disks {
        tracing::info!("  Disk: {} ({} bytes){}", 
            disk.device, 
            disk.size_bytes,
            disk.model.as_ref().map_or("".to_string(), |m| format!(", Model: {}", m)));
    }
    
    disks
}

// Detect nameservers from resolv.conf
fn detect_nameservers() -> Vec<String> {
    let mut nameservers = Vec::new();
    
    // Read resolv.conf to get nameservers
    if let Ok(content) = fs::read_to_string("/etc/resolv.conf") {
        for line in content.lines() {
            if line.starts_with("nameserver") {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 2 {
                    nameservers.push(parts[1].to_string());
                }
            }
        }
    }
    
    // If no nameservers found in resolv.conf, add some sensible defaults
    if nameservers.is_empty() {
        nameservers.push("8.8.8.8".to_string()); // Google DNS
        nameservers.push("1.1.1.1".to_string()); // Cloudflare DNS
    }
    
    tracing::info!("Detected {} nameservers", nameservers.len());
    for ns in &nameservers {
        tracing::info!("  Nameserver: {}", ns);
    }
    
    nameservers
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    
    // Initialize logger
    tracing_subscriber::fmt::init();
    
    // Get API URL from environment, command line, or use default
    let api_url = args.server
        .or_else(|| env::var("DRAGONFLY_API_URL").ok())
        .unwrap_or_else(|| "http://localhost:3000".to_string());

    // --- Get required system info FIRST --- 
    // Get MAC address and IP address (using improved logic)
    let mac_address = get_mac_address().context("Failed to get MAC address")?;
    let ip_address_str = get_ip_address().context("Failed to get IP address")?;
    info!("Agent identified its primary IP as: {}", ip_address_str);

    // Parse the determined IP address for binding
    let local_ip: Option<std::net::IpAddr> = match ip_address_str.parse() {
        Ok(ip) => Some(ip),
        Err(e) => {
            warn!("Failed to parse determined IP address '{}' for binding: {}. Client will use default interface.", ip_address_str, e);
            None
        }
    };
    
    // --- Create HTTP client, binding to the determined IP if possible --- 
    let client_builder = Client::builder();
    let client = match local_ip {
        Some(ip) => {
            info!("Attempting to bind HTTP client to local address: {}", ip);
            client_builder
                .local_address(ip)
                .build()
                .context("Failed to build HTTP client with local address binding")?
        }
        None => {
            info!("Building HTTP client without specific local address binding.");
            client_builder
                .build()
                .context("Failed to build default HTTP client")?
        }
    };
    
    // Get system information (rest of it)
    let mut sys = System::new_all();
    sys.refresh_all();
    
    // Get hostname
    let hostname = System::host_name().unwrap_or_else(|| "unknown".to_string());
    
    // --- Detect CPU, Core Count, and RAM --- 
    // Ensure sysinfo is refreshed first
    sys.refresh_cpu();
    sys.refresh_memory();

    let cpu_model = sys.cpus().first().map(|cpu| cpu.brand().to_string());
    // Prefer physical cores, fallback to logical cores (cpus().len())
    let cpu_cores = sys.physical_core_count().map(|c| c as u32).or_else(|| Some(sys.cpus().len() as u32));
    let total_ram_bytes = sys.total_memory();
    // Convert total RAM to GiB for logging (optional, but often more readable)
    let total_ram_gib = total_ram_bytes as f64 / (1024.0 * 1024.0 * 1024.0);
    
    info!("Detected CPU: {:?}", cpu_model.as_deref().unwrap_or("Unknown"));
    info!("Detected CPU Cores: {:?}", cpu_cores); // Log Option<u32>
    info!("Detected RAM: {} bytes ({:.2} GiB)", total_ram_bytes, total_ram_gib);
    // --- End CPU/RAM Detection ---
    
    // Detect disks and nameservers
    let disks = detect_disks();
    let nameservers = detect_nameservers();
    
    // Detect OS - even in setup mode we want to check for existing OS
    let (os_name, os_version) = detect_os()?;
    tracing::info!("Detected OS: {} {}", os_name, os_version);
    
    // Check if we have a bootable OS
    let has_bootable_os = if args.setup {
        // In setup mode, check if we can find bootable partitions
        let bootable = check_bootable_os()?;
        tracing::info!("Bootable OS check result: {}", bootable);
        bootable
    } else {
        // In normal mode, if we detected a non-Alpine OS, consider it bootable
        os_name != "Alpine" && os_name != "Unknown"
    };
    
    // Determine machine status based on OS detection and setup mode
    let (current_status, os_info) = if has_bootable_os {
        let os_full_name = format!("{} {}", os_name, os_version);
        tracing::info!("Found existing OS: {}", os_full_name);
        (MachineStatus::ExistingOS, Some(os_full_name))
    } else if args.setup {
        tracing::info!("No bootable OS found, marking as ready for adoption");
        (MachineStatus::AwaitingAssignment, None)
    } else {
        tracing::info!("Running in Alpine environment");
        (MachineStatus::AwaitingAssignment, None)
    };
    
    // Check if this machine already exists in the database
    tracing::info!("Checking if machine with MAC {} already exists...", mac_address);
    let existing_machines_response = client.get(format!("{}/api/machines", api_url))
        .send()
        .await
        .context("Failed to fetch existing machines")?;
    
    if !existing_machines_response.status().is_success() {
        let error_text = existing_machines_response.text().await?;
        anyhow::bail!("Failed to fetch existing machines: {}", error_text);
    }
    
    let existing_machines: Vec<Machine> = existing_machines_response.json().await
        .context("Failed to parse existing machines response")?;
    
    // Find if this machine already exists by MAC address
    let existing_machine_option = existing_machines.iter().find(|m| m.mac_address == mac_address).cloned();
    
    // Process registration/update as before
    let _machine_id = match existing_machine_option {
        Some(mut machine) => { // Make machine mutable
            // Machine exists, update its status, OS, and hardware info
            tracing::info!("Machine already exists with ID: {}, fetching current state...", machine.id);

            // Fetch the full machine data first to ensure we have the latest base
            // This is less efficient but safer than assuming the list endpoint has absolutely latest data
            let fetch_url = format!("{}/api/machines/{}", api_url, machine.id);
            match client.get(&fetch_url).send().await {
                Ok(resp) => {
                    if resp.status().is_success() {
                        // The API returns {"machine": ..., "workflow_info": ...}
                        let full_data: serde_json::Value = resp.json().await
                            .context("Failed to parse full machine data JSON")?;
                        if let Some(fetched_machine_json) = full_data.get("machine") {
                             match serde_json::from_value::<Machine>(fetched_machine_json.clone()) {
                                Ok(fetched_machine) => {
                                    machine = fetched_machine; // Replace with the latest fetched data
                                    info!("Successfully fetched latest machine data for ID: {}", machine.id);
                                }
                                Err(e) => {
                                    warn!("Failed to deserialize fetched machine data for {}: {}. Proceeding with list data.", machine.id, e);
                                    // Fallback to using the 'machine' from the list if fetch parsing fails
                                }
                            }
                        } else {
                             warn!("Fetched machine data for {} is missing 'machine' field. Proceeding with list data.", machine.id);
                        }
                    } else {
                        warn!("Failed to fetch full machine data for {}: Status {}. Proceeding with list data.", 
                              machine.id, resp.status());
                        // Fallback to using the 'machine' from the list if fetch fails
                    }
                },
                Err(e) => {
                     warn!("Network error fetching full machine data for {}: {}. Proceeding with list data.", machine.id, e);
                     // Fallback to using the 'machine' from the list if fetch fails
                }
            }
            
            // Update fields on the (potentially refreshed) machine object
            machine.status = current_status; // Set status based on detection
            machine.os_installed = os_info;  // Set os_installed based on detection
            machine.cpu_model = cpu_model.clone();
            machine.cpu_cores = cpu_cores;
            machine.total_ram_bytes = Some(total_ram_bytes);
            // Note: We don't update disks/nameservers here, assuming registration is the source of truth for those
            // updated_at will be set by the server handler
            
            // Send the full updated machine object back to the server
            tracing::info!("Updating existing machine {} with full payload...", machine.id);
            let update_url = format!("{}/api/machines/{}", api_url, machine.id);
            
            // Log the request details before sending
            info!("Attempting to PUT full machine update to URL: {} with payload: {:?}", update_url, machine);

            let update_response = client.put(&update_url)
                .json(&machine) // Send the whole updated machine struct
                .send()
                .await
                .context("Failed to send machine update request")?;

            // Log status and raw response text for debugging
            let status = update_response.status();
            let response_text = match update_response.text().await {
                Ok(text) => text,
                Err(e) => {
                    error!("Failed to read update response text: {}", e);
                    format!("Failed to read response text: {}", e)
                }
            };
            info!("Received response for machine update: Status={}, Body=\"{}\"", status, response_text);

            if !status.is_success() {
                // Use the response text we already read
                error!(
                    "Failed to update machine {}. Status: {}, Response: {}",
                    machine.id,
                    status,
                    response_text
                );
                // Logged the error, but continue agent operation if possible
                // Depending on the error, may want to bail here in some cases?
            } else {
                info!("Successfully updated machine {} on server", machine.id);
            }
            
            // We don't need to update status/os_installed separately anymore
            /*
            // Update machine status with the OS information
            tracing::info!("Updating machine status with OS information...");
            // ... old separate status update code removed ...
            
            // If we detected an OS, also update the os_installed field
            if let Some(os_name) = &os_info {
                tracing::info!("Updating OS installed to: {}", os_name);
                // ... old separate os_installed update code removed ...
            }
            */
            
            machine.id // Return the ID
        },
        None => {
            // Machine doesn't exist, register it
            tracing::info!("Machine not found, registering as new...");
            
            // Prepare registration request
            let register_request = RegisterRequest {
                mac_address,
                ip_address: ip_address_str,
                hostname: Some(hostname),
                disks,
                nameservers,
                // Add the detected hardware info (cloning cpu_model Option)
                cpu_model: cpu_model.clone(), 
                cpu_cores,
                total_ram_bytes: Some(total_ram_bytes),
            };
            
            // Register the machine
            let response = client.post(format!("{}/api/machines", api_url))
                .json(&register_request)
                .send()
                .await
                .context("Failed to send registration request")?;
            
            if !response.status().is_success() {
                let error_text = response.text().await?;
                anyhow::bail!("Failed to register machine: {}", error_text);
            }
            
            let register_response: RegisterResponse = response.json().await
                .context("Failed to parse registration response")?;
            
            tracing::info!("Machine registered successfully!");
            tracing::info!("Machine ID: {}", register_response.machine_id);
            tracing::info!("Next step: {}", register_response.next_step);
            
            // Update machine status with the OS information
            tracing::info!("Updating machine status with OS information...");
            let status_update = StatusUpdateRequest {
                status: MachineStatus::AwaitingAssignment,
                message: None,
            };
            
            let status_response = client.put(format!("{}/api/machines/{}/status", api_url, register_response.machine_id))
                .json(&status_update)
                .send()
                .await
                .context("Failed to send status update")?;
            
            if !status_response.status().is_success() {
                let error_text = status_response.text().await?;
                anyhow::bail!("Failed to update machine status: {}", error_text);
            }
            
            tracing::info!("Machine status updated successfully!");
            
            // If we detected an OS, also update the os_installed field
            if let Some(os_name) = &os_info {
                tracing::info!("Updating OS installed to: {}", os_name);
                let os_installed_update = OsInstalledUpdateRequest {
                    os_installed: os_name.to_string(),
                };
                
                // Construct the URL
                let url = format!(
                    "{}/api/machines/{}/os-installed",
                    api_url,
                    register_response.machine_id
                );

                // Log the request details before sending
                info!("Attempting to PUT OS installed update to URL: {} with payload: {:?}", url, os_installed_update);

                // Send the request and handle potential network/send errors
                let response_result = client
                    .put(&url)
                    .json(&os_installed_update)
                    .send()
                    .await;

                match response_result {
                    Ok(response) => {
                        // Log status and raw response text for debugging
                        let status = response.status();
                        let response_text = match response.text().await {
                            Ok(text) => text,
                            Err(e) => {
                                error!("Failed to read response text: {}", e);
                                format!("Failed to read response text: {}", e)
                            }
                        };
                        info!("Received response for OS installed update: Status={}, Body=\"{}\"", status, response_text);

                        // Check the status code
                        if status.is_success() {
                            info!("Successfully updated OS installed status on server");
                        } else {
                            // Use the response text we already read
                            error!(
                                "Failed to update OS installed. Status: {}, Response: {}",
                                status,
                                response_text
                            );
                             // Logged the error, continue agent operation
                        }
                     },
                    Err(e) => {
                        // Log the specific reqwest error from send()
                        error!("Failed to send OS installed update request: {}", e);
                         // Logged the error, continue agent operation
                    }
                }
            }
            
            register_response.machine_id
        }
    };
    
    // If in setup mode, handle boot decision
    if args.setup {
        if has_bootable_os {
            if args.kexec {
                tracing::info!("--kexec flag provided, attempting to chainload existing OS...");
                // Try to chainload the existing OS
                chainload_existing_os()?;
                // If chainload succeeds, the process is replaced. If it fails, we fall through.
                // Add a log here in case kexec load/exec fails but doesn't return Err?
                tracing::error!("kexec command sequence completed but did not transfer control. This is unexpected.");
                // Still exit cleanly even if kexec didn't work as expected, 
                // as the user explicitly asked for it.
                return Ok(()); 
            } else {
                tracing::info!("Bootable OS detected, but --kexec not specified. Exiting agent cleanly.");
                // Exit cleanly without attempting kexec or reboot
                return Ok(());
            }
        } else {
            tracing::info!("No bootable OS found, attempting reboot into Tinkerbell for OS installation...");
            // Only attempt reboot if no bootable OS is found during setup
            let mut cmd = Command::new("reboot");
            cmd.status().context("Failed to reboot")?;
            // Reboot replaces the current process, so we won't reach here normally.
            // If reboot fails, the context error will propagate.
        }
    } else {
        tracing::info!("Agent finished running in non-setup mode.");
    }
    
    Ok(())
}

/// Check if there's a bootable OS on the system
fn check_bootable_os() -> Result<bool> {
    // First check for EFI boot entries
    if Path::new("/sys/firmware/efi").exists() {
        tracing::info!("Checking EFI boot entries...");
        
        // Use efibootmgr to check boot entries
        if let Ok(output) = Command::new("efibootmgr")
            .output()
        {
            if output.status.success() {
                let stdout = String::from_utf8_lossy(&output.stdout);
                // Look for non-network boot entries
                for line in stdout.lines() {
                    if line.contains("Boot") && !line.contains("Network") {
                        tracing::info!("Found EFI boot entry: {}", line);
                        return Ok(true);
                    }
                }
            }
        }
    }
    
    // Check for GRUB installations
    for path in ["/boot/grub", "/boot/grub2", "/boot/efi/EFI"] {
        if Path::new(path).exists() {
            tracing::info!("Found bootloader directory: {}", path);
            return Ok(true);
        }
    }
    
    // Check for bootable partitions using blkid
    if let Ok(output) = Command::new("blkid")
        .output()
    {
        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            for line in stdout.lines() {
                // Look for common Linux root filesystem types
                if line.contains("TYPE=\"ext4\"") || 
                   line.contains("TYPE=\"xfs\"") || 
                   line.contains("TYPE=\"btrfs\"") {
                    tracing::info!("Found bootable partition: {}", line);
                    return Ok(true);
                }
            }
        }
    }
    
    Ok(false)
}

/// Attempt to chainload the existing OS
fn chainload_existing_os() -> Result<()> {
    // First try to find the kernel in standard locations
    let kernel_locations = [
        "/boot/vmlinuz",
        "/boot/vmlinuz-linux",
        "/boot/vmlinuz-current",
    ];
    
    let initrd_locations = [
        "/boot/initrd.img",
        "/boot/initramfs-linux.img",
        "/boot/initrd-current",
    ];
    
    // Try to find the newest kernel
    let mut newest_kernel: Option<(String, std::fs::Metadata)> = None;
    for kernel in kernel_locations.iter() {
        if let Ok(metadata) = std::fs::metadata(kernel) {
            if metadata.is_file() {
                match &newest_kernel {
                    None => newest_kernel = Some((kernel.to_string(), metadata)),
                    Some((_, old_meta)) => {
                        if let (Ok(old_time), Ok(new_time)) = (old_meta.modified(), metadata.modified()) {
                            if new_time > old_time {
                                newest_kernel = Some((kernel.to_string(), metadata));
                            }
                        }
                    }
                }
            }
        }
    }
    
    // Find matching initrd
    let mut initrd_path = None;
    if let Some((_kernel_path, _)) = &newest_kernel { // Prefix kernel_path with underscore
        for initrd in initrd_locations.iter() {
            if Path::new(initrd).exists() {
                initrd_path = Some(initrd.to_string());
                break;
            }
        }
    }
    
    if let Some((kernel_path, _)) = newest_kernel { // Use original kernel_path here for kexec
        tracing::info!("Found kernel at: {}", kernel_path);
        if let Some(initrd) = &initrd_path {
            tracing::info!("Found initrd at: {}", initrd);
        }
        
        // Build kexec command
        let mut cmd = Command::new("kexec");
        cmd.arg("-l").arg(&kernel_path);
        
        if let Some(initrd) = &initrd_path {
            cmd.args(["--initrd", initrd]);
        }
        
        // Add basic kernel parameters
        cmd.args(["--append", "root=auto rw"]);
        
        // Execute kexec load
        cmd.status().context("Failed to load kernel with kexec")?;
        
        // Execute the loaded kernel
        Command::new("kexec")
            .arg("-e")
            .status()
            .context("Failed to execute IPXE kernel")?;
            
        Ok(())
    } else {
        anyhow::bail!("Could not find bootable kernel")
    }
}

fn get_mac_address() -> Result<String> {
    // First try the ip command
    if let Ok(output) = Command::new("ip")
        .args(["link", "show"])
        .output()
    {
        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            // Skip loopback interfaces
            for line in stdout.lines() {
                if line.contains("link/ether") && !line.contains("lo:") {
                    let parts: Vec<&str> = line.split_whitespace().collect();
                    if parts.len() >= 2 {
                        let mac = parts[1].to_string();
                        tracing::info!("Found actual MAC address: {}", mac);
                        return Ok(mac);
                    }
                }
            }
        }
    }
    
    // Then try with ifconfig
    if let Ok(output) = Command::new("ifconfig")
        .output()
    {
        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            // Skip loopback interfaces
            for line in stdout.lines() {
                if line.contains("ether") && !line.contains("lo:") {
                    let parts: Vec<&str> = line.split_whitespace().collect();
                    if parts.len() >= 2 {
                        let mac = parts[1].to_string();
                        tracing::info!("Found actual MAC address: {}", mac);
                        return Ok(mac);
                    }
                }
            }
        }
    }
    
    // Fallback to looking for network interfaces directly
    let net_dir = Path::new("/sys/class/net");
    if net_dir.exists() {
        for entry in fs::read_dir(net_dir)? {
            let entry = entry?;
            let path = entry.path();
            let if_name = path.file_name().unwrap().to_string_lossy();
            
            // Skip loopback interface
            if if_name == "lo" {
                continue;
            }
            
            let address_path = path.join("address");
            if address_path.exists() {
                if let Ok(mac) = fs::read_to_string(address_path) {
                    let mac = mac.trim().to_string();
                    if !mac.is_empty() && mac != "00:00:00:00:00:00" {
                        tracing::info!("Found actual MAC address: {}", mac);
                        return Ok(mac);
                    }
                }
            }
        }
    }
    
    // Last resort fallback - use a deterministic ID based on hostname
    // This ensures we still get the same ID on subsequent runs
    let hostname = System::host_name().unwrap_or_else(|| "unknown".to_string());
    let mut hasher = DefaultHasher::new();
    hostname.hash(&mut hasher);
    let hash = hasher.finish();
    
    let mac = format!("02:00:00:{:02x}:{:02x}:{:02x}", 
        (hash >> 16) as u8,
        (hash >> 8) as u8,
        hash as u8);
    
    tracing::warn!("Could not detect MAC address, using hostname-based fallback: {}", mac);
    Ok(mac)
}

fn get_ip_address() -> Result<String> {
    // 1. Try to find the IP on the interface used for the default route
    match get_ip_from_default_route_interface() {
        Ok(Some(ip)) => {
            info!("Found IP {} from default route interface", ip);
            return Ok(ip);
        }
        Ok(None) => {
            info!("No default route found or no IP on default interface, scanning all interfaces...");
            // Proceed to scan all interfaces
        }
        Err(e) => {
            warn!("Error checking default route interface: {}. Scanning all interfaces...", e);
            // Proceed to scan all interfaces
        }
    }

    // 2. Fallback: Scan all interfaces if default route method failed or yielded no IP
    info!("Scanning all interfaces for a suitable IP address...");
    let output = Command::new("ip")
        .args(["-4", "addr", "show"])
        .output()
        .context("Failed to run 'ip addr show'")?;

    if !output.status.success() {
        anyhow::bail!("'ip addr show' command failed");
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut current_interface: Option<String> = None;
    let mut candidates: Vec<(String, String)> = Vec::new(); // (interface_name, ip_address)
    let bad_prefixes = ["docker", "virbr", "veth", "cni", "flannel"];
    let bad_masters = ["cni0", "docker0"]; // Add known bad master interfaces
    let preferred_prefixes = ["eth", "en", "wl"]; // Common physical/wifi prefixes

    for line in stdout.lines() {
        // Check for start of a new interface block (e.g., "2: eth0: <...")
        if let Some(colon_pos) = line.find(':') {
            if line[..colon_pos].chars().all(|c| c.is_digit(10)) {
                // Looks like an interface index line
                // Log the raw interface line *before* filtering
                tracing::debug!("Processing interface line: {}", line.trim());

                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() > 1 {
                    let if_name = parts[1].trim_end_matches(':').to_string();
                    let is_lo = if_name == "lo";
                    let is_up = line.contains("<UP,") || line.contains(",UP>");
                    let has_bad_prefix = bad_prefixes.iter().any(|prefix| if_name.starts_with(prefix));
                    // Check if the interface is attached to a known bad master
                    let is_attached_to_bad_master = bad_masters.iter().any(|master| line.contains(&format!(" master {}", master)));

                    // Determine if this interface should be considered
                    if is_lo || !is_up || has_bad_prefix || is_attached_to_bad_master {
                        if is_attached_to_bad_master {
                            tracing::debug!("Ignoring interface {} because it is attached to a bad master", if_name);
                        } else if has_bad_prefix {
                             tracing::debug!("Ignoring interface {} because it has a bad prefix", if_name);
                        } // Add other debug logs if needed
                        current_interface = None; // Skip this interface block
                    } else {
                        current_interface = Some(if_name); // Good candidate interface
                    }
                } else {
                     current_interface = None; // Malformed line?
                }
                continue; // Move to the next line after processing interface header
            }
        }

        // Check for inet line within an active, considered interface block
        if let Some(ref if_name) = current_interface {
            if line.trim().starts_with("inet ") {
                let parts: Vec<&str> = line.trim().split_whitespace().collect();
                if parts.len() >= 2 {
                    if let Some(ip) = parts[1].split('/').next() {
                        // Basic validation and filtering for the IP address itself
                        // Interface checks (prefix, master, up state) were already done when setting current_interface
                        if !ip.starts_with("127.") && !ip.starts_with("169.254.") {
                            // No need to re-check bad_prefixes on if_name here
                            candidates.push((if_name.clone(), ip.to_string()));
                            tracing::debug!("Found candidate IP {} on interface {}", ip, if_name);
                        }
                    }
                }
            }
        }
    }

    // Log all candidates found before prioritization
    if candidates.is_empty() {
        warn!("No suitable IP address candidates found after filtering scanning all interfaces.");
    } else {
        info!("Found {} IP address candidates from scanning all interfaces:", candidates.len());
        for (if_name, ip) in &candidates {
            info!("  - Interface: {}, IP: {}", if_name, ip);
        }
    }

    // Prioritize candidates based on preferred interface prefixes
    if let Some((if_name, ip)) = candidates.iter().find(|(name, _)| preferred_prefixes.iter().any(|p| name.starts_with(p))) {
        info!("Selected preferred IP {} from interface {} based on prefix matching (fallback scan).", ip, if_name);
        return Ok(ip.clone());
    }

    // If no preferred interface found, return the first valid candidate
    if let Some((if_name, ip)) = candidates.first() {
        info!("Selected first available IP {} from interface {} (fallback scan, no preferred prefix match).", ip, if_name);
        return Ok(ip.clone());
    }

    // If no suitable IP found after filtering
    warn!("Could not find any suitable IP address. Falling back to 127.0.0.1");
    Ok("127.0.0.1".to_string())
}

// Helper function to get IP from the default route interface
fn get_ip_from_default_route_interface() -> Result<Option<String>> {
    let output = Command::new("ip")
        .args(["-4", "route", "show", "default"])
        .output()
        .context("Failed to run 'ip route show default'")?;

    if !output.status.success() {
        // Command might fail if there is no default route, which is not an error in itself
        info!("'ip route show default' command failed or no default route set.");
        return Ok(None);
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let default_route_line = stdout.lines().next(); // Default route should be the first line

    if let Some(line) = default_route_line {
        info!("Default route line: {}", line);
        let parts: Vec<&str> = line.split_whitespace().collect();
        if let Some(dev_index) = parts.iter().position(|&p| p == "dev") {
            if let Some(if_name) = parts.get(dev_index + 1) {
                info!("Found default route interface: {}", if_name);
                // Now get the IP address for this specific interface
                let addr_output = Command::new("ip")
                    .args(["-4", "addr", "show", "dev", if_name])
                    .output()
                    .context(format!("Failed to run 'ip addr show dev {}'", if_name))?;

                if !addr_output.status.success() {
                    warn!("Failed to get address for default interface {}. Status: {}", if_name, addr_output.status);
                    return Ok(None);
                }

                let addr_stdout = String::from_utf8_lossy(&addr_output.stdout);
                for addr_line in addr_stdout.lines() {
                    if addr_line.trim().starts_with("inet ") {
                        let addr_parts: Vec<&str> = addr_line.trim().split_whitespace().collect();
                        if addr_parts.len() >= 2 {
                            if let Some(ip) = addr_parts[1].split('/').next() {
                                if !ip.starts_with("127.") && !ip.starts_with("169.254.") {
                                    info!("Found valid IP {} on default interface {}", ip, if_name);
                                    return Ok(Some(ip.to_string()));
                                }
                            }
                        }
                    }
                }
                warn!("No valid inet address found on default interface {}", if_name);
                return Ok(None); // Found interface but no suitable IP
            } else {
                warn!("Could not parse interface name after 'dev' in default route line");
            }
        } else {
            warn!("Could not find 'dev' keyword in default route line");
        }
    } else {
        info!("No output from 'ip route show default' (no default route?)");
    }

    Ok(None) // No default route found or couldn't parse it
} 
