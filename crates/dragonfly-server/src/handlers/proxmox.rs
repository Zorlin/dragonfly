use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use serde::{Deserialize, Serialize};
use tracing::{error, info, warn};
use std::net::IpAddr;

use crate::AppState;
use crate::db;
use proxmox_client::api::{Api, ApiFuture, ApiResult};
use proxmox_client::client::Client;
use proxmox_client::config::nodes::node::qemu::{VmConfig, VmList};
use netscan::scan_network_with_port;

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
}

#[derive(Serialize, Debug)]
struct ErrorResponse {
    error: String,
}

#[derive(Serialize, Debug)]
pub struct ProxmoxDiscoverResponse {
    clusters: Vec<DiscoveredCluster>,
}

#[derive(Serialize, Debug)]
pub struct DiscoveredCluster {
    host: String,
    port: u16,
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

    let client = match Client::new_tls_insecure(
        format!("{}:{}", payload.host, payload.port),
        payload.username,
        payload.password,
    ) {
        Ok(c) => c,
        Err(e) => {
            error!("Failed to create Proxmox client: {}", e);
            return (
                StatusCode::BAD_REQUEST, // Could be bad host/port
                Json(ErrorResponse {
                    error: format!("Failed to create Proxmox client: {}", e),
                }),
            )
                .into_response();
        }
    };

    // Test connection/authentication by fetching nodes
    let nodes_result: ApiResult<Vec<proxmox_client::config::nodes::NodeInformation>> = client
        .get(&Api::new().config().nodes())
        .await;

    let nodes = match nodes_result {
        Ok(n) => n,
        Err(e) => {
            error!("Proxmox authentication/connection failed: {}", e);
            let status = if e.to_string().contains("authentication failed") {
                StatusCode::UNAUTHORIZED
            } else {
                StatusCode::INTERNAL_SERVER_ERROR // Or SERVICE_UNAVAILABLE?
            };
            return (status, Json(ErrorResponse { error: format!("Proxmox connection/auth failed: {}", e) })).into_response();
        }
    };

    info!("Successfully connected to Proxmox host: {}. Found {} nodes.", payload.host, nodes.len());

    let mut added_vms_count = 0;
    let mut vm_errors = Vec::new();

    if payload.vm_selection_option == VmSelectionOption::All {
        // Fetch VMs from all nodes
        for node_info in nodes {
            let node_name = node_info.node;
            info!("Fetching VMs from node: {}", node_name);

            let vms_result: ApiResult<Vec<VmList>> = client
                .get(&Api::new().nodes().node(&node_name).qemu())
                .await;

            match vms_result {
                Ok(vms) => {
                    info!("Found {} VMs on node {}", vms.len(), node_name);
                    for vm_summary in vms {
                        let vmid = vm_summary.vmid;
                        let vm_name = vm_summary.name.unwrap_or_else(|| format!("vm-{}", vmid));
                        
                        // --- TODO: Database Interaction --- 
                        // 1. Get VM config to find MAC address (requires another API call)
                        //    let config_result: ApiResult<VmConfig> = client.get(&Api::new().nodes().node(&node_name).qemu().vm(vmid).config()).await;
                        //    Find network device config (e.g., net0) and extract MAC
                        
                        // 2. Check if machine exists (by MAC or maybe Proxmox VMID?)
                        //    let existing_machine = db::get_machine_by_proxmox_vmid(vmid).await;
                        
                        // 3. If not exists, create Machine struct
                        //    let new_machine = Machine {
                        //        id: Uuid::new_v4(),
                        //        hostname: Some(vm_name.clone()),
                        //        mac_address: extracted_mac, // From config
                        //        ip_address: "".to_string(), // Not easily available here
                        //        status: MachineStatus::ExistingOS, // Or a new ProxmoxManaged status?
                        //        os_choice: None, 
                        //        os_installed: Some("Proxmox VM".to_string()), // Indicate it's a VM
                        //        source: Some("proxmox".to_string()), 
                        //        proxmox_vmid: Some(vmid),
                        //        proxmox_node: Some(node_name.clone()),
                        //        // ... other fields ...
                        //    };
                        
                        // 4. Add to database
                        //    match db::add_machine(&new_machine).await {
                        //        Ok(_) => added_vms_count += 1,
                        //        Err(e) => vm_errors.push(format!("Failed to add VM {} ({}): {}", vmid, vm_name, e)),
                        //    }
                        added_vms_count += 1; // Placeholder increment
                    }
                },
                Err(e) => {
                    let error_msg = format!("Failed to fetch VMs from node {}: {}", node_name, e);
                    error!("{}", error_msg);
                    vm_errors.push(error_msg);
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
        // Maybe return 207 Multi-Status in the future?
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
        }),
    )
        .into_response()
}

// ========================
// Discover Handler
// ========================

pub async fn discover_proxmox_handler(
    // State(app_state): State<AppState>, // Not needed for now
) -> impl IntoResponse {
    const PROXMOX_PORT: u16 = 8006;
    info!("Starting Proxmox discovery scan on port {}", PROXMOX_PORT);

    match scan_network_with_port(PROXMOX_PORT).await { // Assuming await is needed
        Ok(addresses) => {
            info!("Proxmox scan found {} potential hosts", addresses.len());
            let clusters: Vec<DiscoveredCluster> = addresses
                .into_iter()
                .map(|socket_addr| {
                    // Extract IP address, convert to string
                    let host = socket_addr.ip().to_string();
                    DiscoveredCluster { host, port: PROXMOX_PORT }
                })
                .collect();

            (StatusCode::OK, Json(ProxmoxDiscoverResponse { clusters })).into_response()
        }
        Err(e) => {
            error!("Proxmox discovery scan failed: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: format!("Network scan failed: {}", e),
                }),
            )
                .into_response()
        }
    }
}

// TODO: Add handler for /api/proxmox/discover 