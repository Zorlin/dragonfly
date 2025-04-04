use axum::{
    routing::{get, post, delete, put},
    Router,
    extract::{
        State, Path, Json, Form, FromRequest,
        ConnectInfo,
    },
    http::{StatusCode, header::HeaderValue, HeaderMap},
    response::{IntoResponse, Html, Response, sse::{Event, Sse, KeepAlive}, Redirect},
};
use std::convert::Infallible;
use serde_json::json;
use uuid::Uuid;
use dragonfly_common::models::{MachineStatus, HostnameUpdateRequest, HostnameUpdateResponse, OsInstalledUpdateRequest, OsInstalledUpdateResponse, BmcType, BmcCredentials, StatusUpdateRequest, BmcCredentialsUpdateRequest, InstallationProgressUpdateRequest, RegisterRequest, Machine};
use crate::db::{self, RegisterResponse, ErrorResponse, OsAssignmentRequest, get_machine_tags, update_machine_tags as db_update_machine_tags};
use crate::AppState;
use crate::auth::AuthSession;
use std::collections::HashMap;
use tracing::{info, error, warn, debug};
use std::env;
use std::time::Duration;
use serde::Deserialize;
use tokio_stream::Stream;
use futures::stream;
use crate::{
    INSTALL_STATE_REF, 
    InstallationState
};
use std::sync::Arc;
use std::path::Path as FilePath;
use std::fs::File;
use tar::Archive;
use flate2::read::GzDecoder;
use tempfile::tempdir;
use std::os::unix::fs::symlink as unix_symlink;
use tokio::process::Command;
use tokio::fs;
use std::path::Path as StdPath;
use std::path::PathBuf;
use url::Url;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use axum::body::{Body, Bytes};
use http_body::Frame;
use http_body_util::{StreamBody, Empty};
use dragonfly_common::Error;
use tokio::io::{AsyncSeekExt, AsyncReadExt, AsyncWriteExt};
use futures::StreamExt; // For .next() on stream
use crate::ui; // Import the ui module
use axum::http::Request;
use std::net::SocketAddr;
use axum::middleware::Next; // Add this import back
use chrono::Utc;

pub fn api_router() -> Router<crate::AppState> {
    Router::new()
        .route("/machines", get(get_all_machines).post(register_machine))
        .route("/machines/install-status", get(get_install_status))
        .route("/machines/{id}/os", get(get_machine_os).post(assign_os))
        .route("/machines/{id}/hostname", get(get_hostname_form).put(update_hostname))
        .route("/machines/{id}/status", put(update_status))
        .route("/machines/{id}/status-and-progress", get(get_machine_status_and_progress_partial))
        .route("/machines/{id}/os-installed", put(update_os_installed))
        .route("/machines/{id}/bmc", post(update_bmc))
        .route("/machines/{id}/workflow-progress", get(get_workflow_progress))
        .route("/machines/{id}/tags", get(api_get_machine_tags).put(api_update_machine_tags))
        .route("/machines/{id}/tags/{tag}", delete(api_delete_machine_tag))
        .route("/machines/{id}", get(get_machine).put(update_machine).delete(delete_machine))
        .route("/installation/progress", put(update_installation_progress))
        .route("/events", get(machine_events))
        .route("/heartbeat", get(heartbeat))
}

// Content constants
const HOSTS_CONTENT: &str = r#"127.0.0.1 localhost
::1 localhost ip6-localhost ip6-loopback
fe00::0 ip6-localnet
ff00::0 ip6-mcastprefix
ff02::1 ip6-allnodes
ff02::2 ip6-allrouters
"#;

const HOSTNAME_CONTENT: &str = "localhost";
const APK_ARCH_CONTENT: &str = "x86_64"; // Assuming amd64/x86_64 for now
const LBU_LIST_CONTENT: &str = "+usr/local";
const REPOSITORIES_CONTENT: &str = r#"https://dl-cdn.alpinelinux.org/alpine/v3.21/main
https://dl-cdn.alpinelinux.org/alpine/v3.21/community
"#;
const WORLD_CONTENT: &str = r#"alpine-baselayout
alpine-conf
alpine-keys
alpine-release
apk-tools
busybox
libc-utils
kexec-tools
libgcc
wget
"#;

/// Generates the localhost.apkovl.tar.gz file needed by the Dragonfly Agent iPXE script.
pub async fn generate_agent_apkovl(
    target_apkovl_path: &StdPath,
    base_url: &str,
    agent_binary_url: &str,
) -> Result<(), dragonfly_common::Error> {
    info!("Generating agent APK overlay at: {:?}", target_apkovl_path);
    
    // 1. Create a temporary directory
    let temp_dir = tempdir()
        .map_err(|e| dragonfly_common::Error::Internal(format!("Failed to create temp directory for apkovl: {}", e)))?;
    let temp_path = temp_dir.path();
    info!("Building apkovl structure in: {:?}", temp_path);
    
    // 2. Create directory structure
    fs::create_dir_all(temp_path.join("etc/local.d")).await
        .map_err(|e| dragonfly_common::Error::Internal(format!("Failed to create dir etc/local.d: {}", e)))?;
    fs::create_dir_all(temp_path.join("etc/apk/protected_paths.d")).await
        .map_err(|e| dragonfly_common::Error::Internal(format!("Failed to create dir etc/apk/protected_paths.d: {}", e)))?;
    fs::create_dir_all(temp_path.join("etc/runlevels/default")).await
        .map_err(|e| dragonfly_common::Error::Internal(format!("Failed to create dir etc/runlevels/default: {}", e)))?;
    fs::create_dir_all(temp_path.join("usr/local/bin")).await
        .map_err(|e| dragonfly_common::Error::Internal(format!("Failed to create dir usr/local/bin: {}", e)))?;
    
    // 3. Write static files
    fs::write(temp_path.join("etc/hosts"), HOSTS_CONTENT).await
        .map_err(|e| dragonfly_common::Error::Internal(format!("Failed to write etc/hosts: {}", e)))?;
    fs::write(temp_path.join("etc/hostname"), HOSTNAME_CONTENT).await
        .map_err(|e| dragonfly_common::Error::Internal(format!("Failed to write etc/hostname: {}", e)))?;
    fs::write(temp_path.join("etc/apk/arch"), APK_ARCH_CONTENT).await
        .map_err(|e| dragonfly_common::Error::Internal(format!("Failed to write etc/apk/arch: {}", e)))?;
    fs::write(temp_path.join("etc/apk/protected_paths.d/lbu.list"), LBU_LIST_CONTENT).await
        .map_err(|e| dragonfly_common::Error::Internal(format!("Failed to write lbu.list: {}", e)))?;
    fs::write(temp_path.join("etc/apk/repositories"), REPOSITORIES_CONTENT).await
        .map_err(|e| dragonfly_common::Error::Internal(format!("Failed to write repositories: {}", e)))?;
    fs::write(temp_path.join("etc/apk/world"), WORLD_CONTENT).await
        .map_err(|e| dragonfly_common::Error::Internal(format!("Failed to write world: {}", e)))?;
    
    // Create empty mtab needed by Alpine init
    fs::write(temp_path.join("etc/mtab"), "").await
        .map_err(|e| dragonfly_common::Error::Internal(format!("Failed to write etc/mtab: {}", e)))?;
    
    // Create empty .default_boot_services
    fs::write(temp_path.join("etc/.default_boot_services"), "").await
        .map_err(|e| dragonfly_common::Error::Internal(format!("Failed to write .default_boot_services: {}", e)))?;
    
    // 4. Write dynamic dragonfly-agent.start script
    let start_script_path = temp_path.join("etc/local.d/dragonfly-agent.start");
    
    // Create script content with explicit newline characters
    let script_content = format!(
        "#!/bin/sh\n\
        # Start dragonfly-agent\n\
        /usr/local/bin/dragonfly-agent --server {} --setup\n\
        exit 0\n", 
        base_url
    );
    
    // Write the file
    fs::write(&start_script_path, script_content).await
        .map_err(|e| dragonfly_common::Error::Internal(format!("Failed to write start script: {}", e)))?;
    
    // Make it executable
    set_executable_permission(&start_script_path).await?;
    
    // 5. Create the symlink (Unchanged, uses std::os::unix)
    let link_target = "/etc/init.d/local";
    let link_path = temp_path.join("etc/runlevels/default/local");
    unix_symlink(link_target, &link_path)
        .map_err(|e| dragonfly_common::Error::Internal(
            format!("Failed to create symlink {:?} -> {}: {}", link_path, link_target, e)
        ))?;
    
    // 6. Download the agent binary
    let agent_binary_path = temp_path.join("usr/local/bin/dragonfly-agent");
    download_file(agent_binary_url, &agent_binary_path).await?;
    
    // Make it executable
    set_executable_permission(&agent_binary_path).await?;
    
    // 7. Create the tar.gz archive
    info!("Creating tarball: {:?}", target_apkovl_path);
    let output = Command::new("tar")
        .arg("-czf")
        .arg(target_apkovl_path)
        .arg("-C")
        .arg(temp_path)
        .arg(".")
        .output()
        .await
        .map_err(|e| dragonfly_common::Error::Internal(format!("Failed to execute tar command: {}", e)))?;
    
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(dragonfly_common::Error::Internal(format!("Tar command failed: {}", stderr)));
    }
    
    info!("Successfully generated apkovl: {:?}", target_apkovl_path);
    Ok(())
}

// Helper function to set executable permission (Unix specific)
async fn set_executable_permission(path: &StdPath) -> Result<(), dragonfly_common::Error> {
    use std::os::unix::fs::PermissionsExt;
    
    let metadata = fs::metadata(path).await
        .map_err(|e| dragonfly_common::Error::Internal(
            format!("Failed to get metadata for {:?}: {}", path, e)
        ))?;
    
    let mut perms = metadata.permissions();
    perms.set_mode(0o755); // rwxr-xr-x
    
    fs::set_permissions(path, perms).await
        .map_err(|e| dragonfly_common::Error::Internal(
            format!("Failed to set executable permission on {:?}: {}", path, e)
        ))
}

// Helper function to download a file from a URL
async fn download_file(url: &str, target_path: &StdPath) -> Result<(), dragonfly_common::Error> {
    info!("Downloading {} to {:?}", url, target_path);
    
    // Create a reqwest client
    let client = reqwest::Client::new();
    
    // Send GET request to download the file
    let response = client.get(url)
        .send()
        .await
        .map_err(|e| dragonfly_common::Error::Internal(
            format!("Failed to download file from {}: {}", url, e)
        ))?;
    
    // Check if the request was successful
    if !response.status().is_success() {
        return Err(dragonfly_common::Error::Internal(
            format!("Failed to download file from {}: HTTP status {}", url, response.status())
        ));
    }
    
    // Get the file content as bytes
    let bytes = response.bytes().await
        .map_err(|e| dragonfly_common::Error::Internal(
            format!("Failed to read response body from {}: {}", url, e)
        ))?;
    
    // Create the file and write the content
    fs::write(target_path, bytes).await
        .map_err(|e| dragonfly_common::Error::Internal(
            format!("Failed to write downloaded file to {:?}: {}", target_path, e)
        ))?;
    
    info!("Successfully downloaded {} to {:?}", url, target_path);
    Ok(())
}

#[axum::debug_handler]
async fn register_machine(
    State(state): State<AppState>,
    // Ensure the payload type is correct, matching the updated common struct
    Json(payload): Json<RegisterRequest>,
) -> Response {
    // Pass the full payload (including new hardware fields) to the db function
    info!("Registering machine with MAC: {}, CPU: {:?}, Cores: {:?}, RAM: {:?}", 
          payload.mac_address, payload.cpu_model, payload.cpu_cores, payload.total_ram_bytes);
    
    match db::register_machine(&payload).await {
        Ok(machine_id) => {
            // Get the new machine to register with Tinkerbell
            if let Ok(Some(machine)) = db::get_machine_by_id(&machine_id).await {
                // Register with Tinkerbell (don't fail if this fails)
                if let Err(e) = crate::tinkerbell::register_machine(&machine).await {
                    warn!("Failed to register machine with Tinkerbell (continuing anyway): {}", e);
                }
            }
            
            // Emit machine discovered event
            let _ = state.event_manager.send(format!("machine_discovered:{}", machine_id));
            
            let response = RegisterResponse {
                machine_id,
                next_step: "awaiting_os_assignment".to_string(),
            };
            (StatusCode::CREATED, Json(response)).into_response()
        },
        Err(e) => {
            error!("Failed to register machine: {}", e);
            let error_response = ErrorResponse {
                error: "Registration Failed".to_string(),
                message: e.to_string(),
            };
            (StatusCode::INTERNAL_SERVER_ERROR, Json(error_response)).into_response()
        }
    }
}

#[axum::debug_handler]
async fn get_all_machines(
    auth_session: AuthSession,
    req: axum::http::Request<axum::body::Body>
) -> Response {
    // Check if this is an HTMX request
    let is_htmx = req.headers()
        .get("HX-Request")
        .is_some();
    
    // Check if user is authenticated as admin
    let is_admin = auth_session.user.is_some();

    match db::get_all_machines().await {
        Ok(machines) => {
            // Get workflow info for machines that are installing OS
            let mut workflow_infos = HashMap::new();
            for machine in &machines {
                if machine.status == MachineStatus::InstallingOS {
                    if let Ok(Some(info)) = crate::tinkerbell::get_workflow_info(machine).await {
                        workflow_infos.insert(machine.id, info);
                    }
                }
            }

            if is_htmx {
                // For HTMX requests, return HTML table rows
                if machines.is_empty() {
                    Html(r#"<tr>
                        <td colspan="6" class="px-6 py-8 text-center text-gray-500 italic">
                            No machines added or discovered yet.
                        </td>
                    </tr>"#).into_response()
                } else {
                    // Return HTML rows for each machine
                    let mut html = String::new();
                    for machine in machines {
                        let id_string = machine.id.to_string();
                        let display_name = machine.hostname.as_ref()
                            .or(machine.memorable_name.as_ref())
                            .map(|s| s.as_str())
                            .unwrap_or(&id_string);
                        
                        let secondary_name = if machine.hostname.is_some() && machine.memorable_name.is_some() {
                            machine.memorable_name.as_ref().map(|s| s.as_str()).unwrap_or("")
                        } else {
                            ""
                        };

                        let os_display = match &machine.os_installed {
                            Some(os) => os.clone(),
                            None => {
                                if machine.status == MachineStatus::InstallingOS {
                                    if let Some(os) = &machine.os_choice {
                                        format!("🚧 {}", format_os_name(os))
                                    } else {
                                        "🚀 Installing OS".to_string()
                                    }
                                } else if let Some(os) = &machine.os_choice {
                                    os.clone()
                                } else {
                                    "None".to_string()
                                }
                            }
                        };
                        
                        // Admin-only buttons (Assign OS, Update Status, Delete)
                        let admin_buttons = if is_admin {
                            format!(r#"
                                {}
                                <button
                                    @click="showStatusModal('{}')"
                                    class="px-3 py-1 inline-flex text-sm leading-5 font-semibold rounded-full bg-blue-500 text-white hover:bg-blue-600"
                                >
                                    Update Status
                                </button>
                                <button
                                    @click="showDeleteModal('{}')"
                                    class="text-red-600 hover:text-red-900"
                                >
                                    <svg xmlns="http://www.w3.org/2000/svg" fill="none" viewBox="0 0 24 24" stroke-width="1.5" stroke="currentColor" class="w-5 h-5">
                                        <path stroke-linecap="round" stroke-linejoin="round" d="M9.75 9.75l4.5 4.5m0-4.5l-4.5 4.5M21 12a9 9 0 11-18 0 9 9 0 0118 0z" />
                                    </svg>
                                </button>
                            "#,
                            // Conditionally include the Assign OS button
                            if machine.status == MachineStatus::AwaitingAssignment {
                                format!(r#"
                                    <button
                                        @click="showOsModal('{}')"
                                        class="px-3 py-1 inline-flex text-sm leading-5 font-semibold rounded-full bg-indigo-600 text-white hover:bg-indigo-700 cursor-pointer"
                                    >
                                        Assign OS
                                    </button>
                                "#, machine.id)
                            } else {
                                String::new()
                            },
                            machine.id,
                            machine.id
                            )
                        } else {
                            // Empty string when not admin
                            String::new()
                        };
                        
                        html.push_str(&format!(r#"
                            <tr class="hover:bg-gray-50 dark:hover:bg-gradient-to-r dark:hover:from-gray-800 dark:hover:to-gray-900 dark:hover:bg-opacity-50 dark:hover:backdrop-blur-sm transition-colors duration-150 cursor-pointer" @click="window.location='/machines/{}'">
                                <td class="px-6 py-4 whitespace-nowrap">
                                    <div class="text-sm font-medium text-gray-900">
                                        {}
                                    </div>
                                    <div class="text-xs text-gray-500">
                                        {}
                                    </div>
                                </td>
                                <td class="px-6 py-4 whitespace-nowrap">
                                    <div class="text-sm text-gray-500 tech-mono">{}</div>
                                </td>
                                <td class="px-6 py-4 whitespace-nowrap">
                                    <div class="text-sm text-gray-500 tech-mono">{}</div>
                                </td>
                                <td class="px-6 py-4 whitespace-nowrap">
                                    <span class="px-2 inline-flex text-xs leading-5 font-semibold rounded-full {}">
                                        {}
                                    </span>
                                </td>
                                <td class="px-6 py-4 whitespace-nowrap">
                                    <div class="text-sm text-gray-500">
                                        {}
                                    </div>
                                </td>
                                <td class="px-6 py-4 whitespace-nowrap text-sm font-medium">
                                    <div class="flex space-x-3" @click.stop>
                                        {}
                                    </div>
                                </td>
                            </tr>
                        "#,
                        machine.id,
                        display_name,
                        secondary_name,
                        machine.mac_address,
                        machine.ip_address,
                        match machine.status {
                            MachineStatus::Ready => "px-3 py-1 inline-flex text-sm leading-5 font-semibold rounded-full bg-green-100 text-green-800 dark:bg-green-400/10 dark:text-green-300 dark:border dark:border-green-500/20",
                            MachineStatus::InstallingOS => "px-3 py-1 inline-flex text-sm leading-5 font-semibold rounded-full bg-yellow-100 text-yellow-800 dark:bg-yellow-400/10 dark:text-yellow-300 dark:border dark:border-yellow-500/20",
                            MachineStatus::AwaitingAssignment => "px-3 py-1 inline-flex text-sm leading-5 font-semibold rounded-full bg-blue-100 text-blue-800 dark:bg-blue-400/10 dark:text-blue-300 dark:border dark:border-blue-500/20",
                            MachineStatus::ExistingOS => "px-3 py-1 inline-flex text-sm leading-5 font-semibold rounded-full bg-sky-100 text-sky-800 dark:bg-sky-400/10 dark:text-sky-300 dark:border dark:border-sky-500/20",
                            _ => "px-3 py-1 inline-flex text-sm leading-5 font-semibold rounded-full bg-red-100 text-red-800 dark:bg-red-400/10 dark:text-red-300 dark:border dark:border-red-500/20"
                        },
                        match &machine.status { 
                            MachineStatus::Ready => String::from("Ready for Adoption"),
                            MachineStatus::InstallingOS => String::from("Installing OS"),
                            MachineStatus::AwaitingAssignment => String::from("Awaiting Assignment"),
                            _ => machine.status.to_string()
                        },
                        os_display,
                        admin_buttons
                        ));
                    }
                    Html(html).into_response()
                }
            } else {
                // For non-HTMX requests, return JSON (already includes new fields via db query)
                (StatusCode::OK, Json(machines)).into_response()
            }
        },
        Err(e) => {
            error!("Failed to retrieve machines: {}", e);
            let error_response = ErrorResponse {
                error: "Database Error".to_string(),
                message: e.to_string(),
            };
            (StatusCode::INTERNAL_SERVER_ERROR, Json(error_response)).into_response()
        }
    }
}

#[axum::debug_handler]
async fn get_machine(
    Path(id): Path<Uuid>,
) -> Response {
    match db::get_machine_by_id(&id).await {
        Ok(Some(machine)) => { // machine now includes hardware fields from db query
            // Fetch workflow info if the machine is installing OS
            let workflow_info = if machine.status == MachineStatus::InstallingOS {
                match crate::tinkerbell::get_workflow_info(&machine).await {
                    Ok(info_opt) => info_opt, // This could be Some(info) or None
                    Err(e) => {
                        warn!("Failed to get workflow info for machine {} in get_machine: {}", id, e);
                        None // Treat error as no workflow info
                    }
                }
            } else {
                None // Not installing OS, no workflow info
            };

            // Create the wrapped JSON response (already includes hardware fields)
            let response_data = json!({
                "machine": machine,
                "workflow_info": workflow_info, 
            });

            (StatusCode::OK, Json(response_data)).into_response()
        },
        Ok(None) => {
            let error_response = ErrorResponse {
                error: "Not Found".to_string(),
                message: format!("Machine with ID {} not found", id),
            };
            (StatusCode::NOT_FOUND, Json(error_response)).into_response()
        },
        Err(e) => {
            error!("Failed to retrieve machine {}: {}", id, e);
            let error_response = ErrorResponse {
                error: "Database Error".to_string(),
                message: e.to_string(),
            };
            (StatusCode::INTERNAL_SERVER_ERROR, Json(error_response)).into_response()
        }
    }
}

// Combined OS assignment handler
#[axum::debug_handler]
async fn assign_os(
    auth_session: AuthSession,
    Path(id): Path<Uuid>,
    req: axum::http::Request<axum::body::Body>,
) -> Response {
    // Check if user is authenticated as admin
    if auth_session.user.is_none() {
        return (StatusCode::UNAUTHORIZED, Json(json!({
            "error": "Unauthorized",
            "message": "Admin authentication required for this operation"
        }))).into_response();
    }

    // Check content type to determine how to extract the OS choice
    let content_type = req.headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    
    info!("Content-Type received: {}", content_type);
    
    let os_choice = if content_type.starts_with("application/json") {
        // Extract JSON
        match axum::Json::<OsAssignmentRequest>::from_request(req, &()).await {
            Ok(Json(payload)) => Some(payload.os_choice),
            Err(e) => {
                error!("Failed to parse JSON request: {}", e);
                None
            }
        }
    } else if content_type.starts_with("application/x-www-form-urlencoded") {
        // Extract form data
        match axum::Form::<OsAssignmentRequest>::from_request(req, &()).await {
            Ok(Form(payload)) => Some(payload.os_choice),
            Err(e) => {
                error!("Failed to parse form request: {}", e);
                None
            }
        }
    } else {
        error!("Unsupported content type: {}", content_type);
        None
    };
    
    match os_choice {
        Some(os_choice) => assign_os_internal(id, os_choice).await,
        None => {
            let error_response = ErrorResponse {
                error: "Bad Request".to_string(),
                message: "Failed to extract OS choice from request".to_string(),
            };
            (StatusCode::BAD_REQUEST, Json(error_response)).into_response()
        }
    }
}

// Shared implementation
async fn assign_os_internal(id: Uuid, os_choice: String) -> Response {
    info!("Assigning OS {} to machine {}", os_choice, id);
    
    match db::assign_os(&id, &os_choice).await {
        Ok(true) => {
            // Get the machine to create a workflow for OS installation
            let machine_name = if let Ok(Some(machine)) = db::get_machine_by_id(&id).await {
                // Create a workflow for OS installation
                let workflow_result = crate::tinkerbell::create_workflow(&machine, &os_choice).await;
                
                if let Err(e) = workflow_result {
                    // Improved error handling with more specific error message
                    error!("Failed to create Tinkerbell workflow: {}", e);
                    
                    // Check if this is a template not found error
                    if e.to_string().contains("Template") && e.to_string().contains("not found") {
                        // Return an HTML error message specifically for template not found
                        let template_name = machine.os_choice.as_ref().unwrap_or(&os_choice);
                        let error_html = format!(r###"
                            <div class="p-4 mb-4 text-sm text-red-700 bg-red-100 rounded-lg" role="alert">
                                <span class="font-medium">Error!</span> Template for OS "{}" not found in Tinkerbell. 
                                <p class="mt-2">The OS choice was saved, but you will need to create the missing Tinkerbell template 
                                before the installation can proceed.</p>
                            </div>
                        "###, template_name);
                        return (StatusCode::INTERNAL_SERVER_ERROR, [(axum::http::header::CONTENT_TYPE, "text/html")], error_html).into_response();
                    }
                    
                    warn!("Failed to create Tinkerbell workflow (continuing anyway): {}", e);
                } else {
                    info!("Created Tinkerbell workflow for OS installation for machine {}", id);
                }
                
                // Get a user-friendly name for the machine
                if let Some(hostname) = &machine.hostname {
                    hostname.clone()
                } else if let Some(memorable_name) = &machine.memorable_name {
                    memorable_name.clone()
                } else {
                    id.to_string()
                }
            } else {
                warn!("Machine {} not found after assigning OS, couldn't create workflow", id);
                id.to_string()
            };
            
            // Return HTML with a toast notification
            let html = format!(r###"
                <div class="flex flex-col items-center justify-center p-8">
                    <div class="rounded-full bg-green-100 p-3 mb-4">
                        <svg class="h-8 w-8 text-green-600" xmlns="http://www.w3.org/2000/svg" fill="none" viewBox="0 0 24 24" stroke-width="1.5" stroke="currentColor">
                            <path stroke-linecap="round" stroke-linejoin="round" d="M9 12.75L11.25 15 15 9.75M21 12a9 9 0 11-18 0 9 9 0 0118 0z" />
                        </svg>
                    </div>
                    <h3 class="text-lg font-medium text-gray-900">Success!</h3>
                    <p class="mt-2 text-sm text-gray-500">{} has been assigned to {}</p>
                    <p class="mt-1 text-sm text-gray-500">A Tinkerbell workflow is being created to install the OS.</p>
                    <button 
                        type="button" 
                        class="mt-6 inline-flex justify-center rounded-md bg-indigo-600 px-3 py-2 text-sm font-semibold text-white shadow-sm hover:bg-indigo-500"
                        hx-get="/machines"
                        hx-target="body"
                        hx-swap="outerHTML"
                        onclick="document.getElementById('os-modal').classList.add('hidden');">
                        Close
                    </button>
                </div>
                
                <script>
                    // Create toast notification
                    const toast = document.createElement('div');
                    toast.innerHTML = `
                        <div class="fixed bottom-4 right-4 bg-white shadow-lg rounded-lg p-4 max-w-md transform transition-transform duration-300 ease-in-out z-50 flex items-start">
                            <div class="flex-shrink-0 mr-3">
                                <svg class="h-6 w-6 text-green-500" xmlns="http://www.w3.org/2000/svg" fill="none" viewBox="0 0 24 24" stroke="currentColor">
                                    <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M9 12l2 2 4-4m6 2a9 9 0 11-18 0 9 9 0 0118 0z" />
                                </svg>
                            </div>
                            <div>
                                <h3 class="font-medium text-gray-900">Success!</h3>
                                <p class="mt-1 text-sm text-gray-500">{} has been assigned to {}</p>
                            </div>
                        </div>
                    `;
                    document.body.appendChild(toast.firstElementChild);
                    
                    // Auto remove after 5 seconds
                    setTimeout(() => {{
                        const toastEl = document.querySelector('.fixed.bottom-4.right-4');
                        if (toastEl) {{
                            toastEl.classList.add('translate-y-full', 'opacity-0');
                            setTimeout(() => toastEl.remove(), 300);
                        }}
                    }}, 5000);
                    
                    // Use HTMX to refresh the table body
                    htmx.trigger(document.querySelector('tbody'), 'refresh');
                </script>
            "###, os_choice, machine_name, os_choice, machine_name);
            
            (StatusCode::OK, [(axum::http::header::CONTENT_TYPE, "text/html")], html).into_response()
        },
        Ok(false) => {
            let error_html = format!(r###"
                <div class="p-4 mb-4 text-sm text-red-700 bg-red-100 rounded-lg" role="alert">
                    <span class="font-medium">Error!</span> Machine with ID {} not found.
                </div>
            "###, id);
            (StatusCode::NOT_FOUND, [(axum::http::header::CONTENT_TYPE, "text/html")], error_html).into_response()
        },
        Err(e) => {
            error!("Failed to assign OS to machine {}: {}", id, e);
            let error_html = format!(r###"
                <div class="p-4 mb-4 text-sm text-red-700 bg-red-100 rounded-lg" role="alert">
                    <span class="font-medium">Error!</span> Database error: {}.
                </div>
            "###, e);
            (StatusCode::INTERNAL_SERVER_ERROR, [(axum::http::header::CONTENT_TYPE, "text/html")], error_html).into_response()
        }
    }
}

#[axum::debug_handler]
async fn update_status(
    State(state): State<AppState>,
    _auth_session: AuthSession,
    Path(id): Path<Uuid>,
    req: axum::http::Request<axum::body::Body>,
) -> Response {
    // Check content type to determine how to extract the status
    let content_type = req.headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    
    info!("Content-Type received: {}", content_type);
    
    let status = if content_type.starts_with("application/json") {
        // Extract JSON
        match axum::Json::<StatusUpdateRequest>::from_request(req, &()).await {
            Ok(Json(payload)) => Some(payload.status),
            Err(e) => {
                error!("Failed to parse JSON request: {}", e);
                None
            }
        }
    } else {
        // Extract form data
        match axum::Form::<std::collections::HashMap<String, String>>::from_request(req, &()).await {
            Ok(form) => {
                match form.0.get("status") {
                    Some(status_str) => {
                        match status_str.as_str() {
                            "Ready" => Some(MachineStatus::Ready),
                            "AwaitingAssignment" => Some(MachineStatus::AwaitingAssignment),
                            "InstallingOS" => Some(MachineStatus::InstallingOS),
                            "Error" => Some(MachineStatus::Error("Manual error state".to_string())),
                            _ => None
                        }
                    },
                    None => None
                }
            },
            Err(e) => {
                error!("Failed to parse form data: {}", e);
                None
            }
        }
    };

    let status = match status {
        Some(s) => s,
        None => {
            return Html(format!(r#"
                <div class="p-4 mb-4 text-sm text-red-700 bg-red-100 rounded-lg" role="alert">
                    <span class="font-medium">Error!</span> Invalid or missing status field.
                </div>
            "#)).into_response();
        }
    };

    info!("Updating status for machine {} to {:?}", id, status);
    
    match db::update_status(&id, status.clone()).await {
        Ok(true) => {
            // Get the updated machine to update Tinkerbell
            if let Ok(Some(machine)) = db::get_machine_by_id(&id).await {
                // Update the machine in Tinkerbell (don't fail if this fails)
                if let Err(e) = crate::tinkerbell::register_machine(&machine).await {
                    warn!("Failed to update machine in Tinkerbell (continuing anyway): {}", e);
                }
                
                // If the status is AwaitingAssignment, check if we should apply a default OS
                if status == MachineStatus::AwaitingAssignment {
                    // Check if a default OS is configured
                    if let Ok(settings) = db::get_app_settings().await {
                        if let Some(default_os) = settings.default_os {
                            info!("Applying default OS '{}' to newly registered machine {}", default_os, id);
                            // Assign the OS and trigger installation
                            if let Ok(true) = db::assign_os(&id, &default_os).await {
                                // Update Tinkerbell workflow
                                if let Ok(Some(updated_machine)) = db::get_machine_by_id(&id).await {
                                    if let Err(e) = crate::tinkerbell::create_workflow(&updated_machine, &default_os).await {
                                        warn!("Failed to create Tinkerbell workflow for default OS (continuing anyway): {}", e);
                                    } else {
                                        info!("Created Tinkerbell workflow for default OS installation");
                                    }
                                }
                            }
                        }
                    }
                }
            }
            
            // Emit machine updated event
            let _ = state.event_manager.send(format!("machine_updated:{}", id));
            
            // Return HTML success message
            Html(format!(r#"
                <div class="p-4 mb-4 text-sm text-green-700 bg-green-100 rounded-lg" role="alert">
                    <span class="font-medium">Success!</span> Machine status has been updated.
                </div>
                <script>
                    // Close the modal
                    statusModal = false;
                    // Refresh the machine list
                    htmx.trigger(document.querySelector('tbody'), 'refreshMachines');
                </script>
            "#)).into_response()
        },
        Ok(false) => {
            Html(format!(r#"
                <div class="p-4 mb-4 text-sm text-red-700 bg-red-100 rounded-lg" role="alert">
                    <span class="font-medium">Error!</span> Machine with ID {} not found.
                </div>
            "#, id)).into_response()
        },
        Err(e) => {
            error!("Failed to update status for machine {}: {}", id, e);
            Html(format!(r#"
                <div class="p-4 mb-4 text-sm text-red-700 bg-red-100 rounded-lg" role="alert">
                    <span class="font-medium">Error!</span> Database error: {}.
                </div>
            "#, e)).into_response()
        }
    }
}

#[axum::debug_handler]
async fn update_hostname(
    State(state): State<AppState>,
    auth_session: AuthSession,
    Path(id): Path<Uuid>,
    Json(payload): Json<HostnameUpdateRequest>,
) -> Response {
    // Check if user is authenticated as admin
    if auth_session.user.is_none() {
        return (StatusCode::UNAUTHORIZED, Json(json!({
            "error": "Unauthorized",
            "message": "Admin authentication required for this operation"
        }))).into_response();
    }

    info!("Updating hostname for machine {} to {}", id, payload.hostname);
    
    match db::update_hostname(&id, &payload.hostname).await {
        Ok(true) => {
            // Get the updated machine to update Tinkerbell
            if let Ok(Some(machine)) = db::get_machine_by_id(&id).await {
                // Update the machine in Tinkerbell (don't fail if this fails)
                if let Err(e) = crate::tinkerbell::register_machine(&machine).await {
                    warn!("Failed to update machine in Tinkerbell (continuing anyway): {}", e);
                }
            }
            
            // Emit machine updated event
            let _ = state.event_manager.send(format!("machine_updated:{}", id));
            
            let response = HostnameUpdateResponse {
                success: true,
                message: format!("Hostname updated for machine {}", id),
            };
            (StatusCode::OK, Json(response)).into_response()
        },
        Ok(false) => {
            let error_response = ErrorResponse {
                error: "Not Found".to_string(),
                message: format!("Machine with ID {} not found", id),
            };
            (StatusCode::NOT_FOUND, Json(error_response)).into_response()
        },
        Err(e) => {
            error!("Failed to update hostname for machine {}: {}", id, e);
            let error_response = ErrorResponse {
                error: "Database Error".to_string(),
                message: e.to_string(),
            };
            (StatusCode::INTERNAL_SERVER_ERROR, Json(error_response)).into_response()
        }
    }
}

#[axum::debug_handler]
async fn update_os_installed(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(payload): Json<OsInstalledUpdateRequest>,
) -> Response {
    info!("Updating OS installed for machine {} to {}", id, payload.os_installed);
    
    match db::update_os_installed(&id, &payload.os_installed).await {
        Ok(true) => {
            // Emit machine updated event
            let _ = state.event_manager.send(format!("machine_updated:{}", id));
            
            let response = OsInstalledUpdateResponse {
                success: true,
                message: format!("OS installed updated for machine {}", id),
            };
            (StatusCode::OK, Json(response)).into_response()
        },
        Ok(false) => {
            // Add a warning log here to confirm if this path is hit
            warn!("Machine with ID {} not found when attempting to update OS installed.", id);
            let error_response = ErrorResponse {
                error: "Not Found".to_string(),
                message: format!("Machine with ID {} not found", id),
            };
            (StatusCode::NOT_FOUND, Json(error_response)).into_response()
        },
        Err(e) => {
            error!("Failed to update OS installed for machine {}: {}", id, e);
            let error_response = ErrorResponse {
                error: "Database Error".to_string(),
                message: e.to_string(),
            };
            (StatusCode::INTERNAL_SERVER_ERROR, Json(error_response)).into_response()
        }
    }
}

#[axum::debug_handler]
async fn update_bmc(
    State(state): State<AppState>,
    auth_session: AuthSession,
    Path(id): Path<Uuid>,
    Form(payload): Form<BmcCredentialsUpdateRequest>,
) -> Response {
    // Check if user is authenticated as admin
    if auth_session.user.is_none() {
        return (StatusCode::UNAUTHORIZED, Json(json!({
            "error": "Unauthorized",
            "message": "Admin authentication required for this operation"
        }))).into_response();
    }

    info!("Updating BMC credentials for machine {}", id);
    
    // Create BMC credentials from the form data
    let bmc_type = match payload.bmc_type.as_str() {
        "IPMI" => BmcType::IPMI,
        "Redfish" => BmcType::Redfish,
        _ => BmcType::Other(payload.bmc_type.clone()), // Clone string
    };
    
    let credentials = BmcCredentials {
        address: payload.bmc_address,
        username: payload.bmc_username,
        password: Some(payload.bmc_password), // Assume password is provided
        bmc_type,
    };
    
    match db::update_bmc_credentials(&id, &credentials).await {
        Ok(true) => {
            // Emit machine updated event
            let _ = state.event_manager.send(format!("machine_updated:{}", id));
            
            (StatusCode::OK, Html(format!(r#"
                <div class="p-4 mb-4 text-sm text-green-700 bg-green-100 rounded-lg" role="alert">
                    <span class="font-medium">Success!</span> BMC credentials updated.
                </div>
                <script>
                    setTimeout(function() {{
                        window.location.reload();
                    }}, 1500);
                </script>
            "#))).into_response()
        },
        Ok(false) => {
            let error_message = format!("Machine with ID {} not found", id);
            (StatusCode::NOT_FOUND, Html(format!(r#"
                <div class="p-4 mb-4 text-sm text-red-700 bg-red-100 rounded-lg" role="alert">
                    <span class="font-medium">Error!</span> {}.
                </div>
            "#, error_message))).into_response()
        },
        Err(e) => {
            error!("Failed to update BMC credentials for machine {}: {}", id, e);
            let error_message = format!("Database error: {}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, Html(format!(r#"
                <div class="p-4 mb-4 text-sm text-red-700 bg-red-100 rounded-lg" role="alert">
                    <span class="font-medium">Error!</span> {}.
                </div>
            "#, error_message))).into_response()
        }
    }
}

// Handler to get the hostname edit form
#[axum::debug_handler]
async fn get_hostname_form(
    Path(id): Path<Uuid>,
) -> impl IntoResponse {
    match db::get_machine_by_id(&id).await {
        Ok(Some(machine)) => {
            let current_hostname = machine.hostname.unwrap_or_default();
            // Use raw string literals to avoid escaping issues
            let html = format!(
                r###"
                <div class="sm:flex sm:items-start">
                    <div class="mt-3 text-center sm:mt-0 sm:text-left w-full">
                        <h3 class="text-base font-semibold leading-6 text-gray-900">
                            Update Machine Hostname
                        </h3>
                        <div class="mt-2">
                            <form hx-post="/machines/{}/hostname" hx-target="#hostname-modal">
                                <label for="hostname" class="block text-sm font-medium text-gray-700">Hostname</label>
                                <input type="text" name="hostname" id="hostname" value="{}" class="mt-1 block w-full rounded-md border-gray-300 shadow-sm focus:border-indigo-500 focus:ring-indigo-500 sm:text-sm" placeholder="Enter hostname">
                                <div class="mt-5 sm:mt-4 sm:flex sm:flex-row-reverse">
                                    <button type="submit" class="inline-flex w-full justify-center rounded-md bg-indigo-600 px-3 py-2 text-sm font-semibold text-white shadow-sm hover:bg-indigo-500 sm:ml-3 sm:w-auto">
                                        Update
                                    </button>
                                    <button type="button" class="mt-3 inline-flex w-full justify-center rounded-md bg-white px-3 py-2 text-sm font-semibold text-gray-900 shadow-sm ring-1 ring-inset ring-gray-300 hover:bg-gray-50 sm:mt-0 sm:w-auto" onclick="document.getElementById('hostname-modal').classList.add('hidden')">
                                        Cancel
                                    </button>
                                </div>
                            </form>
                        </div>
                    </div>
                </div>
                "###,
                id, current_hostname
            );
            
            (StatusCode::OK, [(axum::http::header::CONTENT_TYPE, "text/html")], html)
        },
        Ok(None) => {
            let error_html = format!(
                r###"<div class="p-4 text-red-500">Machine with ID {} not found</div>"###,
                id
            );
            (StatusCode::NOT_FOUND, [(axum::http::header::CONTENT_TYPE, "text/html")], error_html)
        },
        Err(e) => {
            let error_html = format!(
                r###"<div class="p-4 text-red-500">Error: {}</div>"###,
                e
            );
            (StatusCode::INTERNAL_SERVER_ERROR, [(axum::http::header::CONTENT_TYPE, "text/html")], error_html)
        }
    }
}

// Handler for initial iPXE script generation (DHCP points here)
// Determines whether to chain to HookOS or the Dragonfly Agent
pub async fn ipxe_script(Path(mac): Path<String>) -> Response {
    if !mac.contains(':') || mac.split(':').count() != 6 {
        warn!("Received invalid MAC format in iPXE request: {}", mac);
        return (StatusCode::BAD_REQUEST, "Invalid MAC Address Format").into_response();
    }

    info!("Generating initial iPXE script for MAC: {}", mac);

    // Read required base URL from environment variable
    let base_url = match env::var("DRAGONFLY_BASE_URL") {
        Ok(url) => url,
        Err(_) => {
            error!("CRITICAL: DRAGONFLY_BASE_URL environment variable is not set. iPXE booting requires this configuration.");
            let error_response = ErrorResponse {
                error: "Configuration Error".to_string(),
                message: "Server is missing required DRAGONFLY_BASE_URL configuration.".to_string(),
            };
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(error_response)).into_response();
        }
    };

    match db::get_machine_by_mac(&mac).await {
        Ok(Some(_)) => {
            // Known machine: Chain to Dragonfly's OS installation hook script (hookos.ipxe)
            info!("Known MAC {}, chaining to HookOS script", mac);
            let script = format!("#!ipxe\nchain {}/ipxe/hookos.ipxe", base_url);
            (StatusCode::OK, [(axum::http::header::CONTENT_TYPE, "text/plain")], script).into_response()
        },
        Ok(None) => {
            // Unknown machine: Chain to the Dragonfly agent script
            info!("Unknown MAC {}, chaining to Dragonfly Agent iPXE script", mac);
            let script = format!("#!ipxe\nchain {}/ipxe/dragonfly-agent.ipxe", base_url);
            (StatusCode::OK, [(axum::http::header::CONTENT_TYPE, "text/plain")], script).into_response()
        },
        Err(e) => {
            error!("Database error while looking up MAC {}: {}", mac, e);
            let error_response = ErrorResponse {
                error: "Database Error".to_string(),
                message: e.to_string(),
            };
            (StatusCode::INTERNAL_SERVER_ERROR, Json(error_response)).into_response()
        }
    }
}

#[axum::debug_handler]
async fn delete_machine(
    State(state): State<AppState>,
    auth_session: AuthSession,
    Path(id): Path<Uuid>,
) -> Response {
    // Check if user is authenticated as admin
    if auth_session.user.is_none() {
        return (StatusCode::UNAUTHORIZED, Json(json!({
            "error": "Unauthorized",
            "message": "Admin authentication required for this operation"
        }))).into_response();
    }

    info!("Request to delete machine: {}", id);

    // Get the machine to find its MAC address
    match db::get_machine_by_id(&id).await {
        Ok(Some(machine)) => {
            // Delete from Tinkerbell
            let mac_address = machine.mac_address.replace(":", "-").to_lowercase();
            
            let tinkerbell_result = match crate::tinkerbell::delete_hardware(&mac_address).await {
                Ok(_) => {
                    info!("Successfully deleted machine from Tinkerbell: {}", mac_address);
                    true
                },
                Err(e) => {
                    warn!("Failed to delete machine from Tinkerbell: {}", e);
                    false
                }
            };

            // Delete from database
            match db::delete_machine(&id).await {
                Ok(true) => {
                    let message = if tinkerbell_result {
                        "Machine successfully deleted from Dragonfly and Tinkerbell."
                    } else {
                        "Machine deleted from Dragonfly but there was an issue removing it from Tinkerbell."
                    };
                    
                    // Emit machine deleted event
                    let _ = state.event_manager.send(format!("machine_deleted:{}", id));
                    
                    (StatusCode::OK, Json(json!({ "success": true, "message": message }))).into_response()
                },
                Ok(false) => {
                    (StatusCode::NOT_FOUND, Json(json!({ "error": "Machine not found in database" }))).into_response()
                },
                Err(e) => {
                    error!("Failed to delete machine from database: {}", e);
                    (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": format!("Database error: {}", e) }))).into_response()
                }
            }
        },
        Ok(None) => {
            (StatusCode::NOT_FOUND, Json(json!({ "error": "Machine not found" }))).into_response()
        },
        Err(e) => {
            error!("Error fetching machine for deletion: {}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": format!("Database error: {}", e) }))).into_response()
        }
    }
}

// Add this function to handle machine updates
#[axum::debug_handler]
async fn update_machine(
    State(state): State<AppState>,
    // Use AuthSession directly, not Option<AuthSession>
    auth_session: AuthSession,
    // Add ConnectInfo to get client IP
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Path(id): Path<Uuid>,
    Json(mut machine_payload): Json<Machine>,
) -> Response {
    let client_ip = addr.ip().to_string();
    info!("Update request for machine {} from IP: {}", id, client_ip);

    // Authorization Logic
    // Check if an admin user is logged in
    let is_admin = auth_session.user.is_some();

    let authorized = if is_admin {
        // Admin is always authorized
        info!("Admin user authorized update for machine {}", id);
        true
    } else {
        // Not an admin, check if it's the agent based on IP
        info!("Request is not from an admin, checking IP for agent authorization...");
        match db::get_machine_by_id(&id).await {
            Ok(Some(stored_machine)) => {
                if stored_machine.ip_address == client_ip {
                    info!("Agent IP {} matches stored IP for machine {}. Authorizing update.", client_ip, id);
                    true // IP matches, allow update
                } else {
                    warn!("Agent IP {} does NOT match stored IP {} for machine {}. Denying update.",
                          client_ip, stored_machine.ip_address, id);
                    false // IP mismatch
                }
            },
            Ok(None) => {
                warn!("Machine {} not found during IP authorization check.", id);
                false // Machine not found
                },
                Err(e) => {
                error!("Database error during IP authorization check for machine {}: {}", id, e);
                false // Database error
            }
        }
    };

    if !authorized {
        // Use 403 Forbidden for authorization failures
        // (axum-login middleware handles 401 for missing authentication if configured)
        return (StatusCode::FORBIDDEN, Json(json!({
            "error": "Forbidden",
            "message": "You are not authorized to update this machine."
        }))).into_response();
    }

    // --- Proceed with Update (if authorized) ---
    
    // Ensure the ID from the path matches the payload ID
    if machine_payload.id != id {
        return (StatusCode::BAD_REQUEST, Json(json!({
            "error": "ID Mismatch",
            "message": "The machine ID in the URL path does not match the ID in the request body."
        }))).into_response();
    }

    info!("Updating machine {} with full payload (Authorized by admin: {})", id, is_admin);
    
    // Set the updated_at timestamp before saving
    machine_payload.updated_at = Utc::now();

    // Call the updated db::update_machine function
    match db::update_machine(&machine_payload).await {
                Ok(true) => {
            // Emit machine updated event
            let _ = state.event_manager.send(format!("machine_updated:{}", id));
            
            // Return the updated machine object
            (StatusCode::OK, Json(machine_payload)).into_response()
                },
                Ok(false) => {
            // This case should ideally not happen if the ID check above passed
            // but handle it just in case (e.g., race condition with deletion)
            (StatusCode::NOT_FOUND, Json(json!({
                "error": "Not Found",
                "message": format!("Machine with ID {} not found during update attempt.", id)
            }))).into_response()
                },
                Err(e) => {
            error!("Failed to update machine {}: {}", id, e);
            (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({
                "error": "Database Error",
                "message": e.to_string()
            }))).into_response()
        }
    }
}

// Handler to get the OS assignment form
async fn get_machine_os(Path(id): Path<Uuid>) -> Response {
    Html(format!(r#"
        <div class="sm:flex sm:items-start">
            <div class="mt-3 text-center sm:mt-0 sm:text-left w-full">
                <h3 class="text-lg leading-6 font-medium text-gray-900">
                    Assign Operating System
                </h3>
                <div class="mt-2">
                    <form hx-post="/api/machines/{}/os" hx-swap="none" @submit="osModal = false">
                        <div class="mt-4">
                            <label for="os_choice" class="block text-sm font-medium text-gray-700">Operating System</label>
                            <select
                                id="os_choice"
                                name="os_choice"
                                class="mt-1 block w-full pl-3 pr-10 py-2 text-base border-gray-300 focus:outline-none focus:ring-indigo-500 focus:border-indigo-500 sm:text-sm rounded-md"
                            >
                                <option value="ubuntu-2204">Ubuntu 22.04</option>
                                <option value="ubuntu-2404">Ubuntu 24.04</option>
                                <option value="debian-12">Debian 12</option>
                                <option value="proxmox">Proxmox VE</option>
                                <option value="talos">Talos</option>
                            </select>
                        </div>
                        <div class="mt-5 sm:mt-4 sm:flex sm:flex-row-reverse">
                            <button
                                type="submit"
                                class="inline-flex w-full justify-center rounded-md bg-indigo-600 px-3 py-2 text-sm font-semibold text-white shadow-sm hover:bg-indigo-500 sm:ml-3 sm:w-auto"
                            >
                                Assign
                            </button>
                            <button
                                type="button"
                                class="mt-3 inline-flex w-full justify-center rounded-md bg-white px-3 py-2 text-sm font-semibold text-gray-900 shadow-sm ring-1 ring-inset ring-gray-300 hover:bg-gray-50 sm:mt-0 sm:w-auto"
                                @click="osModal = false"
                            >
                                Cancel
                            </button>
                        </div>
                    </form>
                </div>
            </div>
        </div>
    "#, id)).into_response()
}

// Handler to get the status update form 
pub async fn get_machine_status(Path(id): Path<Uuid>) -> impl IntoResponse {
    let html = format!(r#"
        <div class="sm:flex sm:items-start">
            <div class="mt-3 text-center sm:mt-0 sm:text-left w-full">
                <h3 class="text-lg leading-6 font-medium text-gray-900">
                    Update Machine Status
                </h3>
                <div class="mt-2">
                    <form hx-post="/machines/{}/status" hx-swap="none" @submit="statusModal = false">
                        <div class="mb-4">
                            <label for="status" class="block text-sm font-medium text-gray-700">Status</label>
                            <select name="status" id="status" class="mt-1 block w-full pl-3 pr-10 py-2 text-base border-gray-300 focus:outline-none focus:ring-indigo-500 focus:border-indigo-500 sm:text-sm rounded-md">
                                <option value="Ready">Ready</option>
                                <option value="AwaitingAssignment">Awaiting OS Assignment</option>
                                <option value="InstallingOS">Installing OS</option>
                                <option value="Error">Error</option>
                            </select>
                        </div>
                        <div class="mt-5 sm:mt-6">
                            <button type="submit" class="inline-flex justify-center w-full rounded-md border border-transparent shadow-sm px-4 py-2 bg-indigo-600 text-base font-medium text-white hover:bg-indigo-700 focus:outline-none focus:ring-2 focus:ring-offset-2 focus:ring-indigo-500 sm:text-sm">
                                Update Status
                            </button>
                        </div>
                    </form>
                </div>
            </div>
        </div>
    "#, id);

    Html(html)
}

// Rename from sse_events to machine_events to match the function name used in the working implementation
async fn machine_events(
    State(state): State<AppState>,
) -> Sse<impl Stream<Item = std::result::Result<Event, Infallible>>> {
    let rx = state.event_manager.subscribe(); // Remove mut
    
    let stream = stream::unfold(rx, |mut rx| async move {
        match rx.recv().await {
            Ok(event_string) => {
                // FIX: Correct parsing and variable naming
                let parts: Vec<&str> = event_string.splitn(2, ':').collect();
                let (event_type, event_payload_str) = if parts.len() == 2 { // Renamed event_id_str to event_payload_str for clarity
                    (parts[0], Some(parts[1]))
                } else {
                    (event_string.as_str(), None)
                };

                // Special handling for ip_download_progress to send raw JSON payload
                if event_type == "ip_download_progress" {
                    if let Some(payload_str) = event_payload_str {
                        // Directly use the JSON string as data for this specific event type
                let sse_event = Event::default()
                    .event(event_type)
                            .data(payload_str); // Use the payload string directly
                        Some((Ok(sse_event), rx))
                    } else {
                         warn!("Received ip_download_progress event without payload: {}", event_string);
                         // Optionally send a comment or skip
                         let comment_event = Event::default().comment("Warning: ip_download_progress event received without payload.");
                         Some((Ok(comment_event), rx))
                    }
                } else {
                    // Existing logic for other events (like machine_updated, machine_discovered, etc.)
                    let data_payload = if let Some(id_str) = event_payload_str { // Use the renamed variable
                        json!({ "type": event_type, "id": id_str })
                    } else {
                        // Ensure there's always a payload, even without ID
                        json!({ "type": event_type })
                    };

                    // Serialize JSON to string for SSE data field
                    match serde_json::to_string(&data_payload) {
                        Ok(json_string) => {
                            let sse_event = Event::default()
                                .event(event_type)
                                .data(json_string);
                Some((Ok(sse_event), rx))
                        },
                        Err(e) => {
                            error!("Failed to serialize SSE event data to JSON: {}", e);
                            let comment_event = Event::default().comment("Internal error: failed to serialize event.");
                            Some((Ok(comment_event), rx))
                        }
                    }
                }
            },
            Err(_) => None,
        }
    });

    Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(1))
            .text("ping"),
    )
}

async fn generate_ipxe_script(script_name: &str) -> Result<String, dragonfly_common::Error> {
    info!("Generating IPXE script: {}", script_name);
 
    match script_name {
        "hookos.ipxe" => {
            // Get Dragonfly base URL (required)
            let base_url_str = env::var("DRAGONFLY_BASE_URL")
                .map_err(|_| {
                    error!("CRITICAL: DRAGONFLY_BASE_URL environment variable is not set. HookOS iPXE script requires this.");
                    Error::Internal("Server is missing required DRAGONFLY_BASE_URL configuration.".to_string())
                })?;

            // --- Derive Tinkerbell defaults from DRAGONFLY_BASE_URL ---
            let default_tinkerbell_host = Url::parse(&base_url_str)
                .ok()
                .and_then(|url| url.host_str().map(String::from))
                .unwrap_or_else(|| {
                    warn!("Could not parse DRAGONFLY_BASE_URL host, using fallback '127.0.0.1' for Tinkerbell defaults.");
                    "127.0.0.1".to_string()
                });
            
            const DEFAULT_GRPC_PORT: u16 = 42113;
            let default_grpc_authority = format!("{}:{}", default_tinkerbell_host, DEFAULT_GRPC_PORT);
            let default_syslog_host = default_tinkerbell_host.clone(); // Default syslog host is just the host part
            // -----------------------------------------------------------

            // Get Tinkerbell config, using derived values as defaults
            let grpc_authority = env::var("TINKERBELL_GRPC_AUTHORITY")
                .unwrap_or_else(|_| {
                    info!("TINKERBELL_GRPC_AUTHORITY not set, deriving default: {}", default_grpc_authority);
                    default_grpc_authority
                });
            let syslog_host = env::var("TINKERBELL_SYSLOG_HOST")
                .unwrap_or_else(|_| {
                     info!("TINKERBELL_SYSLOG_HOST not set, deriving default: {}", default_syslog_host);
                     default_syslog_host
                 });
            let tinkerbell_tls = env::var("TINKERBELL_TLS")
                .map(|s| s.parse().unwrap_or(false))
                .unwrap_or(false);

            // Format the HookOS iPXE script using Dragonfly URL for artifacts and Tinkerbell details for params
            Ok(format!(r#"#!ipxe

echo Loading HookOS via Dragonfly...

set arch ${{buildarch}}
# Dragonfly + Tinkerbell only supports 64 bit archectures.
# The build architecture does not necessarily represent the architecture of the machine on which iPXE is running.
# https://ipxe.org/cfg/buildarch

iseq ${{arch}} i386 && set arch x86_64 ||
iseq ${{arch}} arm32 && set arch aarch64 ||
iseq ${{arch}} arm64 && set arch aarch64 ||
set base-url {}
set retries:int32 0
set retry_delay:int32 0

set worker_id ${{mac}}
set grpc_authority {}
set syslog_host {}
set tinkerbell_tls {}

echo worker_id=${{mac}}
echo grpc_authority={}
echo syslog_host={}
echo tinkerbell_tls={}

set idx:int32 0
:retry_kernel
kernel ${{base-url}}/ipxe/hookos/vmlinuz-${{arch}} \
syslog_host=${{syslog_host}} grpc_authority=${{grpc_authority}} tinkerbell_tls=${{tinkerbell_tls}} worker_id=${{worker_id}} hw_addr=${{mac}} \
console=tty1 console=tty2 console=ttyAMA0,115200 console=ttyAMA1,115200 console=ttyS0,115200 console=ttyS1,115200 tink_worker_image=quay.io/tinkerbell/tink-worker:v0.12.1 \
intel_iommu=on iommu=pt initrd=initramfs-${{arch}} && goto download_initrd || iseq ${{idx}} ${{retries}} && goto kernel-error || inc idx && echo retry in ${{retry_delay}} seconds ; sleep ${{retry_delay}} ; goto retry_kernel

:download_initrd
set idx:int32 0
:retry_initrd
initrd ${{base-url}}/ipxe/hookos/initramfs-${{arch}} && goto boot || iseq ${{idx}} ${{retries}} && goto initrd-error || inc idx && echo retry in ${{retry_delay}} seconds ; sleep ${{retry_delay}} ; goto retry_initrd

:boot
set idx:int32 0
:retry_boot
boot || iseq ${{idx}} ${{retries}} && goto boot-error || inc idx && echo retry in ${{retry_delay}} seconds ; sleep ${{retry_delay}} ; goto retry_boot

:kernel-error
echo Failed to load kernel
imgfree
exit

:initrd-error
echo Failed to load initrd
imgfree
exit

:boot-error
echo Failed to boot
imgfree
exit
"#, 
            base_url_str, // Use Dragonfly base URL for artifacts
            grpc_authority, // Use determined gRPC authority (env var or derived default)
            syslog_host,    // Use determined syslog host (env var or derived default)
            tinkerbell_tls, // Use determined TLS setting
            grpc_authority, // for echo
            syslog_host,    // for echo
            tinkerbell_tls  // for echo
            ))
        },
        "dragonfly-agent.ipxe" => {
            // Get Dragonfly base URL for agent artifacts
            let base_url = env::var("DRAGONFLY_BASE_URL")
                .map_err(|_| {
                    error!("CRITICAL: DRAGONFLY_BASE_URL environment variable is not set. Agent iPXE script requires this.");
                    Error::Internal("Server is missing required DRAGONFLY_BASE_URL configuration.".to_string())
                })?;
                
            // Format the Dragonfly Agent iPXE script
            Ok(format!(r#"#!ipxe
kernel {}/ipxe/dragonfly-agent/vmlinuz \
  ip=dhcp \
  alpine_repo=http://dl-cdn.alpinelinux.org/alpine/v3.21/main \
  modules=loop,squashfs,sd-mod,usb-storage \
  initrd=initramfs-lts \
  modloop={}/ipxe/dragonfly-agent/modloop \
  apkovl={}/ipxe/dragonfly-agent/localhost.apkovl.tar.gz \
  rw
initrd {}/ipxe/dragonfly-agent/initramfs-lts
boot
"#, 
            base_url, // for kernel path
            base_url, // for modloop path
            base_url, // for apkovl path
            base_url  // for initrd path
            ))
        },
        _ => {
            warn!("Cannot generate unknown IPXE script: {}", script_name); // Log the specific script name
            Err(Error::NotFound) // Use the unit variant correctly
        },
    }
}

fn create_streaming_response(
    stream: ReceiverStream<Result<Bytes, Error>>,
    content_type: &str,
    content_length: Option<u64>,
    content_range: Option<String>
) -> Response {
    // Map the stream from Result<Bytes> to Result<Frame<Bytes>, BoxError>
    let mapped_stream = stream.map(|result| {
        match result {
            Ok(bytes) => {
                // Removed check for empty EOF marker
                // Simply map non-empty bytes to a data frame
                Ok(Frame::data(bytes))
            },
            Err(e) => Err(Box::new(e) as Box<dyn std::error::Error + Send + Sync>),
        }
    });
    
    // Create a stream body with explicit end signal
    let body = StreamBody::new(mapped_stream);
    
    // Determine status code based on whether it's a partial response
    let status_code = if content_range.is_some() {
        StatusCode::PARTIAL_CONTENT
    } else {
        StatusCode::OK
    };
    
    // Start building the response
    let mut builder = Response::builder()
        .status(status_code)
        .header(axum::http::header::CONTENT_TYPE, content_type)
        // Always accept ranges
        .header(axum::http::header::ACCEPT_RANGES, "bytes")
        // Always set no compression
        .header(axum::http::header::CONTENT_ENCODING, "identity");

    if let Some(length) = content_length {
        // If Content-Length is known, set it and DO NOT use chunked encoding.
        // This applies to both 200 OK and 206 Partial Content.
        builder = builder.header(axum::http::header::CONTENT_LENGTH, length.to_string());
    } else {
        // Only use chunked encoding if length is truly unknown (should typically only be for 200 OK).
        // It's an error to have a 206 response without Content-Length.
        if status_code == StatusCode::OK { 
            builder = builder.header(axum::http::header::TRANSFER_ENCODING, "chunked");
        } else {
            // This case (206 without Content-Length) ideally shouldn't happen with our logic.
            // Log a warning if it does.
            warn!("Attempting to create 206 response without Content-Length!");
        }
    }
    
    // Include Content-Range if it's a partial response
    if let Some(range_header_value) = content_range {
        builder = builder.header(axum::http::header::CONTENT_RANGE, range_header_value);
    }
    
    // Build the final response
    builder.body(Body::new(body))
        .unwrap_or_else(|_| {
            Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .body(Body::new(Empty::new()))
                .unwrap()
        })
}


async fn read_file_as_stream(
    path: &StdPath,
    range_header: Option<&HeaderValue>, // Add parameter for Range header
    state: Option<&AppState>, // Add optional state for event emission
    machine_id: Option<Uuid> // Add optional machine ID for tracking
) -> Result<(ReceiverStream<Result<Bytes, Error>>, Option<u64>, Option<String>), Error> { // Return size and Content-Range
    info!("[STREAM_READ] Beginning read_file_as_stream for path: {}, range: {:?}, machine_id: {:?}", 
          path.display(), range_header.map(|h| h.to_str().unwrap_or("invalid")), machine_id);

    let mut file = fs::File::open(path).await.map_err(|e| Error::Internal(format!("Failed to open file {}: {}", path.display(), e)))?; // Added mut back
    let (tx, rx) = mpsc::channel::<Result<Bytes, Error>>(32);
    let path_buf = path.to_path_buf();
    
    // Get total file size
    let metadata = fs::metadata(path).await.map_err(|e| Error::Internal(format!("Failed to get metadata {}: {}", path.display(), e)))?;
    let total_size = metadata.len();
    
    // Get file name for progress tracking
    let file_name = path.file_name()
                        .and_then(|name| name.to_str())
                        .map(String::from);
    
    let (start, _end, response_length, content_range_header) = // Marked end as unused
        if let Some(range_val) = range_header {
            if let Ok(range_str) = range_val.to_str() {
                if let Some((start, end)) = parse_range_header(range_str, total_size, file_name.as_deref(), state).await {
                    let length = end - start + 1;
                    let content_range = format!("bytes {}-{}/{}", start, end, total_size);
                    // info!("Serving range request: {} for file {}", content_range, path.display()); // Commented out log
                    (start, end, length, Some(content_range))
                } else {
                    warn!("Invalid Range header format: {}", range_str);
                    // Invalid range, serve the whole file
                    (0, total_size.saturating_sub(1), total_size, None)
                }
            } else {
                warn!("Invalid Range header value (not UTF-8)");
                // Invalid range, serve the whole file
                (0, total_size.saturating_sub(1), total_size, None)
            }
        } else {
            // No range header, serve the whole file
            (0, total_size.saturating_sub(1), total_size, None)
        };

    let response_content_length = Some(response_length);
    let content_range_header_clone = content_range_header.clone(); // Clone for the task
    // Clone state and machine_id needed for the background task *before* spawning
    // Ensures owned values are moved into the async block, avoiding lifetime issues.
    let task_state_owned = state.cloned(); // Creates Option<AppState>
    let task_machine_id_copied = machine_id; // Copies Option<Uuid>

    tokio::spawn(async move {
        // Handle Range requests differently: read the whole range at once
        if content_range_header_clone.is_some() { // Use the clone
            if start > 0 {
                if let Err(e) = file.seek(std::io::SeekFrom::Start(start)).await {
                    error!("Failed to seek file {}: {}", path_buf.display(), e);
                    let _ = tx.send(Err(Error::Internal(format!("File seek error: {}", e)))).await;
                    return;
                }
            }
            
            // Allocate buffer for the exact range size
            let mut buffer = Vec::with_capacity(response_length as usize); // Use with_capacity
            
            // Create a reader limited to the exact range size
            let mut limited_reader = file.take(response_length);
            
            // Read the exact range using the limited reader
            match limited_reader.read_to_end(&mut buffer).await {
                Ok(_) => {
                    // Track progress for range requests too
                    // For range requests, we use the start offset as an indicator of download progress
                    if let (Some(state_ref), Some(machine_id_captured)) = (&task_state_owned, task_machine_id_copied) {
                        if total_size > 0 {
                            // Use start position + current range size as effective progress indicator
                            let bytes_read = buffer.len() as u64;
                            let effective_progress = start + bytes_read;
                            
                            info!("[RANGE_READ] Range request: start={}, bytes_read={}, total_size={}, effective_progress={}",
                                  start, bytes_read, total_size, effective_progress);
                                  
                            // Clone state for progress tracking
                            let owned_state = state_ref.clone();
                            
                            // Spawn progress tracking in a separate task
                            tokio::spawn(async move {
                                track_download_progress(Some(machine_id_captured), effective_progress, total_size, owned_state).await;
                            });
                        }
                    }
                
                    // Send the complete range as a single chunk
                    if tx.send(Ok(Bytes::from(buffer))).await.is_err() {
                        warn!("Client stream receiver dropped for file {} while sending range", path_buf.display());
                    }
                    // Task finishes, tx is dropped, stream closes.
                },
                Err(e) => {
                    error!("Failed to read exact range for file {}: {}", path_buf.display(), e);
                    let _ = tx.send(Err(Error::Internal(format!("File read_exact error: {}", e)))).await;
                }
            }
        } else {
            // Original streaming logic for full file requests
            let mut buffer = vec![0; 65536]; // 64KB buffer
            let mut remaining = response_length; // For full file, response_length == total_size
            let mut total_bytes_sent: u64 = 0;

            while remaining > 0 {
                let read_size = std::cmp::min(remaining as usize, buffer.len());
                match file.read(&mut buffer[..read_size]).await {
                    Ok(0) => {
                        //info!("Reached EOF while serving file {} (remaining: {} bytes)", path_buf.display(), remaining);
                        break; // EOF reached
                    },
                    Ok(n) => { // Handles n > 0
                        let chunk = Bytes::copy_from_slice(&buffer[0..n]);
                        remaining -= n as u64;
                        total_bytes_sent += n as u64; // Add this line to update total bytes sent!

                        // ADDED LOG: Log bytes read and total sent
                        debug!(path = %path_buf.display(), bytes_read = n, total_bytes_sent = total_bytes_sent, total_size = total_size, "[STREAM_READ_LOOP] Read chunk");

                        // Use the owned/copied state and machine_id captured by the 'move' closure
                        // Match against the Option<&AppState> and Option<Uuid> directly
                        if let (Some(state_ref), Some(machine_id_captured)) = (&task_state_owned, task_machine_id_copied) {
                            if total_size > 0 { // Avoid division by zero
                                debug!("[PROGRESS_DEBUG][CACHE_READ] Calling track_download_progress (machine_id: {}, sent: {}, total: {})", machine_id_captured, total_bytes_sent, total_size);
                                // Clone the AppState here to get an owned value for the inner task.
                                let owned_state = state_ref.clone(); // <-- Add this line
                                // Spawn progress tracking in a separate task to avoid blocking the stream
                                tokio::spawn(async move {
                                    // Pass the already owned AppState.
                                    track_download_progress(Some(machine_id_captured), total_bytes_sent, total_size, owned_state).await; // <-- Use owned_state here
                                });
                            } // else: Skipping progress track because total_size is 0 (logged elsewhere if needed)
                        } // else: Skipping progress track because machine_id or state is missing

                        if tx.send(Ok(chunk)).await.is_err() {
                            warn!("Client stream receiver dropped for file {}", path_buf.display());
                            break; // Exit loop if receiver is gone
                        }
                    },
                    Err(e) => {
                        let err = Error::Internal(format!("File read error for {}: {}", path_buf.display(), e));
                        if tx.send(Err(err)).await.is_err() {
                            warn!("Client stream receiver dropped while sending error for {}", path_buf.display());
                        }
                        break; // Exit loop on read error
                    }
                }
            }
        }
        
        // Task finishes, tx is dropped, stream closes.
        debug!("Finished streaming task for: {}", path_buf.display());
    });
    
    // Return the stream, the length of the *content being sent*, and the *original* Content-Range header string
    Ok((tokio_stream::wrappers::ReceiverStream::new(rx), response_content_length, content_range_header))
}

// Serve iPXE artifacts (scripts and binaries)
// Function to serve an iPXE artifact file from a configured directory
pub async fn serve_ipxe_artifact(
    headers: HeaderMap,
    Path(requested_path): Path<String>,
    State(state): State<AppState>, // Add AppState to access event manager and client_ip
) -> Response {
    // Define constants for directories and URLs
    const DEFAULT_ARTIFACT_DIR: &str = "/var/lib/dragonfly/ipxe-artifacts";
    const ARTIFACT_DIR_ENV_VAR: &str = "DRAGONFLY_IPXE_ARTIFACT_DIR";
    const ALLOWED_IPXE_SCRIPTS: &[&str] = &["hookos", "dragonfly-agent"]; // Define allowlist
    const AGENT_APKOVL_PATH: &str = "/var/lib/dragonfly/ipxe-artifacts/dragonfly-agent/localhost.apkovl.tar.gz";
    const AGENT_BINARY_URL: &str = "https://github.com/Zorlin/dragonfly/raw/refs/heads/main/dragonfly-agent-musl"; // TODO: Make configurable
    
    // --- Get Machine ID from Client IP --- 
    let client_ip = state.client_ip.lock().await.clone();
    let machine_id = if let Some(ip) = &client_ip {
        // ADDED LOG: Log the IP being looked up
        info!("[PROGRESS_DEBUG] Looking up machine by IP: {}", ip);
        match db::get_machine_by_ip(ip).await {
            Ok(Some(machine)) => {
                // ADDED LOG: Log successful lookup
                info!("[PROGRESS_DEBUG] Found machine ID {} for IP {}", machine.id, ip);
                Some(machine.id)
            },
            Ok(None) => {
                // Changed to INFO for visibility
                info!("[PROGRESS_DEBUG] No machine found for IP {} requesting artifact {}", ip, requested_path);
                None
            },
            Err(e) => {
                // Changed to INFO for visibility
                info!("[PROGRESS_DEBUG] DB error looking up machine by IP {}: {}", ip, e);
                None
            }
        }
    } else {
        // Changed to INFO for visibility
        info!("[PROGRESS_DEBUG] Client IP not found in state for artifact request {}", requested_path);
        None
    };
    // ----------------------------------

    // Get the base directory from env var or use default
    let base_dir = env::var(ARTIFACT_DIR_ENV_VAR)
        .unwrap_or_else(|_| {
            debug!("{} not set, using default: {}", ARTIFACT_DIR_ENV_VAR, DEFAULT_ARTIFACT_DIR);
            DEFAULT_ARTIFACT_DIR.to_string()
        });
    let base_path = PathBuf::from(base_dir);
    
    // Path sanitization - Allow '/' but prevent '..'
    if requested_path.contains("..") || requested_path.contains('\\') {
        warn!("Attempted iPXE artifact path traversal using '..' or '\': {}", requested_path);
        return (StatusCode::BAD_REQUEST, "Invalid artifact path").into_response();
    }
    
    let artifact_path = base_path.join(&requested_path);

    // --- Serve from Cache First ---
    if artifact_path.exists() {
        info!("[SERVE_ARTIFACT] Cached artifact exists at {}, will use read_file_as_stream", artifact_path.display());
        // Determine content type AND if it's an IPXE script
        let (content_type, is_ipxe) = if requested_path.ends_with(".ipxe") {
            ("text/plain", true)
        } else if requested_path.ends_with(".tar.gz") {
            ("application/gzip", false) // Ensure this returns a tuple
        } else {
            ("application/octet-stream", false) // Ensure this returns a tuple
        };

        // Allowlist check for IPXE scripts from cache
        if is_ipxe { // Check the boolean flag
            let stem = StdPath::new(&requested_path).file_stem().and_then(|s| s.to_str());
            if let Some(stem_str) = stem {
                if !ALLOWED_IPXE_SCRIPTS.contains(&stem_str) {
                    warn!("Attempt to serve non-allowlisted IPXE script stem from cache: {}", stem_str);
                    return (StatusCode::NOT_FOUND, "iPXE Script Not Found").into_response();
                }
            } else {
                 warn!("Could not extract stem from IPXE script path: {}", requested_path);
                 return (StatusCode::BAD_REQUEST, "Invalid IPXE Script Path").into_response();
            }
        }
        
        // Serve allowed script or binary artifact from cache using streaming
        // Pass the potentially found machine_id for progress tracking
        match read_file_as_stream(&artifact_path, headers.get(axum::http::header::RANGE), Some(&state), machine_id).await {
            Ok((stream, file_size, content_range)) => {
                info!("Streaming cached artifact from disk: {}", requested_path);
                return create_streaming_response(stream, content_type, file_size, content_range); // Pass content_range
            },
            Err(e) => {
                error!("Failed to stream cached iPXE artifact: {}", e);
                return (StatusCode::INTERNAL_SERVER_ERROR, "Error reading iPXE artifact").into_response();
            }
        }
    } else {
        // --- File Not Found: Generate or Download --- 
        info!("[SERVE_ARTIFACT] Artifact {} not found locally, will need to generate or download", requested_path);
        
        // FIRST check if it is the specific apkovl path that needs generation
        // Compare against the RELATIVE path expected from the URL
        if requested_path == "dragonfly-agent/localhost.apkovl.tar.gz" {
            // --- Special Case: Generate apkovl on demand ---
            // Use the full absolute path for generation logic
            let generation_target_path = PathBuf::from(AGENT_APKOVL_PATH);
            info!("Generating {} on demand...", generation_target_path.display());

            let base_url = match env::var("DRAGONFLY_BASE_URL") {
                Ok(url) => url,
                Err(_) => {
                    error!("Cannot generate apkovl: DRAGONFLY_BASE_URL environment variable is not set.");
                    return (StatusCode::INTERNAL_SERVER_ERROR, "Server configuration error for apkovl generation").into_response();
                }
            };

            match generate_agent_apkovl(&generation_target_path, &base_url, AGENT_BINARY_URL).await {
                Ok(()) => {
                    info!("Successfully generated {}, now serving...", generation_target_path.display());
                    // Serve the newly generated file (no range needed here as it was just created)
                    match read_file_as_stream(&generation_target_path, None, None, None).await { 
                        Ok((stream, file_size, _)) => {
                            return create_streaming_response(stream, "application/gzip", file_size, None); 
                        },
                        Err(e) => {
                            error!("Failed to stream newly generated apkovl {}: {}", generation_target_path.display(), e);
                            return (StatusCode::INTERNAL_SERVER_ERROR, "Error reading newly generated apkovl").into_response();
                        }
                    }
                },
                Err(e) => {
                    error!("Failed to generate {}: {}", generation_target_path.display(), e);
                    return (StatusCode::INTERNAL_SERVER_ERROR, format!("Failed to generate {}: {}", generation_target_path.display(), e)).into_response();
                }
            }
        } 
        // NEXT check if it's a generic .ipxe script that needs generation
        else if requested_path.ends_with(".ipxe") {
            // --- Generate iPXE scripts on the fly ---
            // Use the relative path for script generation lookup
            match generate_ipxe_script(&requested_path).await {
                Ok(script) => {
                    info!("Generated {} script dynamically.", requested_path);
                    // Cache in background using the full artifact_path
                    let path_clone = artifact_path.clone(); 
                    let script_clone = script.clone();
                    let requested_path_clone = requested_path.clone(); // Clone for the task
                    tokio::spawn(async move {
                        // Ensure parent directory exists before writing
                        if let Some(parent) = path_clone.parent() {
                             if let Err(e) = fs::create_dir_all(parent).await {
                                 warn!("Failed to create directory for caching {}: {}", requested_path_clone, e);
                                 return; 
                             }
                         }
                        if let Err(e) = fs::write(&path_clone, &script_clone).await {
                             warn!("Failed to cache generated {} script: {}", requested_path_clone, e);
                        }
                    });
                    
                    // For iPXE scripts, let's build our own response
                    let content_length = script.len() as u64;
                    
                    // Create a response that's optimized for iPXE
                    return Response::builder()
                        .status(StatusCode::OK)
                        .header(axum::http::header::CONTENT_TYPE, "text/plain")
                        .header(axum::http::header::CONTENT_LENGTH, content_length.to_string())
                        .header(axum::http::header::CONTENT_ENCODING, "identity") // No compression
                        .body(Body::from(script))
                        .unwrap_or_else(|_| {
                            (StatusCode::INTERNAL_SERVER_ERROR, "Failed to build response").into_response()
                        });
                },
                Err(Error::NotFound { .. }) => {
                    warn!("IPXE script {} not found or could not be generated.", requested_path);
                    // Fall through to final 404
                },
                Err(e) => {
                    // Other error during generation (e.g., missing env var)
                    error!("Failed to generate {} script: {}", requested_path, e);
                    return (StatusCode::INTERNAL_SERVER_ERROR, format!("Failed to generate script: {}", e)).into_response();
                }
            }
            // If we fall through here, it means generate_ipxe_script returned NotFound
        }
        // FINALLY, assume it's a binary artifact to download/stream
        else {
            // --- Download/Stream Other Binary Artifacts ---
            let remote_url = match requested_path.as_str() {
                // Alpine Linux netboot artifacts for Dragonfly Agent
                "dragonfly-agent/vmlinuz" => "https://dl-cdn.alpinelinux.org/alpine/latest-stable/releases/x86_64/netboot/vmlinuz-lts",
                "dragonfly-agent/initramfs-lts" => "https://dl-cdn.alpinelinux.org/alpine/latest-stable/releases/x86_64/netboot/initramfs-lts",
                "dragonfly-agent/modloop" => "https://dl-cdn.alpinelinux.org/alpine/latest-stable/releases/x86_64/netboot/modloop-lts",
                // Ubuntu 22.04
                "ubuntu/jammy-server-cloudimg-amd64.img" => "https://cloud-images.ubuntu.com/jammy/current/jammy-server-cloudimg-amd64.img",
                _ => {
                    // If it wasn't an .ipxe script and not a known binary, it's unknown.
                    warn!("Unknown artifact requested: {}", requested_path);
                    return (StatusCode::NOT_FOUND, "Unknown iPXE artifact").into_response();
                }
            };
            
            // Use the efficient streaming download with caching for known artifacts
            // Use artifact_path (full path) for caching
            match stream_download_with_caching(
                remote_url, 
                &artifact_path, 
                headers.get(axum::http::header::RANGE),
                machine_id, // Pass the machine_id found via IP lookup
                Some(&state)
            ).await {
                Ok((stream, content_length, content_range)) => {
                    info!("Streaming artifact {} from remote source", requested_path);
                    return create_streaming_response(stream, "application/octet-stream", content_length, content_range);
                },
                Err(e) => {
                    error!("Failed to stream artifact {}: {}", requested_path, e);
                    return (StatusCode::INTERNAL_SERVER_ERROR, format!("Error streaming artifact: {}", e)).into_response();
                }
            }
        }

        // If code reaches here, it means an IPXE script was requested but generate_ipxe_script 
        // returned NotFound, so return 404.
        (StatusCode::NOT_FOUND, "Unknown or Ungeneratable IPXE Script").into_response()
    }
}

// Add this function after parse_range_header
// Helper function to track and report image download progress
async fn track_download_progress(
    machine_id: Option<Uuid>, 
    bytes_downloaded: u64, 
    total_size: u64,
    state: AppState // Changed from Option<&AppState> to AppState
) {
    info!(
        machine_id = ?machine_id, 
        bytes_downloaded = bytes_downloaded, 
        total_size = total_size, 
        "[PROGRESS_TRACK] CALLED track_download_progress with values: bytes_downloaded={}, total_size={}, machine_id={:?}",
        bytes_downloaded, total_size, machine_id
    );

    debug!(
        machine_id = ?machine_id, 
        bytes_downloaded = bytes_downloaded, 
        total_size = total_size, 
        "[PROGRESS_DEBUG] Entering track_download_progress"
    );

    if total_size == 0 {
        // Changed to INFO
        info!("[PROGRESS_DEBUG] Exiting track_download_progress early: total_size is 0");
        return; // Skip progress for zero-sized files
    }
    
    let progress_float = (bytes_downloaded as f64 / total_size as f64) * 100.0;
    let task_name = "stream image"; // TODO: Can we get the actual filename here?
    
    // If we have a machine ID, send task-specific event
    if let Some(id) = machine_id {
        debug!(machine_id = %id, progress = progress_float, task_name = task_name, "Updating DB progress");
        // Update the machine's task progress in DB
        if let Err(e) = db::update_installation_progress(
            &id,
            progress_float.min(100.0) as u8, // Convert to u8 for DB, clamped at 100
            Some(task_name)
        ).await {
            warn!(machine_id = %id, error = %e, "Failed to update download progress in DB");
        }
        
        // For real-time UI updates, emit a more detailed event with floating point precision
        let task_progress_event = format!(
            "task_progress:{}:{}:{:.3}:{}:{}",
            id,                   // Machine ID
            task_name,            // Task name 
            progress_float,       // Floating point percentage (with 3 decimal precision)
            bytes_downloaded,     // Current bytes
            total_size            // Total bytes
        );
        
        debug!(machine_id = %id, event = %task_progress_event, "Attempting to send task_progress event");
        // Emit the detailed task progress event
        if let Err(e) = state.event_manager.send(task_progress_event.clone()) { // Clone for logging
            warn!(machine_id = %id, error = %e, "Failed to emit task_progress event: {}", task_progress_event);
        }
        
        // Also emit standard machine updated event for compatibility
        // debug!(machine_id = %id, "Sending generic machine_updated event");
        // let _ = state.event_manager.send(format!("machine_updated:{}", id));
    }
    
    // Also send IP-based progress event for any HTTP requests
    let client_ip_guard = state.client_ip.lock().await;
    if let Some(client_ip) = client_ip_guard.as_ref() {
        // Find machine by IP if possible (for cases where we don't have machine_id)
        let ip_machine_id = if machine_id.is_none() {
            match db::get_machine_by_ip(client_ip).await {
                Ok(Some(machine)) => Some(machine.id),
                _ => None,
            }
        } else {
            machine_id
        };
        
        // Emit IP-based progress event
        let ip_progress_event_payload = serde_json::json!({ 
            "ip": client_ip,
            "progress": progress_float, // Send float
            "bytes_downloaded": bytes_downloaded,
            "total_size": total_size,
            "file_name": task_name, // Still uses hardcoded "Stream image"
            "machine_id": ip_machine_id
        });

        // Construct the event string
        let ip_progress_event_string = format!("ip_download_progress:{}", ip_progress_event_payload.to_string());

        info!(client_ip = %client_ip, event_payload = %ip_progress_event_payload, "[PROGRESS_SEND] Attempting to send ip_download_progress event NOW"); // ADDED LOUD LOG
        let send_result = state.event_manager.send(ip_progress_event_string.clone()); // Clone for logging
        
        if let Err(e) = send_result {
            warn!(client_ip = %client_ip, error = %e, "[PROGRESS_SEND] Failed to emit IP-based progress event: {}", ip_progress_event_string);
        } else {
            info!(client_ip = %client_ip, event_payload = %ip_progress_event_payload, "[PROGRESS_SEND] Successfully sent ip_download_progress event"); // ADDED SUCCESS LOG
        }
    } // End of: if let Some(client_ip) = client_ip_guard.as_ref()
    
    debug!("Exiting track_download_progress");
}

// Modify stream_download_with_caching to track progress
async fn stream_download_with_caching(
    url: &str,
    cache_path: &StdPath,
    range_header: Option<&HeaderValue>, // Add parameter for Range header
    machine_id: Option<Uuid>, // Add optional machine ID for tracking
    state: Option<&AppState>, // Add optional state for event emission
) -> Result<(ReceiverStream<Result<Bytes, Error>>, Option<u64>, Option<String>), Error> { // Return Content-Range
    info!("[STREAM_DOWNLOAD] Beginning stream_download_with_caching for URL: {}, cache_path: {}, range: {:?}, machine_id: {:?}",
          url, cache_path.display(), range_header.map(|h| h.to_str().unwrap_or("invalid")), machine_id);

    // Create parent directory if needed
    if let Some(parent) = cache_path.parent() {
        fs::create_dir_all(parent).await.map_err(|e| Error::Internal(format!("Failed to create directory: {}", e)))?;
    }

    // Check if file is already cached
    if cache_path.exists() {
        // Even when serving from cache, track progress for range requests
        if let (Some(machine_id), Some(state), Some(range_val)) = (machine_id, state, range_header) {
            if let Ok(range_str) = range_val.to_str() {
                let file_size = fs::metadata(cache_path).await
                    .map(|m| m.len())
                    .unwrap_or(0);
                    
                if let Some((start, end)) = parse_range_header(range_str, file_size, None, Some(state)).await {
                    let bytes_downloaded = end - start + 1;
                    
                    // Use the start position as a progress indicator for range requests
                    // This gives a rough approximation of download progress across multiple range requests
                    let effective_progress = start + bytes_downloaded;
                    
                    info!("[RANGE_PROGRESS] Cached file with range: start={}, end={}, bytes={}, total={}, effective_progress={}",
                          start, end, bytes_downloaded, file_size, effective_progress);
                          
                    // Track download progress with the effective bytes downloaded
                    tokio::spawn(track_download_progress(Some(machine_id), effective_progress, file_size, state.clone()));
                }
            }
        }
        
        // info!("Serving cached artifact from: {:?}", cache_path); // Commented out log
        return read_file_as_stream(cache_path, range_header, state, machine_id).await; // Pass Range header
    }
    
    info!("Downloading and caching artifact from: {}", url);
    
    // Start HTTP request with reqwest feature for streaming
    let client = reqwest::Client::new();
    let response = client.get(url).send().await.map_err(|e| Error::Internal(format!("HTTP request failed: {}", e)))?;
    
    if !response.status().is_success() {
        return Err(Error::Internal(format!("HTTP error: {}", response.status())));
    }
    
    // Get content length if available
    let content_length = response.content_length();
    if let Some(length) = content_length {
        info!("[PROGRESS_DEBUG] Download size from Content-Length: {} bytes", length);
    } else {
        info!("[PROGRESS_DEBUG] No Content-Length header received from remote server.");
    }
    
    let file = fs::File::create(cache_path).await.map_err(|e| Error::Internal(format!("Failed to create cache file: {}", e)))?;
    let file = Arc::new(tokio::sync::Mutex::new(file));
    let (tx, rx) = mpsc::channel::<Result<Bytes, Error>>(32);
    
    let url_clone = url.to_string();
    let cache_path_clone = cache_path.to_path_buf();
    
    // For tracking download progress
    let total_size = content_length.unwrap_or(0);
    let mut total_bytes_downloaded: u64 = 0;
    let tracking_machine_id = machine_id;
    let app_state_clone = state.cloned();
    
    tokio::spawn(async move {
        let mut client_disconnected = false;
        let mut download_error = false;

        // Get the stream. `bytes_stream` consumes the response object.
        let mut stream = response.bytes_stream(); 

        while let Some(chunk_result) = stream.next().await {
            match chunk_result {
                Ok(chunk) => {
                    let chunk_clone = chunk.clone();
                    let chunk_size = chunk.len() as u64;
                    
                    // Write chunk to cache file concurrently
                    let file_clone = Arc::clone(&file);
                    let write_handle = tokio::spawn(async move {
                        let mut file = file_clone.lock().await;
                        file.write_all(&chunk_clone).await
                    });

                    // Update progress tracking
                    total_bytes_downloaded += chunk_size;
                    
                    // ADDED LOG: Log chunk size and total downloaded
                    debug!(url = %url_clone, chunk_size = chunk_size, total_bytes_downloaded = total_bytes_downloaded, total_size = total_size, "[STREAM_DOWNLOAD_LOOP] Downloaded chunk");

                    if let (Some(machine_id), Some(state)) = (tracking_machine_id, &app_state_clone) {
                        if total_size > 0 {
                            // ADDED LOG: Confirm call to track_download_progress
                            debug!("[PROGRESS_DEBUG] Calling track_download_progress (machine_id: {}, downloaded: {}, total: {})", machine_id, total_bytes_downloaded, total_size);
                            
                            // ADDED LOG: Log before calling track_download_progress function
                            debug!(url = %url_clone, machine_id = %machine_id, bytes_downloaded = total_bytes_downloaded, total_size = total_size, "[STREAM_DOWNLOAD_LOOP] PRE-PROGRESS CALL");
                            
                            track_download_progress(Some(machine_id), total_bytes_downloaded, total_size, state.clone()).await;
                        }
                    }
                    
                    // Attempt to send to client only if not already disconnected
                    if !client_disconnected {
                        if tx.send(Ok(chunk)).await.is_err() {
                            warn!("Client stream receiver dropped for {}. Continuing download in background.", url_clone);
                            client_disconnected = true;
                            // DO NOT break here - let download continue for caching
                        }
                    }

                    // Await the write operation regardless of client connection status
                    match write_handle.await { // Await the JoinHandle itself
                        Ok(Ok(())) => {
                            // Write successful, continue loop
                        },
                        Ok(Err(e)) => {
                            // Write operation failed
                            warn!("Failed to write chunk to cache file {}: {}", cache_path_clone.display(), e);
                            download_error = true;
                            break; // Abort download if we can't write to cache
                        },
                        Err(e) => {
                            // Task failed (e.g., panicked)
                            warn!("Cache write task failed (join error) for {}: {}", cache_path_clone.display(), e);
                            download_error = true;
                            break; // Abort download if write task fails
                        }
                    }
                },
                Err(e) => { // e is reqwest::Error here
                    error!("Download stream error for {}: {}", url_clone, e);
                    // Send error to client if still connected
                    if !client_disconnected {
                        let err = Error::Internal(format!("Download stream error: {}", e));
                        if tx.send(Err(err)).await.is_err() {
                             warn!("Client stream receiver dropped while sending download error for {}", url_clone);
                             // Client disconnected while we were trying to send an error
                             client_disconnected = true;
                        }
                    }
                    download_error = true;
                    break; // Stop processing on download error
                }
            }
            // If download_error is true, the inner match already broke, so we'll exit.
        }
        
        // Explicitly drop the response stream to release network resources potentially sooner
        drop(stream);

        // Report final progress on successful download
        if !download_error && total_size > 0 {
            if let (Some(machine_id), Some(state)) = (tracking_machine_id, &app_state_clone) {
                track_download_progress(Some(machine_id), total_size, total_size, state.clone()).await;
            }
        }

        // Ensure file is flushed and closed first
        if let Ok(mut file) = Arc::try_unwrap(file).map_err(|_| "Failed to unwrap Arc").and_then(|mutex| Ok(mutex.into_inner())) {
            if let Err(e) = file.flush().await {
                warn!("Failed to flush cache file {}: {}", cache_path_clone.display(), e);
            }
            // File is closed when it goes out of scope here
        }
        
        // Only send EOF signal if the download completed without error AND the client is still connected
        if !download_error && !client_disconnected {
            info!("Download complete for {}, client still connected.", url_clone);
            // Removed explicit EOF signal
            // debug!("Sending EOF signal for {}", url_clone);
            // let _ = tx.send(Ok(Bytes::new())).await;
        } else if !download_error && client_disconnected {
            info!("Download complete and cached for {} after client disconnected.", url_clone);
        } else {
            // An error occurred during download or caching
            warn!("Download for {} did not complete successfully due to errors.", url_clone);
            // Optionally remove the potentially incomplete cache file
            // if let Err(e) = fs::remove_file(&cache_path_clone).await {
            //     warn!("Failed to remove incomplete cache file {}: {}", cache_path_clone.display(), e);
            // }
        }
    });
    
    // After download completes or if error, handle the stream
    let (stream, content_length) = (tokio_stream::wrappers::ReceiverStream::new(rx), content_length);

    // We cached the full file, but the *initial* request might have been a range request.
    // If so, we need to read the *cached* file with range support now.
    if range_header.is_some() {
        info!("Download complete, now serving range request from cached file: {:?}", cache_path);
        // Re-call read_file_as_stream with the range header on the now-cached file
        read_file_as_stream(cache_path, range_header, state, machine_id).await // Pass machine_id here too
    } else {
        // No range requested initially, return the full stream we prepared during download
        Ok((stream, content_length, None)) // No Content-Range for full file
    }
}

// Helper to parse Range header. Returns (start, end)
async fn parse_range_header(
    range_str: &str,
    total_size: u64,
    _file_name: Option<&str>, // Marked unused, event logic removed
    _state: Option<&AppState>, // Marked unused, event logic removed
) -> Option<(u64, u64)> {
    if !range_str.starts_with("bytes=") {
        return None;
    }
    let range_val = &range_str[6..]; // Skip "bytes="
    let parts: Vec<&str> = range_val.split('-').collect();
    if parts.len() != 2 {
        return None;
    }

    let start_str = parts[0].trim();
    let end_str = parts[1].trim();

    let start = if start_str.is_empty() {
        // Suffix range: "-<length>"
        if end_str.is_empty() { return None; } // Invalid: "-"
        let suffix_len = end_str.parse::<u64>().ok()?;
        if suffix_len >= total_size { 0 } else { total_size - suffix_len }
    } else {
        // Normal range: "start-" or "start-end"
        start_str.parse::<u64>().ok()?
    };

    let end = if end_str.is_empty() {
        // Range "start-" means start to end of file
        total_size.saturating_sub(1)
    } else {
        // Range "start-end"
        end_str.parse::<u64>().ok()?
    };

    // Validate range: start <= end < total_size
    if start > end || end >= total_size {
        warn!("Invalid range request: start={}, end={}, total_size={}", start, end, total_size);
        return None;
    }

    // Optional: Emit progress event for the range being served
    // if let Some(s) = state { // Check if state exists before trying to use it
    //     let bytes_downloaded = end - start + 1;
    //     let event_data = serde_json::json!({
    //         "progress": 100.0, // A single range request is considered 100% of that range
    //         "bytes_downloaded": bytes_downloaded,
    //         "total_size": total_size,
    //         "file_name": file_name.unwrap_or("unknown")
    //     }).to_string();

    //     // Prefer emitting IP-based progress if possible
    //     let client_ip_guard = s.client_ip.lock().await;
    //     if let Some(client_ip) = client_ip_guard.as_ref() {
    //          let ip_progress_event = format!("ip_download_progress:{{ \"ip\": \"{}\", {} }}", client_ip, &event_data[1..]); // Construct JSON manually
    //          // info!("Sending event: {}", ip_progress_event); // Commented out log
    //          let _ = s.event_manager.send(ip_progress_event);
    //     } else if let Some(f_name) = file_name {
    //         // Fallback to file-based progress if IP is unavailable
    //         let file_progress_event = format!("file_progress:{}:{}:{}", f_name, 100.0, event_data);
    //         let _ = s.event_manager.send(file_progress_event);
    //     }
    // }

    Some((start, end))
}

// Restore original function name and intended purpose (returning HTML partial)
pub async fn get_workflow_progress(
    State(app_state): State<AppState>, // Add AppState
    Path(id): Path<Uuid>
) -> Response { 
    info!("Request for workflow progress HTML partial for machine {}", id);

    let machine = match db::get_machine_by_id(&id).await {
        Ok(Some(m)) => m,
        Ok(None) => {
            error!("Machine not found: {}", id);
            return (StatusCode::NOT_FOUND, Html("<div>Machine not found</div>")).into_response();
        },
        Err(e) => {
            error!("Error fetching machine {}: {}", id, e);
            return (StatusCode::INTERNAL_SERVER_ERROR, Html("<div>Database error</div>")).into_response();
        }
    };

    if machine.status != MachineStatus::InstallingOS {
        info!("Machine {} is not installing OS, status: {:?}", id, machine.status);
        return (StatusCode::OK, Html("<div></div>")).into_response(); // Return empty div if not installing
    }

    match crate::tinkerbell::get_workflow_info(&machine).await {
        Ok(Some(info)) => {
            info!("Successfully got workflow info for machine {}: state={}, progress={}", id, info.state, info.progress);

            // Create the context struct (as defined in ui.rs)
            let context = ui::WorkflowProgressTemplate { 
                machine_id: id,
                workflow_info: info,
            };
            
            // Render the partial using MiniJinja
            ui::render_minijinja(&app_state, "partials/workflow_progress.html", context)
        },
        Ok(None) => {
            info!("No workflow found for machine {}", id);
            (StatusCode::OK, Html("<div></div>")).into_response()
        },
        Err(e) => {
            error!("Error fetching workflow for machine {}: {}", id, e);
            (StatusCode::INTERNAL_SERVER_ERROR, Html("<div>Error fetching workflow</div>")).into_response()
        }
    }
}

// ... (rest of api.rs) ...

// Stub for heartbeat
pub async fn heartbeat() -> Response {
    (StatusCode::OK, "OK").into_response()
}

// Add stubs for functions called from mode.rs
pub async fn check_hookos_artifacts() -> bool {
    // Check for the following four files
    let files = vec![
        "vmlinuz-latest-lts-x86_64",
        "initramfs-latest-lts-x86_64",
        "vmlinuz-latest-lts-aarch64",
        "initramfs-latest-lts-aarch64",
        "dtbs-latest-lts-aarch64.tar.gz",
        "vmlinuz-x86_64",
        "initramfs-x86_64",
        "vmlinuz-aarch64",
        "initramfs-aarch64",
        "dtbs-aarch64.tar.gz",
    ];

    for file in files {
        let path = FilePath::new("/var/lib/dragonfly/ipxe-artifacts/hookos").join(file);
        if !path.exists() {
            return false;
        }
    }

    info!("All HookOS artifacts found");
    true
}

pub async fn download_hookos_artifacts(version: &str) -> anyhow::Result<()> {
    // Create directory structure if it doesn't exist
    let hookos_dir = FilePath::new("/var/lib/dragonfly/ipxe-artifacts/hookos");
    if !hookos_dir.exists() {
        info!("Creating directory structure: {:?}", hookos_dir);
        std::fs::create_dir_all(hookos_dir)?;
    }
    
    // Download checksum file
    let checksum_url = format!("https://github.com/tinkerbell/hook/releases/download/{}/checksum.txt", version);
    let checksum_path = hookos_dir.join("checksum.txt");
    let checksum_response = reqwest::get(checksum_url).await?;
    let checksum_content = checksum_response.text().await?;
    std::fs::write(checksum_path, checksum_content)?;

    // Files to download
    let files = vec![
        "hook_x86_64.tar.gz",
        "hook_aarch64.tar.gz",
        "hook_latest-lts-x86_64.tar.gz",
        "hook_latest-lts-aarch64.tar.gz",
    ];

    // Create a vector of download futures
    let download_futures = files.iter().map(|file| {
        let file = file.to_string();
        let version = version.to_string();
        let hookos_dir = hookos_dir.to_path_buf();
        
        // Return a future for each download
        async move {
            let url = format!("https://github.com/tinkerbell/hook/releases/download/{}/{}", version, file);
            info!("Downloading {} in parallel", url);
            let response = reqwest::get(&url).await?;
            let content = response.bytes().await?;
            let tarball_path = hookos_dir.join(&file);
            std::fs::write(&tarball_path, content)?;
            info!("Downloaded {} to {:?}", file, tarball_path);
            Ok::<_, anyhow::Error>(tarball_path)
        }
    }).collect::<Vec<_>>();
    
    // Execute all downloads in parallel
    let download_results = futures::future::try_join_all(download_futures).await?;
    info!("All HookOS artifacts downloaded in parallel successfully");

    // Create a vector of extraction futures
    let extraction_futures = download_results.into_iter().map(|tarball_path| {
        let hookos_dir = hookos_dir.to_path_buf();
        
        // Return a future for each extraction
        async move {
            let file_name = tarball_path.file_name().unwrap().to_string_lossy().to_string();
            info!("Extracting {:?} in parallel", tarball_path);
            
            // Check if the file exists and has content before trying to extract
            let metadata = match std::fs::metadata(&tarball_path) {
                Ok(meta) => meta,
                Err(e) => {
                    warn!("Skipping extraction of {:?}: file not accessible: {}", tarball_path, e);
                    return Ok::<_, anyhow::Error>(tarball_path);
                }
            };
            
            if metadata.len() == 0 {
                warn!("Skipping extraction of {:?}: file is empty", tarball_path);
                return Ok::<_, anyhow::Error>(tarball_path);
            }
            
            // Open the file for reading
            let tar_file = match File::open(&tarball_path) {
                Ok(f) => f,
                Err(e) => {
                    warn!("Failed to open {:?} for extraction: {}", tarball_path, e);
                    return Ok::<_, anyhow::Error>(tarball_path);
                }
            };
            
            // Create the archive and extract, handling any errors
            // Check if the file is a .tar.gz file
            if file_name.ends_with(".tar.gz") || file_name.ends_with(".tgz") {
                // Use GzDecoder for gzipped files
                let gz = GzDecoder::new(tar_file);
                let mut archive = Archive::new(gz);
                match archive.unpack(&hookos_dir) {
                    Ok(_) => info!("Successfully extracted gzipped archive {:?}", tarball_path),
                    Err(e) => warn!("Failed to extract gzipped archive {:?}: {}", tarball_path, e),
                }
            } else {
                // For non-gzipped files, use directly
                let mut archive = Archive::new(tar_file);
                match archive.unpack(&hookos_dir) {
                    Ok(_) => info!("Successfully extracted archive {:?}", tarball_path),
                    Err(e) => warn!("Failed to extract archive {:?}: {}", tarball_path, e),
                }
            }
            
            Ok::<_, anyhow::Error>(tarball_path)
        }
    }).collect::<Vec<_>>();
    
    // Execute all extractions in parallel
    let extraction_results = futures::future::try_join_all(extraction_futures).await?;
    info!("All HookOS artifacts extracted in parallel successfully");
    
    // Remove all tarballs in parallel
    let cleanup_futures = extraction_results.into_iter().map(|tarball_path| {
        async move {
            // Remove the tarball after extraction
            if let Err(e) = std::fs::remove_file(&tarball_path) {
                warn!("Failed to remove tarball {:?}: {}", tarball_path, e);
            } else {
                info!("Removed tarball {:?}", tarball_path);
            }
            Ok::<(), anyhow::Error>(())
        }
    }).collect::<Vec<_>>();
    
    // Execute all cleanup operations in parallel
    futures::future::try_join_all(cleanup_futures).await?;
    
    info!("HookOS artifacts downloaded, extracted, and cleaned up successfully to {:?}", hookos_dir);
    Ok(())
}

// OS information struct
#[derive(Debug, Clone, serde::Serialize)]
pub struct OsInfo {
    pub name: String,
    pub icon: String,
}

// Get OS icon for a specific OS
pub fn get_os_icon(os: &str) -> String {
    match os {
        "ubuntu-2204" | "ubuntu-2404" => "<i class=\"fab fa-ubuntu text-orange-500 dark:text-orange-500 no-invert\"></i>",
        "debian-12" => "<i class=\"fab fa-debian text-red-500\"></i>",
        "proxmox" => "<i class=\"fas fa-server text-blue-500\"></i>",
        "talos" => "<i class=\"fas fa-robot text-purple-500\"></i>",
        "windows" => "<i class=\"fab fa-windows text-blue-400\"></i>",
        "rocky" | "rocky-9" => "<i class=\"fas fa-mountain text-green-500\"></i>",
        "fedora" => "<i class=\"fab fa-fedora text-blue-600\"></i>",
        "alma" | "almalinux" => "<i class=\"fas fa-hat-cowboy text-amber-600\"></i>",
        _ => "<i class=\"fas fa-square-question text-gray-500\"></i>", // Unknown OS
    }.to_string()
}

// Make format_os_name public
pub fn format_os_name(os: &str) -> String {
    match os {
        "ubuntu-2204" => "Ubuntu 22.04",
        "ubuntu-2404" => "Ubuntu 24.04",
        "debian-12" => "Debian 12",
        "proxmox" => "Proxmox VE",
        "talos" => "Talos",
        _ => os, // Return original string if no match
    }.to_string()
}

// Get both OS name and icon
pub fn get_os_info(os: &str) -> OsInfo {
    OsInfo {
        name: format_os_name(os),
        icon: get_os_icon(os),
    }
}

async fn update_installation_progress(
    State(state): State<AppState>, // State is used for event manager
    _auth_session: AuthSession, // Mark as unused - updates come from agent/tinkerbell
    Path(id): Path<Uuid>,
    Json(payload): Json<InstallationProgressUpdateRequest>,
) -> Response {
    // Remove admin check - allow agent/tinkerbell to post updates
    /*
    if let Err(response) = crate::auth::require_admin(&auth_session) {
        return response;
    }
    */

    info!("Updating installation progress for machine {} to {}% (step: {:?})",
          id, payload.progress, payload.step);

    match db::update_installation_progress(&id, payload.progress, payload.step.as_deref()).await {
        Ok(true) => {
            // Emit machine updated event so the UI fetches new progress HTML
            let _ = state.event_manager.send(format!("machine_updated:{}", id));
            (StatusCode::OK, Json(json!({ "status": "progress_updated", "machine_id": id }))).into_response()
        },
        Ok(false) => {
            let error_response = ErrorResponse {
                error: "Not Found".to_string(),
                message: format!("Machine with ID {} not found", id),
            };
            (StatusCode::NOT_FOUND, Json(error_response)).into_response()
        },
        Err(e) => {
            error!("Failed to update installation progress for machine {}: {}", id, e);
            let error_response = ErrorResponse {
                error: "Database Error".to_string(),
                message: e.to_string(),
            };
            (StatusCode::INTERNAL_SERVER_ERROR, Json(error_response)).into_response()
        }
    }
}

// Add new handler for getting machine tags
#[axum::debug_handler]
async fn api_get_machine_tags(
    Path(id): Path<Uuid>,
) -> Response {
    match get_machine_tags(&id).await {
        Ok(tags) => (StatusCode::OK, Json(tags)).into_response(),
        Err(e) => {
            error!("Failed to get tags for machine {}: {}", id, e);
            let error_response = ErrorResponse {
                error: "Database Error".to_string(),
                message: format!("Failed to retrieve tags: {}", e),
            };
            (StatusCode::INTERNAL_SERVER_ERROR, Json(error_response)).into_response()
        }
    }
}

// Add new handler for updating machine tags
#[axum::debug_handler]
async fn api_update_machine_tags(
    State(state): State<AppState>,
    auth_session: AuthSession,
    Path(id): Path<Uuid>,
    Json(tags): Json<Vec<String>>,
) -> Response {
    // Check if user is authenticated as admin
    if let Err(response) = crate::auth::require_admin(&auth_session) {
        return response;
    }

    match db_update_machine_tags(&id, &tags).await {
        Ok(true) => {
            // Emit machine updated event
            let _ = state.event_manager.send(format!("machine_updated:{}", id)); 
            (StatusCode::OK, Json(json!({ "success": true, "message": "Tags updated" }))).into_response()
        }
                    Ok(false) => {
            let error_response = ErrorResponse {
                error: "Not Found".to_string(),
                message: format!("Machine with ID {} not found", id),
            };
            (StatusCode::NOT_FOUND, Json(error_response)).into_response()
        }
                Err(e) => {
            error!("Failed to update tags for machine {}: {}", id, e);
            let error_response = ErrorResponse {
                error: "Database Error".to_string(),
                message: format!("Failed to update tags: {}", e),
            };
            (StatusCode::INTERNAL_SERVER_ERROR, Json(error_response)).into_response()
        }
    }
}

// New handler to get the current installation status
#[axum::debug_handler]
async fn get_install_status() -> Response {
    // Read the current state from the global static
    let install_state_arc_mutex: Option<Arc<tokio::sync::Mutex<InstallationState>>> = {
        // Acquire read lock, clone the Arc if it exists, then drop the lock immediately
        INSTALL_STATE_REF.read().unwrap().as_ref().cloned()
    };
    
    match install_state_arc_mutex {
        Some(state_ref) => {
            // Clone the state inside the read guard
            let current_state = state_ref.lock().await.clone();
            // Serialize the state to JSON
             let payload = json!({
                "status": current_state,
                "message": current_state.get_message(),
                "animation": current_state.get_animation_class(),
            });
            (StatusCode::OK, Json(payload)).into_response()
        }
        None => {
            // Not in install mode
             let payload = json!({
                "status": "NotInstalling",
                "message": "Dragonfly is not currently installing.",
                "animation": "",
            });
            (StatusCode::OK, Json(payload)).into_response()
        }
    }
}

// Middleware to track client IP address - fixed with proper state extraction
// Now prioritizes X-Real-IP header
pub async fn track_client_ip(
    State(state): State<AppState>,             // State first
    ConnectInfo(addr): ConnectInfo<SocketAddr>, // Then other FromRequestParts extractors
    request: axum::http::Request<Body>,
    next: Next,
) -> Result<Response, StatusCode> { // Return Result based on example
    // Try to get IP from X-Real-IP header first
    let real_ip_header = request.headers()
        .get("X-Real-IP")
        .and_then(|value| value.to_str().ok());

    let ip = match real_ip_header {
        Some(real_ip) => {
            // Log that we found the header and its value
            info!("[track_client_ip] Found X-Real-IP header: {}", real_ip);
            real_ip.to_string()
        },
        None => {
            // Log that the header was missing and we're falling back
            let fallback_ip = addr.ip().to_string();
            info!("[track_client_ip] X-Real-IP header not found. Falling back to ConnectInfo IP: {}", fallback_ip);
            fallback_ip
        }
    };

    // Log the IP being stored in state
    info!("[track_client_ip] Storing client IP in state: {}", ip);
    *state.client_ip.lock().await = Some(ip);

    // Proceed with the request
    Ok(next.run(request).await)
}

// Function to add machine IP tracking to API router helpers
async fn api_get_machine_by_ip(ip: &str) -> Option<Machine> {
    match db::get_machine_by_ip(ip).await {
        Ok(Some(machine)) => Some(machine),
        _ => None,
    }
}

// Add handler for deleting a specific machine tag
#[axum::debug_handler]
async fn api_delete_machine_tag(
    State(state): State<AppState>,
    auth_session: AuthSession,
    Path((id, tag)): Path<(Uuid, String)>,
) -> Response {
    // Check if user is authenticated as admin
    if auth_session.user.is_none() {
        return (StatusCode::UNAUTHORIZED, Json(json!({
            "error": "Unauthorized",
            "message": "Admin authentication required for this operation"
        }))).into_response();
    }

    // Get current tags for the machine
    let result = match db::get_machine_tags(&id).await {
        Ok(tags) => {
            // Filter out the tag to delete
            let new_tags: Vec<String> = tags.into_iter()
                .filter(|t| t != &tag)
                .collect();
            
            // Update with the filtered tags
            match db::update_machine_tags(&id, &new_tags).await {
                Ok(true) => {
                    // Emit machine updated event
                    let _ = state.event_manager.send(format!("machine_updated:{}", id));
                    (StatusCode::OK, Json(json!({"success": true, "message": "Tag deleted"})))
                },
                Ok(false) => {
                    (StatusCode::NOT_FOUND, Json(json!({"error": "Machine not found"})))
                },
                Err(e) => {
                    error!("Failed to update tags after deletion for machine {}: {}", id, e);
                    (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({
                        "error": "Database error", 
                        "message": format!("Failed to update tags: {}", e)
                    })))
                }
            }
        },
        Err(e) => {
            error!("Failed to get tags for machine {}: {}", id, e);
            (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({
                "error": "Database error", 
                "message": format!("Failed to retrieve tags: {}", e)
            })))
        }
    };

    result.into_response()
}

// NEW HANDLER for the partial update
#[axum::debug_handler]
async fn get_machine_status_and_progress_partial(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Response { // Explicitly return Response
    info!("Request for status-and-progress partial for machine {}", id);

    let machine = match db::get_machine_by_id(&id).await {
        Ok(Some(m)) => m,
        Ok(None) => return (StatusCode::NOT_FOUND, Html("<!-- Machine not found -->")).into_response(),
        Err(e) => {
            error!("DB error fetching machine {} for partial: {}", id, e);
            return (StatusCode::INTERNAL_SERVER_ERROR, Html("<!-- DB Error -->")).into_response();
        }
    };

    let workflow_info = if machine.status == MachineStatus::InstallingOS {
        match crate::tinkerbell::get_workflow_info(&machine).await {
            Ok(info_opt) => info_opt, // Can be Some(info) or None
            Err(e) => {
                error!("Tinkerbell error fetching workflow info for {}: {}", id, e);
                None // Treat error as no info
            }
        }
    } else {
        None // Not installing, no workflow info needed
    };

    // Prepare context for the partial template
    // Note: The partial will need access to machine and workflow_info
    let context = json!({
        "machine": machine,
        "workflow_info": workflow_info, // Will be null if not installing or error
    });

    // Render the new partial template using render_minijinja directly
    // REMOVE THE MATCH BLOCK BELOW
    /*
    match ui::render_minijinja(&state, "partials/status_and_progress.html", context) {
        Ok(html) => (StatusCode::OK, Html(html)).into_response(), // Add .into_response() back
        Err(e) => {
            error!("Failed to render status_and_progress partial: {}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, Html("<!-- Render Error -->")).into_response() // Add .into_response() back
        }
    }
    */
    // CALL THE FUNCTION DIRECTLY INSTEAD
    ui::render_minijinja(&state, "partials/status_and_progress.html", context)
}

// Utility function to extract client IP