use anyhow::Context;
use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use hyper_tls::HttpsConnector;
use proxmox_client::{HttpApiClient, Client as ProxmoxApiClient};
use std::error::Error as StdError;
use proxmox_login;
use proxmox_client::Error as ProxmoxClientError;
use serde::{Serialize, Deserialize};
use tracing::{error, info, warn};
use std::net::Ipv4Addr;

use crate::AppState;
use crate::db;
use dragonfly_common::models::{RegisterRequest, MachineStatus, ErrorResponse};

// Define local structs needed by discover_proxmox_handler
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

#[derive(Serialize, Debug)]
pub struct ProxmoxDiscoverResponse {
    machines: Vec<DiscoveredProxmox>,
}

// New struct to receive connection details from request body
#[derive(Deserialize, Debug)]
pub struct ProxmoxConnectRequest {
    host: String,
    port: Option<u16>,
    username: String,
    password: String,
    vm_selection_option: Option<String>,
    skip_tls_verify: Option<bool>,
}

// Response with suggestion to disable TLS verification
#[derive(Serialize, Debug)]
pub struct ProxmoxConnectResponse {
    message: String,
    suggest_disable_tls_verify: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    added_vms: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    failed_vms: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    machines: Option<Vec<DiscoveredProxmox>>,
}

// Define Authid locally since we don't have the correct import
#[derive(Debug, Clone)]
struct Authid {
    username: String,
    realm: Option<String>,
}

impl Authid {
    fn new(username: &str, realm: Option<&str>) -> Self {
        Authid {
            username: username.to_string(),
            realm: realm.map(|s| s.to_string()),
        }
    }
}

impl std::fmt::Display for Authid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(realm) = &self.realm {
            write!(f, "{}@{}", self.username, realm)
        } else {
            write!(f, "{}", self.username)
        }
    }
}

// Structs matching Proxmox API documentation
#[derive(Debug, Deserialize, Serialize)]
struct CreateTicketRequest {
    username: String,
    password: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    realm: Option<String>,
    #[serde(rename = "new-format")]
    #[serde(skip_serializing_if = "Option::is_none")]
    new_format: Option<bool>,
}

#[derive(Debug, Deserialize, Serialize)]
struct CreateTicketResponse {
    #[serde(rename = "CSRFPreventionToken")]
    csrfprevention_token: Option<String>,
    clustername: Option<String>,
    ticket: Option<String>,
    username: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct ApiResponse<T> {
    data: Option<T>,
}

// Error types
#[derive(Debug, thiserror::Error)]
pub enum ProxmoxHandlerError {
    #[error("Proxmox API error: {0}")]
    ApiError(#[from] ProxmoxClientError),
    #[error("Database error: {0}")]
    DbError(#[from] sqlx::Error),
    #[error("Configuration error: {0}")]
    ConfigError(String),
    #[error("Internal error: {0}")]
    InternalError(#[from] anyhow::Error),
    // Use Box<dyn StdError> for the error types we can't import directly
    #[error("Login error: {0}")]
    LoginError(Box<dyn StdError + Send + Sync>),
    #[error("HTTP client error: {0}")]
    HttpClientError(Box<dyn StdError + Send + Sync>),
    // Add a specific error type for TLS validation issues
    #[error("TLS Certificate validation error: {0}")]
    TlsValidationError(String),
}

// IntoResponse impl: Populate message field
impl IntoResponse for ProxmoxHandlerError {
    fn into_response(self) -> Response {
        let (status, error_message, error_code, suggest_disable_tls_verify) = match &self {
            ProxmoxHandlerError::ApiError(e) => {
                error!("Proxmox API Error: {}", e);
                // Check if the error message indicates a certificate validation issue
                let err_str = e.to_string();
                if err_str.contains("certificate") || 
                   err_str.contains("SSL") || 
                   err_str.contains("TLS") || 
                   err_str.contains("self-signed") || 
                   err_str.contains("unknown issuer") {
                    // Return special error code for certificate issues
                    (
                        StatusCode::BAD_REQUEST,
                        format!("Proxmox SSL certificate validation failed. You may need to try again with certificate validation disabled: {}", e),
                        "TLS_VALIDATION_ERROR".to_string(),
                        true
                    )
                } else {
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        format!("Proxmox API interaction failed: {}", e),
                        "API_ERROR".to_string(),
                        false
                    )
                }
            }
            ProxmoxHandlerError::TlsValidationError(msg) => {
                error!("Proxmox TLS Validation Error: {}", msg);
                (
                    StatusCode::BAD_REQUEST,
                    format!("Proxmox SSL certificate validation failed: {}. Try again with certificate validation disabled.", msg),
                    "TLS_VALIDATION_ERROR".to_string(),
                    true
                )
            }
            ProxmoxHandlerError::DbError(e) => {
                error!("Database Error: {}", e);
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Database operation failed: {}", e),
                    "DB_ERROR".to_string(),
                    false
                )
            }
            ProxmoxHandlerError::ConfigError(msg) => {
                error!("Configuration Error: {}", msg);
                (
                    StatusCode::BAD_REQUEST,
                    msg.clone(),
                    "CONFIG_ERROR".to_string(),
                    false
                )
            }
            ProxmoxHandlerError::InternalError(e) => {
                error!("Internal Server Error: {}", e);
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "An internal server error occurred.".to_string(),
                    "INTERNAL_ERROR".to_string(),
                    false
                )
            }
            ProxmoxHandlerError::LoginError(e) => {
                error!("Proxmox Login Error: {}", e);
                (
                    StatusCode::UNAUTHORIZED,
                    format!("Proxmox authentication failed: {}", e),
                    "LOGIN_ERROR".to_string(),
                    false
                )
            }
            ProxmoxHandlerError::HttpClientError(e) => {
                error!("Proxmox HTTP Client Error: {}", e);
                let err_str = e.to_string();
                // Also check HTTP client errors for certificate issues
                if err_str.contains("certificate") || 
                   err_str.contains("SSL") || 
                   err_str.contains("TLS") || 
                   err_str.contains("self signed") || 
                   err_str.contains("unknown issuer") {
                    (
                        StatusCode::BAD_REQUEST,
                        format!("Proxmox SSL certificate validation failed: {}. Try again with certificate validation disabled.", e),
                        "TLS_VALIDATION_ERROR".to_string(),
                        true
                    )
                } else {
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        format!("Proxmox HTTP communication failed: {}", e),
                        "HTTP_ERROR".to_string(),
                        false
                    )
                }
            }
        };
        
        // Create a JSON response with error and optional TLS suggestion
        let response_json = serde_json::json!({
            "error": error_code,
            "message": error_message,
            "suggest_disable_tls_verify": suggest_disable_tls_verify
        });
        
        // Ensure we're returning proper JSON
        (status, Json(response_json)).into_response()
    }
}

// Make ProxmoxResult public as well
pub type ProxmoxResult<T> = std::result::Result<T, ProxmoxHandlerError>;

#[axum::debug_handler]
pub async fn connect_proxmox_handler(
    State(state): State<AppState>,
    Json(request): Json<ProxmoxConnectRequest>,
) -> ProxmoxResult<Json<ProxmoxConnectResponse>> {
    info!("Connecting to Proxmox instance...");

    // Use the connection details from the request
    let host = request.host.clone();
    let username_input = request.username.clone();
    let password = request.password.clone();
    let port = request.port.unwrap_or(8006);
    let skip_tls_verify = request.skip_tls_verify.unwrap_or(false);

    // Parse username@realm format if present
    let (username, realm) = if let Some(idx) = username_input.find('@') {
        let (username_part, realm_part) = username_input.split_at(idx);
        // Remove the @ from the beginning of realm
        (username_part.to_string(), Some(realm_part[1..].to_string()))
    } else {
        (username_input.clone(), Some("pam".to_string()))
    };

    // Store the settings for future use
    {
        let mut settings = state.settings.lock().await;
        settings.proxmox_host = Some(host.clone());
        settings.proxmox_username = Some(username_input.clone());
        settings.proxmox_password = Some(password.clone());
        settings.proxmox_port = Some(port);
        settings.proxmox_skip_tls_verify = Some(skip_tls_verify);
    }

    // Create HTTPS connector
    if skip_tls_verify {
        info!("TLS verification disabled");
    } else {
        info!("Using standard TLS verification");
    }
    
    let https = HttpsConnector::new();
    let _hyper_client = hyper::Client::builder().build::<_, hyper::Body>(https);

    // Use just the host URL for client initialization
    let host_url = format!("https://{}:{}", host, port);
    let base_uri: hyper::Uri = host_url.parse::<hyper::Uri>().map_err(|e| {
        ProxmoxHandlerError::ConfigError(format!("Invalid Proxmox URL '{}': {}", host_url, e))
    })?;

    // Initialize the Proxmox client with the host URL only
    let client = ProxmoxApiClient::new(base_uri.clone());

    // Create the login object from proxmox-login
    // Combine username and realm for the login object as expected by the library
    let login_user = match &realm {
        Some(r) => format!("{}@{}", username, r),
        None => username.clone(), // Should ideally specify pam? Check library defaults
    };
    // Login::new still needs the host URL
    let login_builder = proxmox_login::Login::new(&host_url, login_user.clone(), password.clone());

    // Log the full user identifier being used for the login attempt
    info!("Attempting login to Proxmox at {}:{} with user identifier '{}'", host, port, login_user);

    // Perform login using the client's login method
    match client.login(login_builder).await {
        Ok(None) => {
            // Login successful (no TFA challenge)
            info!("Successfully authenticated with Proxmox API via client.login()");

            // Now use the *same* client instance for subsequent requests.
            // Use the full API path relative to the client's base host URL
            match client.get("/api2/json/cluster/status").await {
                Ok(status_response) => {
                    info!("Successfully received Proxmox cluster status response shell");

                    // The body is already a Vec<u8>, no need to read chunks or use to_bytes
                    let body_bytes = status_response.body; 

                    // Deserialize the body bytes directly
                    let cluster_status_value: serde_json::Value = match serde_json::from_slice(&body_bytes) {
                        Ok(value) => value,
                        Err(e) => {
                            error!("Failed to parse cluster status JSON: {}", e);
                            // Log the actual response body for debugging
                            if let Ok(body_str) = std::str::from_utf8(&body_bytes) {
                                error!("Response body: {}", body_str);
                            }
                            return Err(ProxmoxHandlerError::ApiError(
                                ProxmoxClientError::Api(
                                    hyper::StatusCode::INTERNAL_SERVER_ERROR, // Use hyper's StatusCode
                                    format!("Failed to parse cluster status JSON: {}", e)
                                )
                            ));
                        }
                    };

                    info!("Successfully parsed cluster status response JSON");

                    // The rest of the logic remains similar, operating on cluster_status_value
                    let cluster_status_data = cluster_status_value.get("data").cloned().unwrap_or(serde_json::Value::Null);

                    // Find the cluster name (assuming data field contains the array)
                    let cluster_name = cluster_status_data
                        .as_array()
                        .and_then(|arr| arr.iter().find(|item| item.get("type").and_then(|t| t.as_str()) == Some("cluster")))
                        .and_then(|cluster_entry| cluster_entry.get("name").and_then(|n| n.as_str()))
                        .map(String::from)
                        .unwrap_or_else(|| {
                            warn!("Could not find \"cluster\" type entry or name in Proxmox cluster status response data.");
                            // Fallback to a default name
                            "proxmox-cluster".to_string()
                        });

                    info!("Proxmox cluster name: {}", cluster_name);

                    // If we need to discover and register VMs, do it here
                    // Pass the authenticated client
                    let (added_vms, failed_vms, machines) = discover_and_register_proxmox_vms(&client, &cluster_name, &state)
                        .await
                        .context("Failed during Proxmox VM discovery and registration")?;

                    info!("Successfully connected to Proxmox cluster: {}", cluster_name);

                    Ok(Json(ProxmoxConnectResponse {
                        message: format!("Successfully connected to Proxmox cluster: {} and registered {} VMs ({} failed)", 
                                         cluster_name, added_vms, failed_vms),
                        suggest_disable_tls_verify: false,
                        added_vms: Some(added_vms),
                        failed_vms: Some(failed_vms),
                        machines: Some(machines)
                    }))
                },
                Err(e) => {
                    error!("Failed to get cluster status: {}", e);
                    // Use the existing error handler, but pass the error directly
                    handle_proxmox_error(e, skip_tls_verify)
                }
            }
        },
        Ok(Some(_tfa_challenge)) => {
            // TFA is required, which is not handled yet
            error!("Proxmox login requires Two-Factor Authentication, which is not supported yet.");
            Err(ProxmoxHandlerError::LoginError(
                Box::new(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "Proxmox authentication failed: TFA Required".to_string(),
                ))
            ))
        },
        Err(e) => {
            // Log the detailed error from proxmox-client
            error!("Proxmox login failed. Detailed error: {:?}", e);
            // Use the existing error handler for login failures, passing the detailed error
            handle_proxmox_error(e, skip_tls_verify)
        }
    }
}

// Helper to handle Proxmox errors consistently
fn handle_proxmox_error(e: ProxmoxClientError, skip_tls_verify: bool) -> ProxmoxResult<Json<ProxmoxConnectResponse>> {
    // Check if the error might be related to TLS or authentication
    let err_str = e.to_string();
    if err_str.contains("certificate") || 
       err_str.contains("SSL") || 
       err_str.contains("TLS") || 
       err_str.contains("self signed") || 
       err_str.contains("unknown issuer") {
        // If this appears to be a TLS issue and we haven't already tried with skip_tls_verify
        if !skip_tls_verify {
            Err(ProxmoxHandlerError::TlsValidationError(
                "Could not verify SSL certificate. Try again with certificate validation disabled.".to_string()
            ))
        } else {
            // We already tried with skip_tls_verify=true but still got an error
            Err(ProxmoxHandlerError::ApiError(e))
        }
    } else if err_str.contains("unauthorized") || 
              err_str.contains("authentication") || 
              err_str.contains("401") {
        // Authentication error
        Err(ProxmoxHandlerError::LoginError(
            Box::new(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!("Proxmox authentication failed: {}", e),
            ))
        ))
    } else {
        // Other API error
        Err(ProxmoxHandlerError::ApiError(e))
    }
}

// Helper function for discovery and registration
async fn discover_and_register_proxmox_vms(
    client: &ProxmoxApiClient,
    cluster_name: &str,
    _state: &AppState,
) -> ProxmoxResult<(usize, usize, Vec<DiscoveredProxmox>)> {
    info!("Discovering and registering Proxmox VMs for cluster: {}", cluster_name);
    
    // First, get the list of nodes in the cluster
    let nodes_response = client.get("/api2/json/nodes").await
        .map_err(|e| {
            error!("Failed to fetch nodes list: {}", e);
            ProxmoxHandlerError::ApiError(e)
        })?;
    
    // Parse the response
    let nodes_value: serde_json::Value = serde_json::from_slice(&nodes_response.body)
        .map_err(|e| {
            error!("Failed to parse nodes response: {}", e);
            ProxmoxHandlerError::InternalError(anyhow::anyhow!("Failed to parse nodes JSON: {}", e))
        })?;
    
    // Extract the nodes data
    let nodes_data = nodes_value.get("data")
        .and_then(|d| d.as_array())
        .ok_or_else(|| {
            error!("Invalid nodes response format");
            ProxmoxHandlerError::InternalError(anyhow::anyhow!("Invalid nodes response format"))
        })?;
    
    info!("Found {} nodes in Proxmox cluster", nodes_data.len());
    
    let mut registered_machines = 0;
    let mut failed_registrations = 0;
    let mut discovered_machines = Vec::new();
    
    // For each node, get the VMs
    for node in nodes_data {
        let node_name = node.get("node")
            .and_then(|n| n.as_str())
            .ok_or_else(|| {
                error!("Node missing 'node' field");
                ProxmoxHandlerError::InternalError(anyhow::anyhow!("Node missing 'node' field"))
            })?;
        
        // Get node details for more information
        let node_details_path = format!("/api2/json/nodes/{}/status", node_name);
        let mut host_ip_address = None; // Store as Option<String>
        let mut host_hostname = node_name.to_string(); // Default to node name

        // Try to get more details about the node (like IP from status)
        if let Ok(node_details_response) = client.get(&node_details_path).await {
            if let Ok(node_details_value) = serde_json::from_slice::<serde_json::Value>(&node_details_response.body) {
                if let Some(node_details_data) = node_details_value.get("data") {
                    // Try to get IP address from the node details
                    host_ip_address = node_details_data.get("ip").and_then(|i| i.as_str()).map(String::from);
                    
                    // Try to get version information
                    if let Some(version) = node_details_data.get("pveversion").and_then(|v| v.as_str()) {
                        info!("Node {} is running Proxmox version: {}", node_name, version);
                        host_hostname = format!("{} (PVE {})", node_name, version); // Include version in hostname?
                    } else {
                        host_hostname = node_name.to_string(); // Fallback if no version
                    }
                }
            } else {
                warn!("Failed to parse node details JSON for {}: {:?}", node_name, node_details_response.body);
            }
        } else {
             warn!("Failed to get node details for {}", node_name);
        }
        
        // Get network interface information to find the primary MAC address
        let node_net_path = format!("/api2/json/nodes/{}/network", node_name);
        let mut host_mac_address = None; // Store as Option<String>

        if let Ok(node_net_response) = client.get(&node_net_path).await {
            if let Ok(node_net_value) = serde_json::from_slice::<serde_json::Value>(&node_net_response.body) {
                if let Some(net_data) = node_net_value.get("data").and_then(|d| d.as_array()) {
                    // Look for a physical interface (like eth0) or bridge (vmbr0) with a MAC
                    for iface in net_data {
                        let iface_type = iface.get("type").and_then(|t| t.as_str()).unwrap_or("");
                        let iface_name = iface.get("iface").and_then(|n| n.as_str()).unwrap_or("");
                        // Proxmox might store MAC in hwaddr or ether or address?
                        let mac = iface.get("hwaddr")
                            .or_else(|| iface.get("ether"))
                            .or_else(|| iface.get("address")) // Less likely but check
                            .and_then(|h| h.as_str());

                        // Prioritize known physical/bridge interfaces
                        if let Some(mac_str) = mac {
                            if iface_type == "eth" || iface_type == "bond" || iface_name.starts_with("vmbr") {
                                // Basic validation
                                if mac_str.len() == 17 && mac_str.contains(':') {
                                    host_mac_address = Some(mac_str.to_lowercase());
                                    info!("Found potential host MAC {} on interface {} for node {}", host_mac_address.as_ref().unwrap(), iface_name, node_name);
                                    break; // Found a likely candidate
                                }
                            }
                        }
                    }
                }
                 if host_mac_address.is_none() {
                    warn!("Could not determine primary MAC for host node {} from network config.", node_name);
                }
            } else {
                 warn!("Failed to parse node network JSON for {}: {:?}", node_name, node_net_response.body);
            }
        } else {
             warn!("Failed to get node network info for {}", node_name);
        }
        
        // --- Register the Host Node --- 
        if let Some(mac) = host_mac_address {
             let host_req = RegisterRequest {
                mac_address: mac.clone(), // Already lowercased
                // Use "Unknown" as default value instead of a fake IP
                ip_address: host_ip_address.unwrap_or_else(|| "Unknown".to_string()), 
                hostname: Some(host_hostname.clone()), // Use node name (potentially with version)
                proxmox_vmid: None, 
                proxmox_node: Some(node_name.to_string()),
                proxmox_cluster: Some(cluster_name.to_string()),
                cpu_cores: None, 
                total_ram_bytes: None, 
                                    disks: Vec::new(),
                                    nameservers: Vec::new(),
                                    cpu_model: None,
                                };
            info!(?host_req, "Attempting to register Proxmox host node with DB");
            match db::register_machine(&host_req).await { 
                                    Ok(machine_id) => {
                    info!("Successfully registered/updated Proxmox host node '{}' as machine ID {}", node_name, machine_id);
                }
                                    Err(e) => {
                    error!("Failed to register Proxmox host node '{}': {}", node_name, e);
                    // Log error but continue to VMs for this node
                }
            }
        } else {
             warn!("Skipping registration of host node '{}' because MAC address could not be determined.", node_name);
        }

        // --- Fetch and Register VMs for this node ---
        info!("Processing VMs for node: {}", node_name);
        
        // Get VM list for this node
        let vms_path = format!("/api2/json/nodes/{}/qemu", node_name);
        let vms_response = match client.get(&vms_path).await {
            Ok(response) => response,
            Err(e) => {
                error!("Failed to fetch VMs for node {}: {}", node_name, e);
                continue; // Skip this node but continue with others
            }
        };
        
        // Parse the response
        let vms_value: serde_json::Value = match serde_json::from_slice(&vms_response.body) {
            Ok(value) => value,
            Err(e) => {
                error!("Failed to parse VMs response for node {}: {}", node_name, e);
                continue; // Skip this node but continue with others
            }
        };
        
        // Extract the VMs data
        let vms_data = match vms_value.get("data").and_then(|d| d.as_array()) {
            Some(data) => data,
            None => {
                error!("Invalid VMs response format for node {}", node_name);
                continue; // Skip this node but continue with others
            }
        };
        
        info!("Found {} VMs on node {}", vms_data.len(), node_name);
        
        // Register each VM
        for vm in vms_data {
            let vmid = match vm.get("vmid").and_then(|id| id.as_u64()).map(|id| id as u32) {
                Some(id) => id,
                None => {
                    error!("VM missing vmid");
                    continue; // Skip this VM but continue with others
                }
            };
            
            let name = vm.get("name")
                .and_then(|n| n.as_str())
                .unwrap_or("unknown");
            
            let status = vm.get("status")
                .and_then(|s| s.as_str())
                .unwrap_or("unknown");
            
            // Determine OS based on VM name or additional queries
            // Print OS name
            info!("OS name: {}", name);
            let mut vm_os = "Unknown OS".to_string();
            if name.to_lowercase().contains("ubuntu") {
                vm_os = "Ubuntu 22.04".to_string();
            } else if name.to_lowercase().contains("debian") {
                vm_os = "Debian 12".to_string();
            } else if name.to_lowercase().contains("centos") {
                vm_os = "CentOS 7".to_string();
            } else if name.to_lowercase().contains("windows") {
                vm_os = "Windows Server".to_string();
            }
            
            // Get VM details from Proxmox API
            let vm_details_path = format!("/api2/json/nodes/{}/qemu/{}/status/current", node_name, vmid);
            let mut vm_mem_bytes = 0;
            let mut vm_cpu_cores = 0;
            
            if let Ok(vm_details_response) = client.get(&vm_details_path).await {
                if let Ok(vm_details_value) = serde_json::from_slice::<serde_json::Value>(&vm_details_response.body) {
                    if let Some(vm_details_data) = vm_details_value.get("data") {
                        // Get memory info
                        if let Some(mem) = vm_details_data.get("maxmem").and_then(|m| m.as_u64()) {
                            vm_mem_bytes = mem;
                        }
                        
                        // Get CPU info
                        if let Some(cpu) = vm_details_data.get("cpus").and_then(|c| c.as_u64()) {
                            vm_cpu_cores = cpu as u32;
                        }
                    }
                }
            }
            
            // Get VM config to retrieve MAC address and other details
            let vm_config_path = format!("/api2/json/nodes/{}/qemu/{}/config", node_name, vmid);
            let vm_config_response = match client.get(&vm_config_path).await {
                Ok(response) => response,
                Err(e) => {
                    error!("Failed to fetch VM config for VM {}: {}", vmid, e);
                    continue; // Skip this VM but continue with others
                }
            };
            
            // Parse the VM config response
            let vm_config: serde_json::Value = match serde_json::from_slice(&vm_config_response.body) {
                Ok(value) => value,
                Err(e) => {
                    error!("Failed to parse VM config response for VM {}: {}", vmid, e);
                    continue; // Skip this VM but continue with others
                }
            };
            
            // Extract network interfaces and MAC addresses
            let mut mac_addresses = Vec::new();
            let config_data = match vm_config.get("data") {
                Some(data) => data,
                None => {
                    error!("Invalid VM config response format for VM {}", vmid);
                    continue; // Skip this VM but continue with others
                }
            };

            // Check if the Guest Agent is enabled
            let mut agent_enabled = false;
            if let Some(agent) = config_data.get("agent").and_then(|a| a.as_str()) {
                agent_enabled = agent.contains("enabled=1") || agent.contains("enabled=true");
                info!("QEMU Guest Agent status for VM {}: {}", vmid, if agent_enabled { "Enabled" } else { "Disabled" });
            }
            
            // Check for OS info in the config
            if let Some(os_type) = config_data.get("ostype").and_then(|o| o.as_str()) {
                match os_type {
                    "l26" => vm_os = "Unknown".to_string(), // Generic Linux should be Unknown
                    "win10" | "win11" => vm_os = "windows-10".to_string(),
                    "win8" | "win7" => vm_os = "windows-7".to_string(),
                    "other" => {} // Keep current OS guess
                    _ => vm_os = "unknown".to_string(),
                }
                info!("VM {} has OS type {} (from Proxmox config)", vmid, vm_os);
            }
            
            // Proxmox configures network interfaces like net0, net1, etc.
            // Each of these is a string like "virtio=XX:XX:XX:XX:XX:XX,bridge=vmbr0"
            for i in 0..8 {  // Assume max 8 network interfaces
                let net_key = format!("net{}", i);
                if let Some(net_config) = config_data.get(&net_key).and_then(|n| n.as_str()) {
                    // Parse the MAC address from the net config string
                    if let Some(mac) = extract_mac_from_net_config(net_config) {
                        mac_addresses.push(mac);
                    }
                }
            }
            
            if mac_addresses.is_empty() {
                error!("No MAC addresses found for VM {}", vmid);
                continue; // Skip this VM but continue with others
            }
            
            // Use the first MAC address for registration
            let mac_address = mac_addresses[0].clone().to_lowercase(); // Ensure lowercase
            
            // Try to get the IP address from the QEMU Guest Agent if enabled
            let mut ip_address = "Unknown".to_string(); // Default to Unknown
            
            if agent_enabled {
                // First check if agent is actually running
                let agent_ping_path = format!("/api2/json/nodes/{}/qemu/{}/agent/ping", node_name, vmid);
                let agent_running = match client.get(&agent_ping_path).await {
                    Ok(ping_response) => {
                        if let Ok(ping_value) = serde_json::from_slice::<serde_json::Value>(&ping_response.body) {
                            // Check for successful response (should contain data with no error)
                            ping_value.get("data").is_some() && !ping_value.get("data").and_then(|d| d.get("error")).is_some()
                        } else {
                            false
                        }
                    },
                    Err(_) => false
                };
                
                if agent_running {
                    info!("QEMU Guest Agent is running for VM {}, attempting to retrieve network interfaces", vmid);
                    
                    // First, try to get OS information
                    let agent_os_path = format!("/api2/json/nodes/{}/qemu/{}/agent/get-osinfo", node_name, vmid);
                    let os_detected = match client.get(&agent_os_path).await {
                        Ok(os_response) => {
                            match serde_json::from_slice::<serde_json::Value>(&os_response.body) {
                                Ok(os_value) => {
                                    // Pretty print for debugging
                                    info!("OS info response for VM {}: {}", vmid, 
                                          serde_json::to_string_pretty(&os_value).unwrap_or_else(|_| "Failed to format".to_string()));
                                    
                                    // Extract useful OS information
                                    if let Some(result) = os_value.get("data").and_then(|d| d.get("result")) {
                                        // Log the raw result for debugging
                                        info!("Raw OS info for VM {}: {}", vmid, serde_json::to_string(result).unwrap_or_default());
                                        
                                        let os_name = result.get("id").and_then(|id| id.as_str()).unwrap_or("Unknown");
                                        let os_version = result.get("version").and_then(|v| v.as_str()).unwrap_or("");
                                        let os_pretty_name = result.get("pretty-name").and_then(|pn| pn.as_str());
                                        
                                        // First determine the detected OS for logging
                                        let detected_os = if let Some(pretty) = os_pretty_name {
                                            pretty.to_string()
                                        } else if !os_version.is_empty() {
                                            format!("{} {}", os_name, os_version)
                                        } else {
                                            os_name.to_string()
                                        };
                                        
                                        // Now standardize the OS name to match our UI format
                                        let os_name_lower = os_name.to_lowercase();
                                        
                                        vm_os = if os_name_lower.contains("ubuntu") || detected_os.to_lowercase().contains("ubuntu") {
                                            // Extract major version, e.g., "22.04" -> "2204"
                                            if os_version.contains(".") {
                                                let version_parts: Vec<&str> = os_version.split('.').collect();
                                                if version_parts.len() >= 2 {
                                                    format!("ubuntu-{}{}", version_parts[0], version_parts[1])
                                                } else {
                                                    format!("ubuntu-{}", os_version.replace(".", ""))
                                                }
                                            } else if detected_os.contains("22.04") {
                                                "ubuntu-2204".to_string()
                                            } else if detected_os.contains("24.04") {
                                                "ubuntu-2404".to_string()
                                            } else {
                                                "ubuntu".to_string()
                                            }
                                        } else if os_name_lower.contains("debian") || detected_os.to_lowercase().contains("debian") {
                                            // Try to extract version from pretty name or version string
                                            if detected_os.contains("12") || detected_os.contains("bookworm") {
                                                "debian-12".to_string()
                                            } else if let Some(version) = os_version.split(' ').next().and_then(|v| v.parse::<u32>().ok()) {
                                                format!("debian-{}", version)
                                            } else {
                                                "debian".to_string()
                                            }
                                        } else {
                                            // For other OSes, keep the detected format but log it
                                            detected_os.clone()
                                        };
                                        
                                        info!("Guest Agent detected OS for VM {}: {} (standardized as: {})", vmid, detected_os, vm_os);
                                        true
                                    } else {
                                        info!("No OS information in Guest Agent response for VM {}", vmid);
                                        false
                                    }
                                }
                                Err(e) => {
                                    warn!("Failed to parse Guest Agent OS info response for VM {}: {}", vmid, e);
                                    false
                                }
                            }
                        }
                        Err(e) => {
                            warn!("Failed to get OS info from Guest Agent for VM {}: {}", vmid, e);
                            false
                        }
                    };
                    
                    if !os_detected {
                        info!("Using fallback OS detection for VM {}: {}", vmid, vm_os);
                    }
                    
                    // Then, get network interfaces (existing code)
                    let agent_path = format!("/api2/json/nodes/{}/qemu/{}/agent/network-get-interfaces", node_name, vmid);
                    
                    match client.get(&agent_path).await {
                        Ok(agent_response) => {
                            match serde_json::from_slice::<serde_json::Value>(&agent_response.body) {
                                Ok(agent_value) => {
                                    // Pretty print the full response for debugging
                                    info!("Full Guest Agent response for VM {}: {}", vmid, 
                                          serde_json::to_string_pretty(&agent_value).unwrap_or_else(|_| "Failed to format".to_string()));
                                    
                                    if let Some(result) = agent_value.get("data").and_then(|d| d.get("result")) {
                                        // QEMU agent returns array of network interfaces
                                        if let Some(interfaces) = result.as_array() {
                                            info!("Found {} network interfaces for VM {}", interfaces.len(), vmid);
                                            
                                            // --- Modified IP Detection Logic ---
                                            let mut preferred_ip: Option<String> = None;
                                            let mut fallback_ip: Option<String> = None;

                                            // First pass: Look for preferred interfaces (eth*, ens*, eno*)
                                            for iface in interfaces {
                                                if let Some(name) = iface.get("name").and_then(|n| n.as_str()) {
                                                    if name.starts_with("lo") { continue; } // Skip loopback
                                                    
                                                    // Check if it's a preferred interface
                                                    let is_preferred = name.starts_with("eth") || name.starts_with("ens") || name.starts_with("eno");
                                                    if !is_preferred { continue; } // Skip non-preferred in this pass
                                                    
                                                    info!("Processing preferred interface '{}' for VM {}", name, vmid);
                                                    
                                                    if let Some(ip_addr) = find_valid_ipv4_in_interface(iface, vmid) {
                                                        preferred_ip = Some(ip_addr);
                                                        break; // Found IP on a preferred interface
                                                    }
                                                }
                                            }

                                            // Second pass: Look in other interfaces if no preferred IP was found
                                            if preferred_ip.is_none() {
                                                info!("No IP found on preferred interfaces for VM {}. Checking others.", vmid);
                                                for iface in interfaces {
                                                    if let Some(name) = iface.get("name").and_then(|n| n.as_str()) {
                                                        // Skip loopback and already checked preferred interfaces
                                                        if name.starts_with("lo") || name.starts_with("eth") || name.starts_with("ens") || name.starts_with("eno") { continue; }
                                                        // Skip common virtual interfaces (like tailscale, docker, etc.)
                                                        if name.starts_with("tailscale") || name.starts_with("docker") || name.starts_with("veth") || name.starts_with("virbr") || name.starts_with("br-") { continue; }

                                                        info!("Processing fallback interface '{}' for VM {}", name, vmid);
                                                        
                                                        if let Some(ip_addr) = find_valid_ipv4_in_interface(iface, vmid) {
                                                            fallback_ip = Some(ip_addr);
                                                            break; // Found first valid fallback IP
                                                        }
                                                    }
                                                }
                                            }

                                            // Assign the IP address based on priority
                                            if let Some(preferred) = preferred_ip {
                                                ip_address = preferred;
                                                info!("Selected preferred IPv4 address {} for VM {} via Guest Agent", ip_address, vmid);
                                            } else if let Some(fallback) = fallback_ip {
                                                ip_address = fallback;
                                                info!("Selected fallback IPv4 address {} for VM {} via Guest Agent", ip_address, vmid);
                                            } else {
                                                info!("No suitable IPv4 address found for VM {} via Guest Agent", vmid);
                                                // ip_address remains "Unknown"
                                            }
                                            // --- End Modified IP Detection Logic ---
                                            
                                        } else {
                                            info!("No network interfaces array found in Guest Agent response for VM {}", vmid);
                                        }
                                    } else {
                                        info!("No 'result' field in Guest Agent response for VM {}", vmid);
                                    }
                                }
                                Err(e) => warn!("Failed to parse Guest Agent response for VM {}: {}", vmid, e),
                            }
                        }
                        Err(e) => warn!("Failed to get network interfaces from QEMU Guest Agent for VM {}: {}", vmid, e),
                    }
                }
            } else {
                info!("QEMU Guest Agent not enabled for VM {}. IP will be set to Unknown.", vmid);
            }
            
            // If the Guest Agent didn't provide an IP, leave it as "Unknown"
            // We no longer generate fake deterministic IPs
            
            // Add this VM to our discovered machines list
            discovered_machines.push(DiscoveredProxmox {
                host: format!("{}-{}", node_name, vmid),
                port: 0, // VMs don't have a port
                hostname: Some(name.to_string()),
                mac_address: Some(mac_address.clone()),
                machine_type: "proxmox-vm".to_string(),
                vmid: Some(vmid),
                parent_host: Some(node_name.to_string()),
            });
            
            info!("Processing VM {} (ID: {}, Status: {}, OS: {}, IP: {})", name, vmid, status, vm_os, ip_address);
            
            // Prepare RegisterRequest
            let register_request = RegisterRequest {
                mac_address,
                ip_address,
                hostname: Some(name.to_string()),
                disks: Vec::new(), // We don't know the disks yet
                nameservers: Vec::new(), // We don't know the nameservers yet
                cpu_model: Some("Proxmox Virtual CPU".to_string()), // Generic CPU model
                cpu_cores: Some(vm_cpu_cores),
                total_ram_bytes: Some(vm_mem_bytes),
                proxmox_vmid: Some(vmid),
                proxmox_node: Some(node_name.to_string()),
                proxmox_cluster: Some(cluster_name.to_string()),
            };

            // DEBUG: Log the request before attempting registration
            info!(?register_request, "Attempting to register VM with DB");
            
            // Register the VM
            match db::register_machine(&register_request).await {
                Ok(machine_id) => {
                    info!("Successfully registered Proxmox VM {} as machine {}", vmid, machine_id);
                    
                    // Get the new machine to register with Tinkerbell
                    if let Ok(Some(machine)) = db::get_machine_by_id(&machine_id).await {
                        // Register with Tinkerbell (don't fail if this fails)
                        // Assuming tinkerbell module is accessible via crate::tinkerbell
                        if let Err(e) = crate::tinkerbell::register_machine(&machine).await {
                            warn!("Failed to register machine with Tinkerbell (continuing anyway): {}", e);
                        }
                        
                        // Update the machine status and OS - requires dbpool
                        let machine_status = match status {
                            "running" => MachineStatus::Ready,
                            "stopped" => MachineStatus::Offline,
                            _ => MachineStatus::ExistingOS,
                        };
                        
                        let _ = db::update_status(&machine_id, machine_status).await;
                        let _ = db::update_os_installed(&machine_id, &vm_os).await;
                    }
                    
                    registered_machines += 1;
                },
                Err(e) => {
                    error!("Failed to register Proxmox VM {}: {}", vmid, e);
                    failed_registrations += 1;
                }
            }
        }
    }
    
    // Return success with a summary
    info!("Proxmox VM discovery and registration complete: {} successful, {} failed", 
           registered_machines, failed_registrations);
    
    Ok((registered_machines, failed_registrations, discovered_machines))
}

// Helper function to extract MAC address from Proxmox network configuration
fn extract_mac_from_net_config(net_config: &str) -> Option<String> {
    // Proxmox network configs look like: "virtio=XX:XX:XX:XX:XX:XX,bridge=vmbr0"
    // or "e1000=XX:XX:XX:XX:XX:XX,bridge=vmbr0"
    
    // Split by comma and look for the part with the MAC address
    for part in net_config.split(',') {
        // The part with MAC should start with "virtio=" or "e1000=" or another NIC type
        if part.contains('=') {
            let mut parts = part.splitn(2, '=');
            _ = parts.next(); // Skip the NIC type
            if let Some(mac) = parts.next() {
                // Verify this looks like a MAC address (XX:XX:XX:XX:XX:XX)
                if mac.len() == 17 && mac.bytes().filter(|&b| b == b':').count() == 5 {
                    // Convert to lowercase to satisfy Tinkerbell requirements
                    return Some(mac.to_lowercase());
                }
            }
        }
    }
    
    None
}

// --- NEW HELPER FUNCTION ---
// Helper to find the first valid, non-loopback, non-link-local IPv4 address in a single interface object
fn find_valid_ipv4_in_interface(iface: &serde_json::Value, vmid: u32) -> Option<String> {
    let interface_name = iface.get("name").and_then(|n| n.as_str()).unwrap_or("unknown");
    if let Some(ip_addresses) = iface.get("ip-addresses").and_then(|ips| ips.as_array()) {
        info!("Checking {} IP addresses on interface '{}' for VM {}", ip_addresses.len(), interface_name, vmid);
        
        for ip_obj in ip_addresses {
            // Debug each IP address entry
            info!("IP address entry for VM {} on interface {}: {}", vmid, interface_name,
                  serde_json::to_string_pretty(&ip_obj).unwrap_or_else(|_| "Failed to format".to_string()));
            
            let ip_type = ip_obj.get("ip-address-type").and_then(|t| t.as_str());
            let ip = ip_obj.get("ip-address").and_then(|a| a.as_str());
            
            info!("Found IP address type: {:?}, address: {:?}", ip_type, ip);
            
            if let (Some("ipv4"), Some(addr)) = (ip_type, ip) {
                // Skip link-local addresses (169.254.x.x)
                if addr.starts_with("169.254.") {
                    info!("Skipping link-local address {} for VM {}", addr, vmid);
                    continue;
                }
                
                // Skip loopback addresses (127.x.x.x)
                if addr.starts_with("127.") {
                    info!("Skipping loopback address {} for VM {}", addr, vmid);
                    continue;
                }
                
                // Found a valid IPv4 address
                return Some(addr.to_string()); 
            }
        }
    }
    // No valid IPv4 found in this interface
    None
}
// --- END NEW HELPER FUNCTION ---

// ========================
// Discover Handler
// ========================

pub async fn discover_proxmox_handler() -> impl IntoResponse {
    const PROXMOX_PORT: u16 = 8006;
    info!("Starting Proxmox discovery scan on port {}", PROXMOX_PORT);

    let scan_result = tokio::task::spawn_blocking(move || {
        let interfaces = netdev::get_interfaces();
        let mut all_addresses = Vec::new();
        let bad_prefixes = ["docker", "virbr", "veth", "cni", "flannel", "br-", "vnet"];
        let bad_names = ["cni0", "docker0", "podman0", "podman1", "virbr0", "k3s0", "k3s1"];
        let preferred_prefixes = ["eth", "en", "wl", "bond", "br0"];

        for interface in interfaces {
            let if_name = &interface.name;
            if interface.is_loopback() {
                continue;
            }
            let has_bad_prefix = bad_prefixes.iter().any(|prefix| if_name.starts_with(prefix));
            let is_bad_name = bad_names.iter().any(|name| if_name == name);
            if has_bad_prefix || is_bad_name {
                continue;
            }
            let is_preferred = preferred_prefixes.iter().any(|prefix| if_name.starts_with(prefix));
            if !is_preferred && interface.ipv4.is_empty() {
                    continue;
            }

            let mut scan_targets = Vec::new();
            for ip_config in &interface.ipv4 {
                let ip_addr = ip_config.addr;
                let prefix_len = ip_config.prefix_len;
                let host_count = if prefix_len >= 30 { 4u32 } else if prefix_len >= 24 { 1u32 << (32 - prefix_len) } else { 256u32 };
                let network_addr = calculate_network_address(ip_addr, prefix_len);
                for i in 1..(host_count - 1) {
                    let host_ip = generate_ip_in_subnet(network_addr, i);
                    let host = netscan::host::Host::new(host_ip.into(), String::new()).with_ports(vec![PROXMOX_PORT]);
                    scan_targets.push(host);
                }
            }
            if scan_targets.is_empty() { continue; }

            let scan_setting = netscan::scan::setting::PortScanSetting::default()
                .set_if_index(interface.index)
                .set_scan_type(netscan::scan::setting::PortScanType::TcpConnectScan)
                .set_targets(scan_targets)
                .set_timeout(std::time::Duration::from_secs(5))
                .set_wait_time(std::time::Duration::from_millis(500));
            let scanner = netscan::scan::scanner::PortScanner::new(scan_setting);
            let scan_result = scanner.scan();
            for host in scan_result.hosts {
                if host.get_open_ports().iter().any(|p| p.number == PROXMOX_PORT) {
                        all_addresses.push(std::net::SocketAddr::new(host.ip_addr, PROXMOX_PORT));
                }
            }
        }
        Ok::<Vec<std::net::SocketAddr>, String>(all_addresses)
    }).await;

    match scan_result {
        Ok(Ok(addresses)) => {
            info!("Proxmox scan found {} potential machines", addresses.len());
            let machines: Vec<DiscoveredProxmox> = addresses
                .into_iter()
                .map(|socket_addr| {
                    let ip = socket_addr.ip();
                    let host = ip.to_string();
                    let hostname = match tokio::task::block_in_place(|| dns_lookup::lookup_addr(&ip).ok()) {
                        Some(name) if name != host => Some(name),
                        _ => None,
                    };
                    // Use the locally defined DiscoveredProxmox struct
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
            // Use the locally defined ProxmoxDiscoverResponse struct
            (StatusCode::OK, Json(ProxmoxDiscoverResponse { machines })).into_response()
        }
        Ok(Err(e)) => {
            error!("Proxmox discovery scan failed: {}", e);
            let error_message = format!("Network scan failed: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse { error: "Scan Error".to_string(), message: error_message }),
            )
                .into_response()
        }
        Err(e) => {
            error!("Proxmox discovery task failed: {}", e);
            let error_message = format!("Scanner task failed: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse { error: "Task Error".to_string(), message: error_message }),
            )
                .into_response()
        }
    }
}

// ========================
// Helper Functions (Restored)
// ========================

fn calculate_network_address(ip: Ipv4Addr, prefix_len: u8) -> Ipv4Addr {
    let ip_u32 = u32::from(ip);
    let mask = !((1u32 << (32 - prefix_len)) - 1);
    Ipv4Addr::from(ip_u32 & mask)
}

fn generate_ip_in_subnet(network_addr: Ipv4Addr, host_num: u32) -> Ipv4Addr {
    let network_u32 = u32::from(network_addr);
    Ipv4Addr::from(network_u32 + host_num)
}

// Start a background task to periodically prune machines that have been removed from Proxmox
pub async fn start_proxmox_sync_task(
    state: std::sync::Arc<crate::AppState>,
    mut shutdown_rx: tokio::sync::watch::Receiver<()>
) {
    use std::time::Duration;
    
    // Clone the state for the task
    let state_clone = state.clone();
    
    tokio::spawn(async move {
        let poll_interval = Duration::from_secs(90); // Check every 90 seconds
        info!("Starting Proxmox sync task with interval of {:?}", poll_interval);
        
        loop {
            tokio::select! {
                _ = tokio::time::sleep(poll_interval) => {
                    info!("Running Proxmox machine sync check");
                    
                    // Check if Proxmox settings are configured
                    let proxmox_configured = {
                        let settings = state_clone.settings.lock().await;
                        settings.proxmox_host.is_some() 
                            && settings.proxmox_username.is_some() 
                            && settings.proxmox_password.is_some()
                    };
                    
                    if !proxmox_configured {
                        info!("Proxmox not configured, skipping sync check");
                        continue;
                    }
                    
                    // Get all machines with Proxmox information
                    let machines = match db::get_all_machines().await {
                        Ok(m) => m,
                        Err(e) => {
                            error!("Failed to get machines for Proxmox sync: {}", e);
                            continue;
                        }
                    };
                    
                    // Filter out machines with Proxmox info
                    let proxmox_machines: Vec<_> = machines.into_iter()
                        .filter(|m| m.proxmox_vmid.is_some() || m.is_proxmox_host)
                        .collect();
                    
                    if proxmox_machines.is_empty() {
                        info!("No Proxmox machines found, skipping sync check");
                        continue;
                    }
                    
                    // Connect to Proxmox and get current machine list
                    match connect_to_proxmox(&state_clone).await {
                        Ok(client) => {
                            // Process each cluster and its machines
                            if let Err(e) = sync_proxmox_machines(&client, &proxmox_machines).await {
                                error!("Error during Proxmox sync check: {}", e);
                            }
                        },
                        Err(e) => {
                            error!("Failed to connect to Proxmox for sync check: {}", e);
                        }
                    }
                }
                _ = shutdown_rx.changed() => {
                    info!("Shutdown signal received, stopping Proxmox sync task");
                    break;
                }
            }
        }
    });
}

// Connect to Proxmox using the saved credentials
async fn connect_to_proxmox(state: &crate::AppState) -> Result<ProxmoxApiClient, anyhow::Error> {
    // Get Proxmox settings
    let (host, username, password, port, _skip_tls_verify) = {
        let settings = state.settings.lock().await;
        let host = settings.proxmox_host.clone().ok_or_else(|| anyhow::anyhow!("Proxmox host not configured"))?;
        let username = settings.proxmox_username.clone().ok_or_else(|| anyhow::anyhow!("Proxmox username not configured"))?;
        let password = settings.proxmox_password.clone().ok_or_else(|| anyhow::anyhow!("Proxmox password not configured"))?;
        let port = settings.proxmox_port.unwrap_or(8006);
        let skip_tls_verify = settings.proxmox_skip_tls_verify.unwrap_or(false);
        (host, username, password, port, skip_tls_verify)
    };
    
    // Parse username@realm format if present
    let (username_part, realm) = if let Some(idx) = username.find('@') {
        let (username_part, realm_part) = username.split_at(idx);
        // Remove the @ from the beginning of realm
        (username_part.to_string(), Some(realm_part[1..].to_string()))
    } else {
        (username.clone(), Some("pam".to_string()))
    };
    
    // Create HTTPS connector with appropriate TLS settings
    let _https = HttpsConnector::new();
    
    // Use just the host URL for client initialization
    let host_url = format!("https://{}:{}", host, port);
    let base_uri: hyper::Uri = host_url.parse::<hyper::Uri>()
        .map_err(|e| anyhow::anyhow!("Invalid Proxmox URL: {}", e))?;
    
    // Initialize the Proxmox client
    let client = ProxmoxApiClient::new(base_uri.clone());
    
    // Create the login object for authentication
    let login_user = match &realm {
        Some(r) => format!("{}@{}", username_part, r),
        None => username_part,
    };
    
    let login_builder = proxmox_login::Login::new(&host_url, login_user, password);
    
    // Authenticate
    match client.login(login_builder).await {
        Ok(None) => {
            info!("Successfully authenticated with Proxmox API for sync task");
            Ok(client)
        },
        Ok(Some(_)) => {
            Err(anyhow::anyhow!("Two-factor authentication is required but not supported"))
        },
        Err(e) => {
            Err(anyhow::anyhow!("Failed to login to Proxmox: {}", e))
        }
    }
}

// NEW function to handle both updates and pruning
async fn sync_proxmox_machines(
    client: &ProxmoxApiClient,
    db_machines: &[dragonfly_common::models::Machine]
) -> Result<(), anyhow::Error> {
    info!("Starting Proxmox machine synchronization...");

    // Get current nodes from Proxmox
    let nodes_response = client.get("/api2/json/nodes").await
        .map_err(|e| anyhow::anyhow!("Sync: Failed to fetch nodes: {}", e))?;
    let nodes_value: serde_json::Value = serde_json::from_slice(&nodes_response.body)
        .map_err(|e| anyhow::anyhow!("Sync: Failed to parse nodes response: {}", e))?;
    let nodes_data = nodes_value.get("data")
        .and_then(|d| d.as_array())
        .ok_or_else(|| anyhow::anyhow!("Sync: Invalid nodes response format"))?;

    // Build sets of existing nodes and VMs from Proxmox API
    let mut existing_node_names = std::collections::HashSet::new();
    let mut existing_vm_ids = std::collections::HashSet::new();
    let mut current_vm_details = std::collections::HashMap::new(); // Store {vmid: (node_name, status, agent_running, config_data)}

    for node in nodes_data {
        let node_name = node.get("node")
            .and_then(|n| n.as_str())
            .ok_or_else(|| anyhow::anyhow!("Sync: Node missing 'node' field"))?;
        
        existing_node_names.insert(node_name.to_string());

        // Get VMs for this node
        let vms_path = format!("/api2/json/nodes/{}/qemu", node_name);
        match client.get(&vms_path).await {
            Ok(vms_response) => {
                match serde_json::from_slice::<serde_json::Value>(&vms_response.body) {
                    Ok(vms_value) => {
                        if let Some(vms_data) = vms_value.get("data").and_then(|d| d.as_array()) {
                            for vm in vms_data {
                                if let Some(vmid) = vm.get("vmid").and_then(|id| id.as_u64()).map(|id| id as u32) {
                                    existing_vm_ids.insert(vmid);
                                    let status = vm.get("status").and_then(|s| s.as_str()).unwrap_or("unknown").to_string();
                                    
                                    // Get config to check agent enablement
                                    let vm_config_path = format!("/api2/json/nodes/{}/qemu/{}/config", node_name, vmid);
                                    let agent_enabled = match client.get(&vm_config_path).await {
                                        Ok(cfg_resp) => {
                                            match serde_json::from_slice::<serde_json::Value>(&cfg_resp.body) {
                                                Ok(cfg_val) => {
                                                    if let Some(agent_str) = cfg_val.get("data").and_then(|d| d.get("agent")).and_then(|a| a.as_str()) {
                                                        agent_str.contains("enabled=1") || agent_str.contains("enabled=true")
                                                    } else { false }
                                                }, 
                                                Err(_) => false
                                            }
                                        }, 
                                        Err(_) => false
                                    };

                                    let mut agent_running = false;
                                    if status == "running" && agent_enabled {
                                        let agent_ping_path = format!("/api2/json/nodes/{}/qemu/{}/agent/ping", node_name, vmid);
                                        agent_running = match client.get(&agent_ping_path).await {
                                            Ok(ping_resp) => serde_json::from_slice::<serde_json::Value>(&ping_resp.body)
                                                .map_or(false, |v| v.get("data").is_some() && !v.get("data").and_then(|d| d.get("error")).is_some()),
                                            Err(_) => false
                                        };
                                    }
                                    
                                    current_vm_details.insert(vmid, (node_name.to_string(), status, agent_running));
                                }
                            }
                        } else {
                            warn!("Sync: Invalid VMs data format for node {}", node_name);
                        }
                    },
                    Err(e) => warn!("Sync: Failed to parse VMs response for node {}: {}", node_name, e),
                }
            },
            Err(e) => warn!("Sync: Failed to get VMs for node {}: {}", node_name, e),
        }
    }

    info!("Sync: Found {} nodes and {} VMs in Proxmox API", existing_node_names.len(), existing_vm_ids.len());

    // Iterate through machines stored in Dragonfly DB
    let mut pruned_count = 0;
    let mut updated_ip_count = 0;
    let mut updated_status_count = 0;

    for machine in db_machines {
        let machine_id_str = machine.id.to_string(); // For logging
        
        if let Some(vmid) = machine.proxmox_vmid {
            // --- Handle VMs ---
            if !existing_vm_ids.contains(&vmid) {
                // Prune VM
                info!(machine_id = %machine_id_str, vmid = vmid, "Sync: Proxmox VM no longer exists, removing from DB");
                if let Err(e) = db::delete_machine(&machine.id).await {
                    error!(machine_id = %machine_id_str, error = %e, "Sync: Failed to delete machine");
                } else {
                    pruned_count += 1;
                }
                continue; // Move to the next machine in DB
            }

            // VM exists, check for updates
            if let Some((node_name, current_status, agent_running)) = current_vm_details.get(&vmid) {
                // 1. Update Status if changed
                let new_db_status = match current_status.as_str() {
                    "running" => MachineStatus::Ready,
                    "stopped" => MachineStatus::Offline,
                    _ => MachineStatus::ExistingOS, // Or another appropriate status?
                };
                if machine.status != new_db_status {
                    info!(machine_id = %machine_id_str, vmid = vmid, old_status = ?machine.status, new_status = ?new_db_status, "Sync: Updating machine status");
                    if let Err(e) = db::update_status(&machine.id, new_db_status).await {
                        error!(machine_id = %machine_id_str, error = %e, "Sync: Failed to update machine status");
                    } else {
                        updated_status_count += 1;
                    }
                }

                // 2. Update IP if running and agent is available
                if current_status == "running" && *agent_running {
                    let agent_path = format!("/api2/json/nodes/{}/qemu/{}/agent/network-get-interfaces", node_name, vmid);
                    match client.get(&agent_path).await {
                        Ok(agent_response) => {
                            match serde_json::from_slice::<serde_json::Value>(&agent_response.body) {
                                Ok(agent_value) => {
                                    if let Some(result) = agent_value.get("data").and_then(|d| d.get("result")) {
                                        if let Some(interfaces) = result.as_array() {
                                            let mut current_ip: Option<String> = None;
                                            // Use the same two-pass logic to find the IP
                                            let mut preferred_ip: Option<String> = None;
                                            for iface in interfaces {
                                                if let Some(name) = iface.get("name").and_then(|n| n.as_str()) {
                                                    if name.starts_with("eth") || name.starts_with("ens") || name.starts_with("eno") {
                                                        if let Some(ip) = find_valid_ipv4_in_interface(iface, vmid) {
                                                            preferred_ip = Some(ip);
                                                            break;
                                                        }
                                                    }
                                                }
                                            }
                                            if preferred_ip.is_some() {
                                                current_ip = preferred_ip;
                                            } else {
                                                for iface in interfaces {
                                                    if let Some(name) = iface.get("name").and_then(|n| n.as_str()) {
                                                        if !(name.starts_with("lo") || name.starts_with("eth") || name.starts_with("ens") || name.starts_with("eno") || name.starts_with("tailscale") || name.starts_with("docker") || name.starts_with("veth") || name.starts_with("virbr") || name.starts_with("br-")) {
                                                             if let Some(ip) = find_valid_ipv4_in_interface(iface, vmid) {
                                                                current_ip = Some(ip);
                                                                break;
                                                            }
                                                        }
                                                    }
                                                }
                                            }

                                            // Compare with DB IP and update if needed
                                            if let Some(new_ip) = current_ip {
                                                if machine.ip_address != new_ip {
                                                    info!(machine_id = %machine_id_str, vmid = vmid, old_ip = %machine.ip_address, new_ip = %new_ip, "Sync: Updating machine IP address");
                                                    if let Err(e) = db::update_ip_address(&machine.id, &new_ip).await {
                                                        error!(machine_id = %machine_id_str, error = %e, "Sync: Failed to update IP address");
                                                    } else {
                                                        updated_ip_count += 1;
                                                    }
                                                }
                                            } else {
                                                // No valid IP found via agent, maybe update DB to Unknown if it wasn't already?
                                                if machine.ip_address != "Unknown" {
                                                    info!(machine_id = %machine_id_str, vmid = vmid, old_ip = %machine.ip_address, "Sync: No valid IP found via agent, setting IP to Unknown");
                                                    if let Err(e) = db::update_ip_address(&machine.id, "Unknown").await {
                                                         error!(machine_id = %machine_id_str, error = %e, "Sync: Failed to set IP address to Unknown");
                                                    }
                                                    // Don't increment updated_ip_count for setting to Unknown?
                                                }
                                            }
                                        }
                                    }
                                },
                                Err(e) => warn!(machine_id = %machine_id_str, vmid = vmid, error = %e, "Sync: Failed to parse agent network response"),
                            }
                        },
                        Err(e) => warn!(machine_id = %machine_id_str, vmid = vmid, error = %e, "Sync: Failed to get agent network interfaces"),
                    }
                }
            }

        } else if machine.is_proxmox_host {
            // --- Handle Hosts ---
            if let Some(node_name) = &machine.proxmox_node {
                if !existing_node_names.contains(node_name) {
                    // Prune Host
                    info!(machine_id = %machine_id_str, node = %node_name, "Sync: Proxmox node no longer exists, removing from DB");
                    if let Err(e) = db::delete_machine(&machine.id).await {
                        error!(machine_id = %machine_id_str, error = %e, "Sync: Failed to delete machine");
                    } else {
                        pruned_count += 1;
                    }
                }
                // TODO: Optionally update host details (IP, status?) if needed
            } else {
                warn!(machine_id = %machine_id_str, "Sync: Proxmox host machine is missing proxmox_node field");
            }
        }
    }

    info!(
        "Proxmox sync complete: {} machines pruned, {} IPs updated, {} statuses updated",
        pruned_count, updated_ip_count, updated_status_count
    );

    Ok(())
}

// ... (Keep other existing helpers like register_machine_with_id if still needed)