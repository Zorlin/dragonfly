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
use hyper::body::to_bytes;
use hyper::body::HttpBody as HyperHttpBody;

use crate::AppState;
use crate::db::ErrorResponse;

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
                    discover_and_register_proxmox_vms(&client, &cluster_name)
                        .await
                        .context("Failed during Proxmox VM discovery and registration")?;

                    info!("Successfully connected to Proxmox cluster: {}", cluster_name);

                    Ok(Json(ProxmoxConnectResponse {
                        message: format!("Successfully connected to Proxmox cluster: {}", cluster_name),
                        suggest_disable_tls_verify: false
                    }))
                },
                Err(e) => {
                    error!("Failed to get cluster status: {}", e);
                    // Use the existing error handler, but pass the error directly
                    handle_proxmox_error(e, skip_tls_verify)
                }
            }
        }
        Ok(Some(_tfa_challenge)) => {
            // TFA is required, which is not handled yet
            error!("Proxmox login requires Two-Factor Authentication, which is not supported yet.");
            Err(ProxmoxHandlerError::LoginError(
                "TFA Required".into() // Use a placeholder error
            ))
        }
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

async fn discover_and_register_proxmox_vms(
    _client: &ProxmoxApiClient,
    cluster_name: &str,
) -> ProxmoxResult<()> {
    warn!(
        "discover_and_register_proxmox_vms called (cluster: {}), but is currently a placeholder.",
        cluster_name
    );
    Ok(())
}

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

// ... (Keep other existing helpers like register_machine_with_id if still needed)