use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use serde::{Deserialize, Serialize};
use tracing::{error, info, warn};
use std::net::IpAddr;
use uuid::Uuid;

use crate::AppState;

// Standard reqwest for HTTP requests to Proxmox
use reqwest::{Client as ReqwestClient, header::HeaderMap};
// Import the required crates 
use netscan;
use netdev;

// ========================
// Structs
// ========================

#[derive(Deserialize, Debug)]
pub struct ProxmoxConnectRequest {
    host: String,
    #[serde(default = "default_port")]
    port: u16,
    username: String,
    password: String, // Could be password or API token value
    #[serde(default = "default_vm_selection")]
    vm_selection_option: VmSelectionOption,
    // TODO: Add field for tags if vm_selection_option is Tags
}

fn default_port() -> u16 {
    8006
}

#[derive(Deserialize, Serialize, Debug, PartialEq, Default)]
#[serde(rename_all = "lowercase")]
enum VmSelectionOption {
    #[default]
    All,
    None,
    Tags,
}

fn default_vm_selection() -> VmSelectionOption {
    VmSelectionOption::All
}

#[derive(Serialize, Debug)]
pub struct ProxmoxConnectResponse {
    message: String,
    added_vms: usize,
    machines: Vec<DiscoveredProxmox>,
}

#[derive(Serialize, Debug)]
struct ErrorResponse {
    error: String,
}

#[derive(Serialize, Debug)]
pub struct ProxmoxDiscoverResponse {
    machines: Vec<DiscoveredProxmox>,
}

#[derive(Serialize, Debug, Clone)]
pub struct DiscoveredProxmox {
    host: String,
    port: u16,
    hostname: Option<String>,
    mac_address: Option<String>,
    machine_type: String,
    vmid: Option<u32>,
    parent_host: Option<String>,
}

// Node and VM information structures
#[derive(Deserialize, Debug)]
struct Node {
    node: String,
    status: String,
}

#[derive(Deserialize, Debug)]
struct VmInfo {
    vmid: u32,
    name: Option<String>,
    status: String,
}

// New struct for VM details including MAC addresses
#[derive(Deserialize, Debug)]
struct VmConfig {
    #[serde(default)]
    net0: Option<String>, // Network interface config containing MAC
    #[serde(default)]
    net1: Option<String>,
    #[serde(default)]
    net2: Option<String>,
    #[serde(default)]
    net3: Option<String>,
}

// ========================
// Handler
// ========================

pub async fn connect_proxmox_handler(
    State(app_state): State<AppState>,
    Json(payload): Json<ProxmoxConnectRequest>,
) -> impl IntoResponse {
    info!(
        host = %payload.host,
        port = payload.port,
        user = %payload.username,
        options = ?payload.vm_selection_option,
        "Received Proxmox connect request"
    );

    // Build reqwest client with TLS skip verification for internal usage
    let client = match ReqwestClient::builder()
        .danger_accept_invalid_certs(true)
        .build() {
        Ok(c) => c,
        Err(e) => {
            error!("Failed to create HTTP client: {}", e);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: format!("Failed to create HTTP client: {}", e),
                }),
            )
                .into_response();
        }
    };

    // Proxmox API base URL
    let base_url = format!("https://{}:{}/api2/json", payload.host, payload.port);
    
    // First, try to authenticate and get a ticket
    let auth_url = format!("{}/access/ticket", base_url);
    let auth_params = [
        ("username", payload.username.as_str()),
        ("password", payload.password.as_str()),
    ];

    // Get auth ticket
    let auth_response = match client.post(&auth_url).form(&auth_params).send().await {
        Ok(resp) => resp,
        Err(e) => {
            error!("Failed to connect to Proxmox: {}", e);
            return (
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse {
                    error: format!("Failed to connect to Proxmox: {}", e),
                }),
            )
                .into_response();
        }
    };

    if !auth_response.status().is_success() {
        let status = if auth_response.status() == reqwest::StatusCode::UNAUTHORIZED {
            StatusCode::UNAUTHORIZED
        } else {
            StatusCode::INTERNAL_SERVER_ERROR
        };
        
        let error_text = match auth_response.text().await {
            Ok(text) => text,
            Err(_) => "Unknown error".to_string(),
        };
        
        error!("Proxmox authentication failed: {}", error_text);
        return (
            status,
            Json(ErrorResponse {
                error: format!("Proxmox authentication failed: {}", error_text),
            }),
        )
            .into_response();
    }

    // Parse auth response
    let auth_data: serde_json::Value = match auth_response.json().await {
        Ok(data) => data,
        Err(e) => {
            error!("Failed to parse Proxmox auth response: {}", e);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: format!("Failed to parse Proxmox auth response: {}", e),
                }),
            )
                .into_response();
        }
    };

    // Extract ticket and CSRF token
    let ticket = match auth_data["data"]["ticket"].as_str() {
        Some(t) => t,
        None => {
            error!("No ticket in Proxmox auth response");
            return (
                StatusCode::INTERNAL_SERVER_ERROR, 
                Json(ErrorResponse { 
                    error: "Failed to get authentication ticket".to_string() 
                })
            ).into_response();
        }
    };

    let csrf_token = match auth_data["data"]["CSRFPreventionToken"].as_str() {
        Some(t) => t,
        None => {
            error!("No CSRF token in Proxmox auth response");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: "Failed to get CSRF token".to_string(),
                }),
            )
                .into_response();
        }
    };

    // Create headers for subsequent requests
    let mut headers = HeaderMap::new();
    headers.insert("Cookie", format!("PVEAuthCookie={}", ticket).parse().unwrap());
    headers.insert("CSRFPreventionToken", csrf_token.parse().unwrap());

    // Get nodes
    let nodes_url = format!("{}/nodes", base_url);
    let nodes_response = match client.get(&nodes_url).headers(headers.clone()).send().await {
        Ok(resp) => resp,
        Err(e) => {
            error!("Failed to fetch Proxmox nodes: {}", e);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: format!("Failed to fetch Proxmox nodes: {}", e),
                }),
            )
                .into_response();
        }
    };

    let nodes_data: serde_json::Value = match nodes_response.json().await {
        Ok(data) => data,
        Err(e) => {
            error!("Failed to parse Proxmox nodes response: {}", e);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: format!("Failed to parse Proxmox nodes response: {}", e),
                }),
            )
                .into_response();
        }
    };

    // Parse node data
    let nodes: Vec<Node> = match serde_json::from_value(nodes_data["data"].clone()) {
        Ok(n) => n,
        Err(e) => {
            error!("Failed to parse Proxmox node data: {}", e);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: format!("Failed to parse Proxmox node data: {}", e),
                }),
            )
                .into_response();
        }
    };

    info!("Successfully connected to Proxmox host: {}. Found {} nodes.", payload.host, nodes.len());

    let mut added_vms_count = 0;
    let mut vm_errors = Vec::new();
    let mut all_machines: Vec<DiscoveredProxmox> = Vec::new();
    
    // First, add the Proxmox host itself
    all_machines.push(DiscoveredProxmox {
        host: payload.host.clone(),
        port: payload.port,
        hostname: None, // We don't know the hostname yet
        mac_address: None, // We don't know the MAC address yet
        machine_type: "proxmox-host".to_string(),
        vmid: None,
        parent_host: None,
    });
    
    // Add the host to the database
    info!("Adding Proxmox host {} to machines", payload.host);
    // Create a unique deterministic machine ID based on host address
    let host_machine_id = Uuid::new_v5(&Uuid::NAMESPACE_URL, format!("proxmox:{}", payload.host).as_bytes());
    // Create a register request for the host
    let host_register_req = dragonfly_common::models::RegisterRequest {
        // Generate a deterministic MAC address for the host based on its address
        mac_address: format!("22:22:22:{:02x}:{:02x}:{:02x}", 
            payload.host.as_bytes().iter().fold(0, |acc, &x| acc ^ x),
            (payload.host.len() % 256) as u8,
            (payload.port % 256) as u8),
        ip_address: payload.host.clone(),
        hostname: Some(payload.host.clone()),
        disks: Vec::new(),
        nameservers: Vec::new(),
        cpu_model: None,
        cpu_cores: None,
        total_ram_bytes: None,
        proxmox_vmid: None,
        proxmox_node: None,
    };
    
    // Check if host already exists by ID first
    match crate::db::get_machine_by_id(&host_machine_id).await {
        Ok(Some(_)) => {
            info!("Proxmox host {} already exists in database", payload.host);
            // Host already exists, no need to add it again
        },
        _ => {
            // Host doesn't exist, register it
            match crate::db::register_machine(&host_register_req).await {
                Ok(_) => {
                    info!("Successfully registered Proxmox host {}", payload.host);
                    added_vms_count += 1;
                },
                Err(e) => {
                    error!("Failed to register Proxmox host {}: {}", payload.host, e);
                    vm_errors.push(format!("Failed to register host {}: {}", payload.host, e));
                }
            }
        }
    }

    if payload.vm_selection_option == VmSelectionOption::All {
        // Fetch VMs from all nodes
        for node in nodes {
            let node_name = &node.node;
            info!("Fetching VMs from node: {}", node_name);
            
            // Add the Proxmox node to the list
            all_machines.push(DiscoveredProxmox {
                host: format!("{}.{}", node_name, payload.host.clone()), // Construct node FQDN
                port: payload.port,
                hostname: Some(node_name.clone()),
                mac_address: None, // We don't know the MAC address of the node
                machine_type: "proxmox-node".to_string(),
                vmid: None,
                parent_host: Some(payload.host.clone()),
            });

            // Add node to the database
            info!("Adding Proxmox node {} to machines", node_name);
            let node_fqdn = format!("{}.{}", node_name, payload.host);
            // Create a unique deterministic machine ID based on node address
            let node_machine_id = Uuid::new_v5(&Uuid::NAMESPACE_URL, format!("proxmox:node:{}", node_fqdn).as_bytes());
            
            // Create a register request for the node
            let node_register_req = dragonfly_common::models::RegisterRequest {
                // Generate a deterministic MAC address for the node based on its name
                mac_address: format!("22:22:33:{:02x}:{:02x}:{:02x}", 
                    node_name.as_bytes().iter().fold(0, |acc, &x| acc ^ x),
                    (node_name.len() % 256) as u8,
                    (payload.port % 256) as u8),
                ip_address: node_fqdn.clone(), // Use FQDN as IP for now
                hostname: Some(node_name.clone()),
                disks: Vec::new(),
                nameservers: Vec::new(),
                cpu_model: None,
                cpu_cores: None,
                total_ram_bytes: None,
                proxmox_vmid: None,
                proxmox_node: Some(node_name.clone()),
            };
            
            // Check if node already exists by ID first
            match crate::db::get_machine_by_id(&node_machine_id).await {
                Ok(Some(_)) => {
                    info!("Proxmox node {} already exists in database", node_name);
                    // Node already exists, no need to add it again
                },
                _ => {
                    // Node doesn't exist, register it
                    match crate::db::register_machine(&node_register_req).await {
                        Ok(_) => {
                            info!("Successfully registered Proxmox node {}", node_name);
                            added_vms_count += 1;
                        },
                        Err(e) => {
                            error!("Failed to register Proxmox node {}: {}", node_name, e);
                            vm_errors.push(format!("Failed to register node {}: {}", node_name, e));
                        }
                    }
                }
            }

            let vms_url = format!("{}/nodes/{}/qemu", base_url, node_name);
            let vms_response = match client.get(&vms_url).headers(headers.clone()).send().await {
                Ok(resp) => resp,
                Err(e) => {
                    let error_msg = format!("Failed to fetch VMs from node {}: {}", node_name, e);
                    error!("{}", error_msg);
                    vm_errors.push(error_msg);
                    continue;
                }
            };

            let vms_data: serde_json::Value = match vms_response.json().await {
                Ok(data) => data,
                Err(e) => {
                    let error_msg = format!("Failed to parse VMs from node {}: {}", node_name, e);
                    error!("{}", error_msg);
                    vm_errors.push(error_msg);
                    continue;
                }
            };

            // Parse VM data
            let vms: Vec<VmInfo> = match serde_json::from_value(vms_data["data"].clone()) {
                Ok(v) => v,
                Err(e) => {
                    let error_msg = format!("Failed to parse VM data from node {}: {}", node_name, e);
                    error!("{}", error_msg);
                    vm_errors.push(error_msg);
                    continue;
                }
            };

            info!("Found {} VMs on node {}", vms.len(), node_name);
            for vm in vms {
                let vmid = vm.vmid;
                let vm_name = vm.name.unwrap_or_else(|| format!("vm-{}", vmid));
                
                // Get VM config to find MAC address
                let config_url = format!("{}/nodes/{}/qemu/{}/config", base_url, node_name, vmid);
                let config_response = match client.get(&config_url).headers(headers.clone()).send().await {
                    Ok(resp) => resp,
                    Err(e) => {
                        warn!("Failed to fetch VM {} config: {}", vmid, e);
                        continue;
                    }
                };
                
                if !config_response.status().is_success() {
                    warn!("Failed to fetch VM {} config, status: {}", vmid, config_response.status());
                    continue;
                }
                
                let config_data: serde_json::Value = match config_response.json().await {
                    Ok(data) => data,
                    Err(e) => {
                        warn!("Failed to parse VM {} config: {}", vmid, e);
                        continue;
                    }
                };
                
                // Parse VM config data
                let vm_config: VmConfig = match serde_json::from_value(config_data["data"].clone()) {
                    Ok(c) => c,
                    Err(e) => {
                        warn!("Failed to parse VM {} config data: {}", vmid, e);
                        continue;
                    }
                };
                
                // Extract MAC address from network device config
                let mut mac_address = None;
                for net_config in [&vm_config.net0, &vm_config.net1, &vm_config.net2, &vm_config.net3].iter().filter_map(|&x| x.as_ref()) {
                    // Network config format is typically: "model=virtio,bridge=vmbr0,macaddr=XX:XX:XX:XX:XX:XX"
                    if let Some(mac_pos) = net_config.find("macaddr=") {
                        let mac_start = mac_pos + 8; // Length of "macaddr="
                        if let Some(mac_end) = net_config[mac_start..].find(',') {
                            mac_address = Some(net_config[mac_start..mac_start + mac_end].to_string());
                        } else {
                            // If no comma after MAC, it might be the last parameter
                            mac_address = Some(net_config[mac_start..].to_string());
                        }
                        break; // Use the first MAC address found
                    }
                }
                
                if let Some(mac) = &mac_address {
                    info!("Found MAC address {} for VM {} ({})", mac, vmid, vm_name);
                }
                
                // Add this VM to the list of machines
                let vm_machine = DiscoveredProxmox {
                    host: format!("{}.{}", vm_name, payload.host.clone()), // Construct VM FQDN
                    port: 0, // VMs don't have a port for Proxmox API
                    hostname: Some(vm_name.clone()),
                    mac_address,
                    machine_type: "proxmox-vm".to_string(),
                    vmid: Some(vmid),
                    parent_host: Some(format!("{}.{}", node_name, payload.host.clone())), // Parent is the node
                };
                
                all_machines.push(vm_machine.clone());
                
                // Create a unique deterministic ID for this VM
                let vm_id = Uuid::new_v5(&Uuid::NAMESPACE_URL, format!("proxmox:vm:{}:{}", node_name, vmid).as_bytes());
                
                // Generate a MAC if none exists
                let generated_mac = if let Some(ref mac) = vm_machine.mac_address {
                    mac.clone()
                } else {
                    // Generate a deterministic MAC based on VMID and node
                    format!("22:22:44:{:02x}:{:02x}:{:02x}", 
                        (vmid & 0xFF) as u8,
                        ((vmid >> 8) & 0xFF) as u8,
                        node_name.as_bytes().iter().fold(0, |acc, &x| acc ^ x))
                };
                
                // Check if machine already exists by ID
                match crate::db::get_machine_by_id(&vm_id).await {
                    Ok(Some(existing_machine)) => {
                        // Machine exists, update its Proxmox info
                        info!("Updating existing machine {} (by ID) with Proxmox VMID {}", existing_machine.id, vmid);
                        let mut machine = existing_machine.clone();
                        machine.proxmox_vmid = Some(vmid);
                        machine.proxmox_node = Some(node_name.clone());
                        if machine.hostname.is_none() {
                            machine.hostname = Some(vm_name.clone());
                        }
                        match crate::db::update_machine(&machine).await {
                            Ok(_) => added_vms_count += 1,
                            Err(e) => {
                                error!("Failed to update machine {}: {}", machine.id, e);
                                vm_errors.push(format!("Failed to update VM {}: {}", vm_name, e));
                            }
                        }
                    },
                    _ => {
                        // Check if machine already exists by MAC address (if we have one)
                        if let Some(ref mac) = vm_machine.mac_address {
                            match crate::db::get_machine_by_mac(mac).await {
                                Ok(Some(existing_machine)) => {
                                    // Machine exists, update its Proxmox info
                                    info!("Updating existing machine {} (by MAC) with Proxmox VMID {}", existing_machine.id, vmid);
                                    let mut machine = existing_machine.clone();
                                    machine.proxmox_vmid = Some(vmid);
                                    machine.proxmox_node = Some(node_name.clone());
                                    if machine.hostname.is_none() {
                                        machine.hostname = Some(vm_name.clone());
                                    }
                                    match crate::db::update_machine(&machine).await {
                                        Ok(_) => added_vms_count += 1,
                                        Err(e) => {
                                            error!("Failed to update machine {}: {}", machine.id, e);
                                            vm_errors.push(format!("Failed to update VM {}: {}", vm_name, e));
                                        }
                                    }
                                },
                                _ => {
                                    // Machine doesn't exist by MAC either, register a new one
                                    register_new_vm(&vm_name, &generated_mac, vmid, node_name, &payload.host, &mut added_vms_count, &mut vm_errors).await;
                                }
                            }
                        } else {
                            // No MAC address, just register as new VM
                            register_new_vm(&vm_name, &generated_mac, vmid, node_name, &payload.host, &mut added_vms_count, &mut vm_errors).await;
                        }
                    }
                }
            }
        }
    } else if payload.vm_selection_option == VmSelectionOption::Tags {
        // TODO: Implement tag-based filtering (requires fetching VM configs)
        vm_errors.push("VM selection by tags is not yet implemented.".to_string());
    }
    // If VmSelectionOption::None, added_vms_count remains 0

    if !vm_errors.is_empty() {
        // Return partial success with errors
        warn!("Completed Proxmox connection with errors: {:?}", vm_errors);
        return (
            StatusCode::OK, // Still OK, but include errors in message
            Json(ProxmoxConnectResponse {
                message: format!(
                    "Connected to {}, added {} VMs with errors: {}",
                    payload.host,
                    added_vms_count,
                    vm_errors.join("; ")
                ),
                added_vms: added_vms_count,
                machines: all_machines,
            }),
        )
            .into_response();
    }

    info!(
        "Proxmox connection successful for {}. Added {} VMs.",
        payload.host, added_vms_count
    );

    (
        StatusCode::OK,
        Json(ProxmoxConnectResponse {
            message: format!("Successfully connected to {} and processed {} VMs.", payload.host, added_vms_count),
            added_vms: added_vms_count,
            machines: all_machines,
        }),
    )
        .into_response()
}

// ========================
// Discover Handler
// ========================

pub async fn discover_proxmox_handler() -> impl IntoResponse {
    const PROXMOX_PORT: u16 = 8006;
    info!("Starting Proxmox discovery scan on port {}", PROXMOX_PORT);

    // Use a tokio spawn to run the blocking netscan operation
    let scan_result = tokio::task::spawn_blocking(move || {
        // Create a scan for all network interfaces
        let interfaces = netdev::get_interfaces();
        
        let mut all_addresses = Vec::new();

        // Define interface filters similar to dragonfly-agent
        let bad_prefixes = ["docker", "virbr", "veth", "cni", "flannel", "br-", "vnet"];
        let bad_names = ["cni0", "docker0", "podman0", "podman1", "virbr0", "k3s0", "k3s1"];
        let preferred_prefixes = ["eth", "en", "wl", "bond", "br0"]; // Common physical network interfaces

        // Scan each interface, but only physical ones
        for interface in interfaces {
            let if_name = &interface.name;
            
            // Skip loopback interfaces
            if interface.is_loopback() {
                info!("Skipping loopback interface {}", if_name);
                continue;
            }
            
            // Skip interfaces with bad prefixes or names
            let has_bad_prefix = bad_prefixes.iter().any(|prefix| if_name.starts_with(prefix));
            let is_bad_name = bad_names.iter().any(|name| if_name == name);
            
            if has_bad_prefix || is_bad_name {
                info!("Skipping virtual/container interface {} (has_bad_prefix={}, is_bad_name={})", 
                     if_name, has_bad_prefix, is_bad_name);
                continue;
            }
            
            // Prioritize interfaces with preferred prefixes
            let is_preferred = preferred_prefixes.iter().any(|prefix| if_name.starts_with(prefix));
            if !is_preferred {
                // Only scan non-preferred interfaces if they're not virtualization/container interfaces
                // and they have IPv4 addresses (might be legitimate but uncommon physical interfaces)
                if interface.ipv4.is_empty() {
                    info!("Skipping non-preferred interface {} with no IPv4 addresses", if_name);
                    continue;
                }
                info!("Interface {} is not preferred but has IPv4 addresses, will scan anyway", if_name);
            }

            // Set up scan targets for all local network addresses
            let mut scan_targets = Vec::new();
            
            for ip_config in &interface.ipv4 {
                let ip_addr = ip_config.addr;
                let prefix_len = ip_config.prefix_len;
                
                // Calculate the network range to scan
                let host_count = if prefix_len >= 30 {
                    // Tiny networks with 1-2 hosts
                    4u32
                } else if prefix_len >= 24 {
                    // Standard /24 networks (256 hosts)
                    1u32 << (32 - prefix_len)
                } else {
                    // Limit larger networks to avoid excessive scanning
                    // For networks larger than /24, just scan up to 256 hosts
                    256u32
                };

                // Log the network being scanned
                let network_addr = calculate_network_address(ip_addr, prefix_len);
                info!("Scanning network {}/{} for Proxmox machines (up to {} hosts)", 
                      network_addr, prefix_len, host_count);
                
                // Generate all possible IPs in the subnet (up to our limit)
                for i in 0..host_count {
                    // Skip network address (i=0) and broadcast address (i=max)
                    if i == 0 || i == host_count - 1 {
                        continue;
                    }
                    
                    let host_ip = generate_ip_in_subnet(network_addr, i);
                    let host = netscan::host::Host::new(host_ip.into(), String::new())
                        .with_ports(vec![PROXMOX_PORT]);
                    scan_targets.push(host);
                }
            }
            
            if scan_targets.is_empty() {
                info!("No scan targets for interface {}, skipping", if_name);
                continue;
            }

            info!("Scanning {} potential hosts on interface {}", scan_targets.len(), if_name);

            // Create port scan settings
            let scan_setting = netscan::scan::setting::PortScanSetting::default()
                .set_if_index(interface.index)
                .set_scan_type(netscan::scan::setting::PortScanType::TcpConnectScan) // Connect scan doesn't require admin privileges
                .set_targets(scan_targets)
                .set_timeout(std::time::Duration::from_secs(5))
                .set_wait_time(std::time::Duration::from_millis(500));
                
            // Create and run scanner
            let scanner = netscan::scan::scanner::PortScanner::new(scan_setting);
            // ScanResult is a struct, not a Result enum
            let scan_result = scanner.scan();
            
            // Extract open ports from results
            for host in scan_result.hosts {
                let open_ports = host.get_open_ports();
                for port in open_ports {
                    if port.number == PROXMOX_PORT {
                        all_addresses.push(std::net::SocketAddr::new(host.ip_addr, PROXMOX_PORT));
                    }
                }
            }
        }

        // Explicitly specify the error type as String
        Ok::<Vec<std::net::SocketAddr>, String>(all_addresses)
    }).await;

    // Handle the result of the scan
    match scan_result {
        Ok(Ok(addresses)) => {
            info!("Proxmox scan found {} potential machines", addresses.len());
            
            // Perform reverse DNS lookups for all addresses
            let machines: Vec<DiscoveredProxmox> = addresses
                .into_iter()
                .map(|socket_addr| {
                    // Extract IP address
                    let ip = socket_addr.ip();
                    let host = ip.to_string();
                    
                    // Attempt reverse DNS lookup
                    let hostname = match tokio::task::block_in_place(|| {
                        std::net::ToSocketAddrs::to_socket_addrs(&(ip, 0))
                            .ok()
                            .and_then(|mut iter| iter.next())
                            .and_then(|addr| {
                                addr.ip()
                                    .to_string()
                                    .parse::<std::net::IpAddr>()
                                    .ok()
                            })
                            .and_then(|ip| {
                                dns_lookup::lookup_addr(&ip).ok()
                            })
                    }) {
                        Some(name) => {
                            if name != host {
                                info!("Resolved IP {} to hostname {}", host, name);
                                Some(name)
                            } else {
                                None
                            }
                        },
                        None => None
                    };
                    
                    DiscoveredProxmox { 
                        host, 
                        port: PROXMOX_PORT,
                        hostname,
                        mac_address: None,
                        machine_type: "host".to_string(),
                        vmid: None,
                        parent_host: None,
                    }
                })
                .collect();

            info!("Completed Proxmox discovery with {} machines", machines.len());
            (StatusCode::OK, Json(ProxmoxDiscoverResponse { machines })).into_response()
        }
        Ok(Err(e)) => {
            error!("Proxmox discovery scan failed: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: format!("Network scan failed: {}", e),
                }),
            )
                .into_response()
        }
        Err(e) => {
            error!("Proxmox discovery task failed: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: format!("Scanner task failed: {}", e),
                }),
            )
                .into_response()
        }
    }
}

// Helper functions for IP subnet operations
fn calculate_network_address(ip: std::net::Ipv4Addr, prefix_len: u8) -> std::net::Ipv4Addr {
    let ip_u32 = u32::from(ip);
    let mask = !((1u32 << (32 - prefix_len)) - 1);
    std::net::Ipv4Addr::from(ip_u32 & mask)
}

fn generate_ip_in_subnet(network_addr: std::net::Ipv4Addr, host_num: u32) -> std::net::Ipv4Addr {
    let network_u32 = u32::from(network_addr);
    std::net::Ipv4Addr::from(network_u32 + host_num)
}

// Helper function to register a new VM
async fn register_new_vm(
    vm_name: &str,
    mac_address: &str,
    vmid: u32,
    node_name: &str,
    host: &str,
    added_vms_count: &mut usize,
    vm_errors: &mut Vec<String>
) {
    info!("Adding new machine for VM {} with MAC {}", vmid, mac_address);
    
    // Create a RegisterRequest
    let register_req = dragonfly_common::models::RegisterRequest {
        mac_address: mac_address.to_string(),
        ip_address: "0.0.0.0".to_string(), // Placeholder until we discover the IP
        hostname: Some(vm_name.to_string()),
        disks: Vec::new(), // We don't have disk info yet
        nameservers: Vec::new(), // No nameserver info yet
        cpu_model: None, // No CPU info yet
        cpu_cores: None, // No CPU info yet
        total_ram_bytes: None, // No RAM info yet
        proxmox_vmid: Some(vmid),
        proxmox_node: Some(node_name.to_string()),
    };
    
    match crate::db::register_machine(&register_req).await {
        Ok(_) => {
            *added_vms_count += 1;
            info!("Successfully registered VM {} with MAC {}", vm_name, mac_address);
        }
        Err(e) => {
            error!("Failed to register VM {}: {}", vm_name, e);
            vm_errors.push(format!("Failed to register VM {}: {}", vm_name, e));
        }
    }
}

// TODO: Add handler for /api/proxmox/discover 