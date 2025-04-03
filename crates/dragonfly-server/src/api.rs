use axum::{
    routing::{get, post, delete, put},
    Router,
    extract::{State, Path, Json, Form, FromRequest},
    http::{StatusCode},
    response::{IntoResponse, Html, Response, sse::{Event, Sse, KeepAlive}},
};
use std::convert::Infallible;
use serde_json::json;
use uuid::Uuid;
use dragonfly_common::models::{MachineStatus, HostnameUpdateRequest, HostnameUpdateResponse, OsInstalledUpdateRequest, OsInstalledUpdateResponse, BmcType, BmcCredentials, StatusUpdateRequest, BmcCredentialsUpdateRequest, InstallationProgressUpdateRequest, RegisterRequest};
use crate::db::{self, RegisterResponse, ErrorResponse, OsAssignmentRequest, get_machine_tags, update_machine_tags as db_update_machine_tags};
use crate::AppState;
use crate::auth::AuthSession;
use std::collections::HashMap;
use tracing::{info, error, warn};
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
use axum::http::header;
use tokio::fs;
use std::path::Path as StdPath;
use std::path::PathBuf;

pub fn api_router() -> Router<crate::AppState> {
    Router::new()
        .route("/machines", post(register_machine))
        .route("/machines", get(get_all_machines))
        .route("/machines/{id}", get(get_machine))
        .route("/machines/{id}", delete(delete_machine))
        .route("/machines/{id}", put(update_machine))
        .route("/machines/{id}/os", get(get_machine_os))
        .route("/machines/{id}/os", post(assign_os))
        .route("/machines/{id}/status", get(get_machine_status))
        .route("/machines/{id}/status", post(update_status))
        .route("/machines/{id}/hostname", post(update_hostname))
        .route("/machines/{id}/hostname", get(get_hostname_form))
        .route("/machines/{id}/os_installed", post(update_os_installed))
        .route("/machines/{id}/bmc", post(update_bmc))
        .route("/machines/{id}/progress", post(update_installation_progress))
        .route("/machines/{id}/tags", get(api_get_machine_tags))
        .route("/machines/{id}/tags", put(api_update_machine_tags))
        .route("/events", get(sse_events))
        .route("/machines/{id}/workflow-progress", get(get_workflow_progress))
        .route("/heartbeat", get(heartbeat))
        .route("/install/status", get(get_install_status))
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
    Json(payload): Json<RegisterRequest>,
) -> Response {
    info!("Registering machine with MAC: {}", payload.mac_address);
    
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
                // For non-HTMX requests, return JSON
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
        Ok(Some(machine)) => {
            (StatusCode::OK, Json(machine)).into_response()
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
    _auth_session: AuthSession,
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

// Define a new request struct for the update machine operation
#[derive(Debug, Deserialize)]
struct UpdateMachineRequest {
    hostname: Option<String>,
    ip_address: Option<String>,
    mac_address: Option<String>,
    #[serde(rename = "nameservers[]")]
    nameservers: Option<Vec<String>>,
}

// Add this function to handle machine updates
#[axum::debug_handler]
async fn update_machine(
    State(state): State<AppState>,
    auth_session: AuthSession,
    Path(id): Path<Uuid>,
    Form(payload): Form<UpdateMachineRequest>,
) -> Response {
    // Check if user is authenticated as admin
    if auth_session.user.is_none() {
        return (StatusCode::UNAUTHORIZED, Json(json!({
            "error": "Unauthorized",
            "message": "Admin authentication required for this operation"
        }))).into_response();
    }

    info!("Updating machine {}", id);
    let mut updated = false;
    let mut messages = vec![];

    // Update hostname if provided
    if let Some(hostname) = &payload.hostname {
        if !hostname.is_empty() {
            match db::update_hostname(&id, hostname).await {
                Ok(true) => {
                    updated = true;
                    messages.push(format!("Hostname updated to '{}'", hostname));
                },
                Ok(false) => {
                    messages.push("Machine not found for hostname update".to_string());
                },
                Err(e) => {
                    error!("Failed to update hostname: {}", e);
                    messages.push(format!("Failed to update hostname: {}", e));
                }
            }
        }
    }

    // Update IP address if provided
    if let Some(ip_address) = &payload.ip_address {
        if !ip_address.is_empty() {
            match db::update_ip_address(&id, ip_address).await {
                Ok(true) => {
                    updated = true;
                    messages.push(format!("IP address updated to '{}'", ip_address));
                },
                Ok(false) => {
                    messages.push("Machine not found for IP address update".to_string());
                },
                Err(e) => {
                    error!("Failed to update IP address: {}", e);
                    messages.push(format!("Failed to update IP address: {}", e));
                }
            }
        }
    }
    
    // Update MAC address if provided
    if let Some(mac_address) = &payload.mac_address {
        if !mac_address.is_empty() {
            match db::update_mac_address(&id, mac_address).await {
                Ok(true) => {
                    updated = true;
                    messages.push(format!("MAC address updated to '{}'", mac_address));
                },
                Ok(false) => {
                    messages.push("Machine not found for MAC address update".to_string());
                },
                Err(e) => {
                    error!("Failed to update MAC address: {}", e);
                    messages.push(format!("Failed to update MAC address: {}", e));
                }
            }
        }
    }
    
    // Update DNS servers if provided
    if let Some(nameservers) = &payload.nameservers {
        // Filter out empty strings
        let filtered_nameservers: Vec<String> = nameservers.iter()
            .filter(|ns| !ns.is_empty())
            .cloned()
            .collect();
            
        if !filtered_nameservers.is_empty() {
            match db::update_nameservers(&id, &filtered_nameservers).await {
                Ok(true) => {
                    updated = true;
                    messages.push(format!("DNS servers updated"));
                },
                Ok(false) => {
                    messages.push("Machine not found for DNS servers update".to_string());
                },
                Err(e) => {
                    error!("Failed to update DNS servers: {}", e);
                    messages.push(format!("Failed to update DNS servers: {}", e));
                }
            }
        }
    }

    if updated {
        // Emit machine updated event
        let _ = state.event_manager.send(format!("machine_updated:{}", id));
        
        (StatusCode::OK, Json(json!({
            "success": true,
            "message": messages.join(", ")
        }))).into_response()
    } else {
        (StatusCode::BAD_REQUEST, Json(json!({
            "success": false,
            "message": if messages.is_empty() { "No updates provided".to_string() } else { messages.join(", ") }
        }))).into_response()
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

// Single SSE handler for all event types
async fn sse_events(
    State(state): State<AppState>,
) -> Sse<impl Stream<Item = std::result::Result<Event, Infallible>>> {
    info!("Client connected to SSE endpoint (/events)");
    let rx = state.event_manager.subscribe();
    
    // Prepare the initial state event to send immediately
    let initial_event: Option<Result<Event, Infallible>> = {
        let state_ref_option = {
            let guard = INSTALL_STATE_REF.read().unwrap();
            guard.as_ref().cloned()
        };
        
        if let Some(state_ref) = state_ref_option {
            // Use tokio::task::block_in_place for the synchronous lock inside the async fn
            // Or preferably, make INSTALL_STATE_REF use tokio::sync::Mutex if possible
            // For now, let's assume it's okay or handle potential blocking
            let current_state = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(async {
                    state_ref.lock().await.clone()
                })
            });

            info!("Preparing initial installation state to send: {:?}", current_state);
            
            let payload = serde_json::json!({
                "message": current_state.get_message(),
                "animation": current_state.get_animation_class(),
            });
            let event_data = payload.to_string(); // Just the data for install_status
            
            Some(Ok(Event::default()
                .event("install_status")
                .data(event_data)))
        } else {
            info!("No initial installation state found to send.");
            None
        }
    };

    // Create the stream, starting with the initial event if available
    let stream = stream::unfold((rx, initial_event), |(mut rx, mut initial)| async move {
        // If there's an initial event, yield it first
        if let Some(event) = initial.take() {
             info!("Yielding initial state event directly to client.");
            return Some((event, (rx, None))); // Pass along rx and None for initial
        }

        // Otherwise, wait for the next event from the broadcast channel
        match rx.recv().await {
            Ok(event_string) => {
                // Parse the event string to extract type and data
                let event_parts: Vec<&str> = event_string.splitn(2, ':').collect();
                let event_type = event_parts[0];
                let event_data = event_parts.get(1).unwrap_or(&"");
                
                // Different event types need different formatting
                let sse_event = match event_type {
                    // Machine events need the id wrapper JSON format
                    "machine_discovered" | "machine_updated" | "machine_deleted" => {
                        let json_data = serde_json::json!({ "id": event_data }).to_string();
                        Event::default().event(event_type).data(json_data)
                    },
                    // Installation events pass the raw data through (already formatted)
                    "install_status" | "browser_redirect" => {
                        Event::default().event(event_type).data(*event_data)
                    },
                    // Default format for any other event types
                    _ => {
                        info!("Unknown SSE event type: {}, using default formatting", event_type);
                        Event::default().event(event_type).data(*event_data)
                    }
                };
                
                Some((Ok(sse_event), (rx, None))) // Pass along rx and None for initial
            },
            Err(e) => {
                error!("SSE stream recv error: {:?}. Closing stream.", e);
                None // End stream on error
            },
        }
    });

    Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("ping")
    )
}

async fn generate_ipxe_script(script_name: &str) -> Result<String> {
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

// Serve iPXE artifacts (scripts and binaries)
// Function to serve an iPXE artifact file from a configured directory
pub async fn serve_ipxe_artifact(headers: HeaderMap, Path(requested_path): Path<String>) -> Response {
    // Define constants for directories and URLs
    const DEFAULT_ARTIFACT_DIR: &str = "/var/lib/dragonfly/ipxe-artifacts";
    const ARTIFACT_DIR_ENV_VAR: &str = "DRAGONFLY_IPXE_ARTIFACT_DIR";
    const ALLOWED_IPXE_SCRIPTS: &[&str] = &["hookos", "dragonfly-agent"]; // Define allowlist
    const AGENT_APKOVL_PATH: &str = "dragonfly-agent/localhost.apkovl.tar.gz";
    const AGENT_BINARY_URL: &str = "https://github.com/Zorlin/dragonfly/raw/refs/heads/main/dragonfly-agent"; // TODO: Make configurable
    
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
        match read_file_as_stream(&artifact_path, headers.get(axum::http::header::RANGE)).await { // Pass Range header
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
        info!("Artifact {} not found locally, attempting to fetch or generate", requested_path);
        
        // FIRST check if it is the specific apkovl path that needs generation
        if requested_path == AGENT_APKOVL_PATH {
            // --- Special Case: Generate apkovl on demand ---
            info!("Generating {} on demand...", AGENT_APKOVL_PATH);
            
            let base_url = match env::var("DRAGONFLY_BASE_URL") {
                Ok(url) => url,
                Err(_) => {
                    error!("Cannot generate apkovl: DRAGONFLY_BASE_URL environment variable is not set.");
                    return (StatusCode::INTERNAL_SERVER_ERROR, "Server configuration error for apkovl generation").into_response();
                }
            };

            match generate_agent_apkovl(&artifact_path, &base_url, AGENT_BINARY_URL).await {
                Ok(()) => {
                    info!("Successfully generated {}, now serving...", AGENT_APKOVL_PATH);
                    match read_file_as_stream(&artifact_path, None).await { // Pass None for range
                        Ok((stream, file_size, _)) => { // Adjust pattern match
                            return create_streaming_response(stream, "application/gzip", file_size, None); // Pass None for content_range
                        },
                        Err(e) => {
                            error!("Failed to stream newly generated apkovl {}: {}", AGENT_APKOVL_PATH, e);
                            return (StatusCode::INTERNAL_SERVER_ERROR, "Error reading newly generated apkovl").into_response();
                        }
                    }
                },
                Err(e) => {
                    error!("Failed to generate {}: {}", AGENT_APKOVL_PATH, e);
                    return (StatusCode::INTERNAL_SERVER_ERROR, format!("Failed to generate {}: {}", AGENT_APKOVL_PATH, e)).into_response();
                }
            }
        } 
        // NEXT check if it's a generic .ipxe script that needs generation
        else if requested_path.ends_with(".ipxe") {
            // --- Generate iPXE scripts on the fly ---
            match generate_ipxe_script(&requested_path).await {
                Ok(script) => {
                    info!("Generated {} script dynamically.", requested_path);
                    // Cache in background
                    let path_clone = artifact_path.clone();
                    let script_clone = script.clone();
                    tokio::spawn(async move {
                        // Ensure parent directory exists before writing
                        if let Some(parent) = path_clone.parent() {
                             if let Err(e) = fs::create_dir_all(parent).await {
                                 warn!("Failed to create directory for caching {}: {}", requested_path, e);
                                 return; 
                             }
                         }
                        if let Err(e) = fs::write(&path_clone, &script_clone).await {
                             warn!("Failed to cache generated {} script: {}", requested_path, e);
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
            match stream_download_with_caching(remote_url, &artifact_path, headers.get(axum::http::header::RANGE)).await { // Pass Range header
                Ok((stream, content_length, content_range)) => {
                    info!("Streaming artifact {} from remote source", requested_path);
                    return create_streaming_response(stream, "application/octet-stream", content_length, content_range); // Pass content_range
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


// Stub for getting workflow progress
pub async fn get_workflow_progress(Path(id): Path<Uuid>) -> Response {
    info!("Request for workflow progress for machine {}", id);
    // TODO: Implement logic to fetch actual workflow progress
    (StatusCode::OK, Json(json!({ "machine_id": id, "progress": 0, "step": "Not Implemented" }))).into_response()
}

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
        let path = FilePath::new("/var/lib/dragonfly/ipxe/hookos").join(file);
        if !path.exists() {
            return false;
        }
    }

    info!("All HookOS artifacts found");
    true
}

pub async fn download_hookos_artifacts(version: &str) -> anyhow::Result<()> {
    // Create directory structure if it doesn't exist
    let hookos_dir = FilePath::new("/var/lib/dragonfly/ipxe/hookos");
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
    auth_session: AuthSession,
    Path(id): Path<Uuid>,
    // Use db::InstallationProgressUpdateRequest
    Json(payload): Json<InstallationProgressUpdateRequest>,
) -> Response {
    // Ensure admin authentication
    // Use the imported require_admin function
    if let Err(response) = crate::auth::require_admin(&auth_session) {
        return response;
    }

    info!("Updating installation progress for machine {} to {}% (step: {:?})",
          id, payload.progress, payload.step);

    // Update progress in the database
    match db::update_installation_progress(&id, payload.progress, payload.step.as_deref()).await {
        Ok(true) => {
            // Emit machine updated event - Consider adding progress info?
            // state.event_manager.send(format!("machine_updated:{}", id));
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
            error!("Failed to update installation progress for {}: {}", id, e);
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