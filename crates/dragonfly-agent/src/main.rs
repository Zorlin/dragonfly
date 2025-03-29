use reqwest::Client;
use anyhow::{Result, Context};
use dragonfly_common::models::{MachineStatus, DiskInfo, Machine, RegisterRequest, RegisterResponse, StatusUpdateRequest, OsInstalledUpdateRequest};
use dragonfly_common::mac_to_words;
use std::env;
use std::fs;
use std::path::Path;
use std::process::Command;
use sysinfo::System;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use clap::Parser;

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Run in setup mode (initial PXE boot)
    #[arg(long)]
    setup: bool,

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
    // Parse command line arguments
    let args = Args::parse();
    
    // Initialize logger
    tracing_subscriber::fmt::init();
    
    // Get API URL from environment, command line, or use default
    let api_url = args.server
        .or_else(|| env::var("DRAGONFLY_API_URL").ok())
        .unwrap_or_else(|| "http://localhost:3000".to_string());
    
    // Create HTTP client
    let client = Client::new();
    
    // Get system information
    let _system = System::new();
    
    // Get MAC address and IP address
    let mac_address = get_mac_address().context("Failed to get MAC address")?;
    let ip_address = get_ip_address().context("Failed to get IP address")?;
    
    // Get hostname
    let hostname = System::host_name().unwrap_or_else(|| "unknown".to_string());
    
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
    let existing_machine = existing_machines.iter().find(|m| m.mac_address == mac_address);
    
    // Process registration/update as before
    let machine_id = match existing_machine {
        Some(machine) => {
            // Machine exists, update its status
            tracing::info!("Machine already exists with ID: {}", machine.id);
            
            // Update machine status with the OS information
            tracing::info!("Updating machine status with OS information...");
            let status_update = StatusUpdateRequest {
                status: current_status,
                message: None,
            };
            
            let status_response = client.post(format!("{}/api/machines/{}/status", api_url, machine.id))
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
                
                let os_installed_response = client.post(format!("{}/api/machines/{}/os_installed", api_url, machine.id))
                    .json(&os_installed_update)
                    .send()
                    .await
                    .context("Failed to send OS installed update")?;
                
                if !os_installed_response.status().is_success() {
                    let error_text = os_installed_response.text().await?;
                    anyhow::bail!("Failed to update OS installed: {}", error_text);
                }
                
                tracing::info!("OS installed updated successfully!");
            }
            
            machine.id
        },
        None => {
            // Machine doesn't exist, register it
            tracing::info!("Machine not found, registering as new...");
            
            // Prepare registration request
            let register_request = RegisterRequest {
                mac_address,
                ip_address,
                hostname: Some(hostname),
                disks,
                nameservers,
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
                status: current_status,
                message: None,
            };
            
            let status_response = client.post(format!("{}/api/machines/{}/status", api_url, register_response.machine_id))
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
                
                let os_installed_response = client.post(format!("{}/api/machines/{}/os_installed", api_url, register_response.machine_id))
                    .json(&os_installed_update)
                    .send()
                    .await
                    .context("Failed to send OS installed update")?;
                
                if !os_installed_response.status().is_success() {
                    let error_text = os_installed_response.text().await?;
                    anyhow::bail!("Failed to update OS installed: {}", error_text);
                }
                
                tracing::info!("OS installed updated successfully!");
            }
            
            register_response.machine_id
        }
    };
    
    // If in setup mode, handle boot decision
    if args.setup {
        if has_bootable_os {
            tracing::info!("Bootable OS detected, attempting to chainload existing OS...");
            
            // Try to chainload the existing OS
            chainload_existing_os()?;
        } else {
            tracing::info!("No bootable OS found, rebooting into Tinkerbell...");
            let mut cmd = Command::new("reboot");
            cmd.status().context("Failed to reboot")?;
        }
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
    if let Some((kernel_path, _)) = &newest_kernel {
        for initrd in initrd_locations.iter() {
            if Path::new(initrd).exists() {
                initrd_path = Some(initrd.to_string());
                break;
            }
        }
    }
    
    if let Some((kernel_path, _)) = newest_kernel {
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
    // Try to use ip command if available
    if let Ok(output) = Command::new("ip")
        .args(["addr", "show"])
        .output()
    {
        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            for line in stdout.lines() {
                if line.contains("inet ") && !line.contains("127.0.0.1") {
                    let parts: Vec<&str> = line.split_whitespace().collect();
                    if parts.len() >= 2 {
                        let addr = parts[1].split('/').next().unwrap_or("").to_string();
                        if !addr.is_empty() {
                            return Ok(addr);
                        }
                    }
                }
            }
        }
    }
    
    // Fallback
    Ok("127.0.0.1".to_string())
} 