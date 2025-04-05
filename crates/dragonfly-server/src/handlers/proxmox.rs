use anyhow::{anyhow, Context, Result};
use axum::{
    extract::{Path as AxumPath, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use chrono::Utc;
use dragonfly_common::models::{Machine, MachineStatus, RegisterRequest};
use hyper::client::HttpConnector;
use hyper::StatusCode as HyperStatusCode;
use hyper::Uri;
use hyper_tls::HttpsConnector;
// Correct proxmox imports
use proxmox_client::{HttpApiClient, Client as ProxmoxApiClient};
use proxmox_http::client::Client as PxHttpClient;
use proxmox_http::Error as HttpError;
use proxmox_login::Authid;
use proxmox_login::Error as LoginError;
use proxmox_login::Login;
use serde::Serialize;
use sqlx::{Pool, Sqlite};
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use std::time::Duration;
use tracing::{error, info, warn};
use uuid::Uuid;

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

// Error types remain the same
#[derive(Debug, thiserror::Error)]
enum ProxmoxHandlerError {
    #[error("Proxmox API error: {0}")]
    ApiError(#[from] proxmox_client::Error),
    #[error("Database error: {0}")]
    DbError(#[from] sqlx::Error),
    #[error("Configuration error: {0}")]
    ConfigError(String),
    #[error("Internal error: {0}")]
    InternalError(#[from] anyhow::Error),
    #[error("Login error: {0}")]
    LoginError(#[from] LoginError),
    #[error("HTTP client error: {0}")]
    HttpClientError(#[from] HttpError),
}

// IntoResponse impl: Populate message field
impl IntoResponse for ProxmoxHandlerError {
    fn into_response(self) -> Response {
        let (status, error_message) = match self {
            ProxmoxHandlerError::ApiError(e) => {
                error!("Proxmox API Error: {}", e);
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Proxmox API interaction failed: {}", e),
                )
            }
            ProxmoxHandlerError::DbError(e) => {
                error!("Database Error: {}", e);
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Database operation failed: {}", e),
                )
            }
            ProxmoxHandlerError::ConfigError(msg) => {
                error!("Configuration Error: {}", msg);
                (StatusCode::BAD_REQUEST, msg)
            }
            ProxmoxHandlerError::InternalError(e) => {
                error!("Internal Server Error: {}", e);
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "An internal server error occurred.".to_string(),
                )
            }
            ProxmoxHandlerError::LoginError(e) => {
                error!("Proxmox Login Error: {}", e);
                (
                    StatusCode::UNAUTHORIZED,
                    format!("Proxmox authentication failed: {}", e),
                )
            }
            ProxmoxHandlerError::HttpClientError(e) => {
                error!("Proxmox HTTP Client Error: {}", e);
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Proxmox HTTP communication failed: {}", e),
                )
            }
        };
        // Add the message field
        (status, Json(ErrorResponse { error: status.canonical_reason().unwrap_or("Error").to_string(), message: error_message })).into_response()
    }
}

type ProxmoxResult<T> = std::result::Result<T, ProxmoxHandlerError>;

#[axum::debug_handler]
pub async fn connect_proxmox_handler(
    State(state): State<AppState>,
) -> ProxmoxResult<Json<String>> {
    info!("Connecting to Proxmox instance...");

    let host = state.settings.proxmox_host
        .as_ref()
        .ok_or_else(|| ProxmoxHandlerError::ConfigError("Proxmox host not configured".to_string()))?
        .clone();
    let username = state.settings.proxmox_username
        .as_ref()
        .ok_or_else(|| ProxmoxHandlerError::ConfigError("Proxmox username not configured".to_string()))?
        .clone();
    let password = state.settings.proxmox_password
        .as_ref()
        .ok_or_else(|| ProxmoxHandlerError::ConfigError("Proxmox password not configured".to_string()))?
        .clone();
    let port = state.settings.proxmox_port.unwrap_or(8006);

    let auth_id = Authid::new(&username, Some("pam"));

    let https = HttpsConnector::new();
    let hyper_client = hyper::Client::builder().build::<_, hyper::Body>(https);

    let base_uri_str = format!("https://{}:{}/", host, port);
    let base_uri: hyper::Uri = base_uri_str.parse::<hyper::Uri>().map_err(|e| {
        ProxmoxHandlerError::ConfigError(format!("Invalid Proxmox URL '{}': {}", base_uri_str, e))
    })?;

    let client = ProxmoxApiClient::new(base_uri);

    info!(
        "Attempting login to Proxmox at {}:{} with user {}",
        host, port, auth_id
    );

    let login_info = Login::new(auth_id, password.clone(), None);
    client.login(login_info).await?;

    info!("Successfully logged into Proxmox API.");

    let response = client
        .get("cluster/status")
        .await?;

    let cluster_status: serde_json::Value = response
        .json()
        .await
        .context("Failed to deserialize Proxmox cluster status")?;

    info!("Successfully fetched and parsed Proxmox cluster status.");

    let cluster_name = cluster_status["data"]
        .as_array()
        .and_then(|arr| arr.iter().find(|item| item["type"] == "cluster"))
        .and_then(|cluster_entry| cluster_entry["name"].as_str())
        .map(String::from)
        .ok_or_else(|| {
            warn!("Could not find \"cluster\" type entry in Proxmox cluster status response.");
            ProxmoxHandlerError::ApiError(proxmox_client::Error::Api(
                HyperStatusCode::INTERNAL_SERVER_ERROR,
                "Failed to parse cluster name from API response".to_string(),
            ))
        })?;

    info!("Proxmox cluster name: {}", cluster_name);

    discover_and_register_proxmox_vms(&client, &cluster_name)
        .await
        .context("Failed during Proxmox VM discovery and registration")?;

    info!(
        "Successfully connected to Proxmox cluster: {}",
        cluster_name
    );
    Ok(Json(format!(
        "Successfully connected to Proxmox cluster: {}",
        cluster_name
    )))
}

// Placeholder function definition
async fn discover_and_register_proxmox_vms(
    client: &ProxmoxApiClient,
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

fn calculate_network_address(ip: std::net::Ipv4Addr, prefix_len: u8) -> std::net::Ipv4Addr {
    let ip_u32 = u32::from(ip);
    let mask = !((1u32 << (32 - prefix_len)) - 1);
    std::net::Ipv4Addr::from(ip_u32 & mask)
}

fn generate_ip_in_subnet(network_addr: std::net::Ipv4Addr, host_num: u32) -> std::net::Ipv4Addr {
    let network_u32 = u32::from(network_addr);
    std::net::Ipv4Addr::from(network_u32 + host_num)
}

// ... (Keep other existing helpers like register_machine_with_id if still needed)