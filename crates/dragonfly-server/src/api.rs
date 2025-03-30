use axum::{
    extract::{Json, Path, Form, FromRequest, State},
    http::{StatusCode},
    response::{IntoResponse, Response, Html, Sse},
    response::sse::{Event, KeepAlive},
    routing::{post, get, delete, put},
    Router,
    body::Body,
};
use uuid::Uuid;
use dragonfly_common::*;
use dragonfly_common::models::{HostnameUpdateRequest, HostnameUpdateResponse, OsInstalledUpdateRequest, OsInstalledUpdateResponse, BmcCredentialsUpdateRequest, BmcCredentials, BmcType};
use tracing::{error, info, warn, debug};
use serde_json::json;
use serde::Deserialize;
use futures::stream::{self, Stream};
use std::convert::Infallible;
use std::time::Duration;
use crate::auth::AuthSession;
use crate::AppState;
use std::collections::HashMap;
use crate::ui::WorkflowProgressTemplate;
use askama::Template;
use crate::db;
use std::env;
use std::path::{Path as StdPath, PathBuf};
use tokio::fs;
use reqwest;
use bytes::Bytes;
use sha2::{Sha512, Digest};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use http_body::Frame;
use http_body_util::{StreamBody, Empty};
use futures::StreamExt;
use tokio::sync::mpsc;
use std::sync::{Arc};
use tokio_stream::wrappers::ReceiverStream;
use url::Url; // Add Url import
use tempfile::tempdir;
use std::os::unix::fs::symlink as unix_symlink; // For creating the symlink
use tokio::process::Command;

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
        .route("/machines/{id}/tags", get(get_machine_tags))
        .route("/machines/{id}/tags", put(update_machine_tags))
        .route("/events", get(machine_events))
        .route("/machines/{id}/workflow-progress", get(get_workflow_progress))
}

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
            state.event_manager.send(format!("machine_discovered:{}", machine_id));
            
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
                if let Err(e) = crate::tinkerbell::create_workflow(&machine, &os_choice).await {
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
            state.event_manager.send(format!("machine_updated:{}", id));
            
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
            state.event_manager.send(format!("machine_updated:{}", id));
            
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
            state.event_manager.send(format!("machine_updated:{}", id));
            
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
        _ => BmcType::Other(payload.bmc_type),
    };
    
    let credentials = BmcCredentials {
        address: payload.bmc_address,
        username: payload.bmc_username,
        password: Some(payload.bmc_password),
        bmc_type,
    };
    
    match db::update_bmc_credentials(&id, &credentials).await {
        Ok(true) => {
            // Emit machine updated event
            state.event_manager.send(format!("machine_updated:{}", id));
            
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
                    state.event_manager.send(format!("machine_deleted:{}", id));
                    
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
        state.event_manager.send(format!("machine_updated:{}", id));
        
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
async fn get_machine_os(Path(id): Path<String>) -> Response {
    Html(format!(r#"
        <div class="sm:flex sm:items-start">
            <div class="mt-3 text-center sm:mt-0 sm:text-left w-full">
                <h3 class="text-lg leading-6 font-medium text-gray-900">
                    Assign Operating System
                </h3>
                <div class="mt-2">
                    <form hx-post="/machines/{}/os" hx-swap="none" @submit="osModal = false">
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

// Error handling
pub async fn handle_error(err: anyhow::Error) -> Response {
    error!("Internal server error: {}", err);
    let error_response = ErrorResponse {
        error: "Internal Server Error".to_string(),
        message: err.to_string(),
    };

    (StatusCode::INTERNAL_SERVER_ERROR, Json(error_response)).into_response()
}

// Add this new handler function
async fn machine_events(
    State(state): State<AppState>,
) -> Sse<impl Stream<Item = std::result::Result<Event, Infallible>>> {
    let rx = state.event_manager.subscribe();
    
    let stream = stream::unfold(rx, |mut rx| async move {
        match rx.recv().await {
            Ok(event) => {
                let sse_event = Event::default()
                    .data(event);
                Some((Ok(sse_event), rx))
            },
            Err(_) => None,
        }
    });

    Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(1))
            .text("ping")
    )
}

fn format_os_name(os: &str) -> String {
    match os {
        "ubuntu-2204" => "Ubuntu 22.04",
        "ubuntu-2404" => "Ubuntu 24.04",
        "debian-12" => "Debian 12",
        "proxmox" => "Proxmox VE",
        "talos" => "Talos",
        _ => os,
    }.to_string()
}

// Utility function to require admin authentication
async fn require_admin(auth_session: AuthSession) -> std::result::Result<(), StatusCode> {
    // Check if user is authenticated as admin
    if auth_session.user.is_none() {
        Err(StatusCode::UNAUTHORIZED)
    } else {
        Ok(())
    }
}

async fn update_installation_progress(
    State(state): State<AppState>,
    _auth_session: AuthSession,
    Path(id): Path<Uuid>,
    Json(payload): Json<InstallationProgressUpdateRequest>,
) -> Response {
    // We should allow Tinkerbell to update progress without authentication
    // This exception is only for this specific endpoint
    
    info!("Updating installation progress for machine {} to {}%", id, payload.progress);
    if let Some(step) = &payload.step {
        info!("Current installation step: {}", step);
    }
    
    match db::update_installation_progress(&id, payload.progress, payload.step.as_deref()).await {
        Ok(true) => {
            // Emit machine updated event
            state.event_manager.send(format!("machine_updated:{}", id));
            
            let response = InstallationProgressUpdateResponse {
                success: true,
                message: format!("Installation progress updated for machine {}", id),
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
            error!("Failed to update installation progress for machine {}: {}", id, e);
            let error_response = ErrorResponse {
                error: "Database Error".to_string(),
                message: e.to_string(),
            };
            (StatusCode::INTERNAL_SERVER_ERROR, Json(error_response)).into_response()
        }
    }
}

pub async fn get_workflow_progress(
    Path(id): Path<Uuid>,
) -> impl IntoResponse {
    // Get machine from database
    let machine = match db::get_machine_by_id(&id).await {
        Ok(Some(machine)) => machine,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };

    // Get workflow info
    let workflow_info = match crate::tinkerbell::get_workflow_info(&machine).await {
        Ok(Some(info)) => info,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(), 
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };

    // Render just the workflow progress partial
    let template = WorkflowProgressTemplate {
        machine,
        workflow_info: Some(workflow_info),
    };

    match template.render() {
        Ok(html) => Html(html).into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

// New handler to get machine tags
async fn get_machine_tags(
    State(_state): State<AppState>,
    Path(id): Path<Uuid>,
    auth_session: AuthSession, // Ensure user is authenticated/authorized
) -> Response {
    // Basic authentication check (replace with proper authorization if needed)
    if auth_session.user.is_none() {
        return (StatusCode::UNAUTHORIZED, Json(json!({ "message": "Authentication required" }))).into_response();
    }

    match db::get_machine_tags(&id).await { // Assuming db::get_machine_tags exists
        Ok(tags) => {
            (StatusCode::OK, Json(tags)).into_response()
        },
        Err(e) => {
            error!("Failed to retrieve tags for machine {}: {}", id, e);
            (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "message": "Failed to retrieve tags" }))).into_response()
        }
    }
}

// New handler to update machine tags
async fn update_machine_tags(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    auth_session: AuthSession, // Ensure user is authenticated/authorized (admin?)
    Json(tags): Json<Vec<String>>, // Expect a JSON array of strings
) -> Response {
    // Basic admin check (replace/enhance with proper authorization)
    if auth_session.user.is_none() {
        return (StatusCode::FORBIDDEN, Json(json!({ "message": "Admin privileges required" }))).into_response();
    }

    match db::update_machine_tags(&id, &tags).await { // Assuming db::update_machine_tags exists
        Ok(_) => {
            info!("Updated tags for machine {}: {:?}", id, tags);
            // Emit event for SSE refresh
            state.event_manager.send(format!("machine_updated:{}", id)); 
            (StatusCode::OK, Json(json!({ "message": "Tags updated successfully" }))).into_response()
        },
        Err(e) => {
            error!("Failed to update tags for machine {}: {}", id, e);
            (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "message": "Failed to update tags" }))).into_response()
        }
    }
}

// Placeholder function to generate iPXE scripts dynamically
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
# Adjust arch if necessary (iPXE buildarch might not match runtime)
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
    intel_iommu=on iommu=pt tink_worker_image=quay.io/tinkerbell/tink-worker:v0.12.1 initrd=initramfs-${{arch}} && goto download_initrd || iseq ${{idx}} ${{retries}} && goto kernel-error || inc idx && echo retry in ${{retry_delay}} seconds ; sleep ${{retry_delay}} ; goto retry_kernel

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

// Function to serve an iPXE artifact file from a configured directory
pub async fn serve_ipxe_artifact(Path(requested_path): Path<String>) -> Response {
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
        match read_file_as_stream(&artifact_path).await {
            Ok(stream) => {
                info!("Streaming cached artifact from disk: {}", requested_path);
                return create_streaming_response(stream, content_type);
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
                    match read_file_as_stream(&artifact_path).await {
                        Ok(stream) => {
                            return create_streaming_response(stream, "application/gzip");
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
                    return (StatusCode::OK, [(axum::http::header::CONTENT_TYPE, "text/plain")], script).into_response();
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
                // Add other mappings as needed
                _ => {
                    // If it wasn't an .ipxe script and not a known binary, it's unknown.
                    warn!("Unknown artifact requested: {}", requested_path);
                    return (StatusCode::NOT_FOUND, "Unknown iPXE artifact").into_response();
                }
            };
            
            // Use the efficient streaming download with caching for known artifacts
            match stream_download_with_caching(remote_url, &artifact_path).await {
                Ok(stream) => {
                    info!("Streaming artifact {} from remote source", requested_path);
                    return create_streaming_response(stream, "application/octet-stream");
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

// Keep the verify_sha512 function
async fn verify_sha512(file_path: &StdPath, expected_checksum: &str) -> Result<bool> {
    let mut file = fs::File::open(file_path).await.map_err(|e| {
        Error::Internal(format!("Failed to open file for verification: {}", e))
    })?;
    
    let mut buffer = Vec::new();
    file.read_to_end(&mut buffer).await.map_err(|e| {
        Error::Internal(format!("Failed to read file content: {}", e))
    })?;
    
    let mut hasher = Sha512::new();
    hasher.update(&buffer);
    let result = hasher.finalize();
    let actual_checksum = format!("{:x}", result);
    
    Ok(actual_checksum == expected_checksum)
}

// Keep the download_and_verify_artifact function
async fn download_and_verify_artifact(
    artifact_name: &str,
    base_url: &str,
    dest_dir: &StdPath,
    checksum_content: &str,
    max_retries: usize,
) -> Result<()> {
    let dest_file = dest_dir.join(artifact_name);
    let url = format!("{}/{}", base_url, artifact_name);
    
    // Extract expected checksum from checksum file
    let expected_checksum = match checksum_content.lines()
        .find_map(|line| {
            if line.ends_with(artifact_name) {
                line.split_whitespace().next()
            } else {
                None
            }
        }) {
        Some(checksum) => checksum.to_string(),
        None => return Err(Error::Internal(format!("Checksum not found for {}", artifact_name))),
    };
    
    let mut retry_count = 0;
    let mut backoff_ms = 100; // Start with 100ms backoff
    
    while retry_count <= max_retries {
        if retry_count > 0 {
            info!("Retry #{} for downloading {}", retry_count, artifact_name);
            tokio::time::sleep(tokio::time::Duration::from_millis(backoff_ms)).await;
            backoff_ms = std::cmp::min(backoff_ms * 2, 30000); // Exponential backoff, max 30 seconds
        }
        
        // Download the file
        match download_file(&url, &dest_file).await {
            Ok(()) => {
                // Verify checksum
                match verify_sha512(&dest_file, &expected_checksum).await {
                    Ok(true) => {
                        info!("Successfully downloaded and verified {}", artifact_name);
                        return Ok(());
                    },
                    Ok(false) => {
                        warn!("Checksum verification failed for {}, retrying...", artifact_name);
                    },
                    Err(e) => {
                        warn!("Error verifying checksum for {}: {}, retrying...", artifact_name, e);
                    }
                }
            },
            Err(e) => {
                warn!("Failed to download {}: {}, retrying...", artifact_name, e);
            }
        }
        
        retry_count += 1;
    }
    
    Err(Error::Internal(format!("Failed to download {} after {} retries", artifact_name, max_retries)))
}

// Keep the download_hookos_artifacts function
pub async fn download_hookos_artifacts(version: &str) -> Result<()> {
    // Get artifact directory
    let hookos_dir = get_artifacts_dir().join("hookos");
    fs::create_dir_all(&hookos_dir).await.map_err(|e| {
        Error::Internal(format!("Failed to create hookos directory: {}", e))
    })?;
    
    // Define base URL
    let base_url = format!("https://github.com/tinkerbell/hook/releases/download/{}", version);
    
    // First download checksum file
    let checksum_file = hookos_dir.join("checksum.txt");
    let checksum_url = format!("{}/checksum.txt", base_url);
    
    // Try to download checksum with retries
    let mut retry_count = 0;
    let mut backoff_ms = 100;
    let max_retries = 10;
    
    let checksum_content = loop {
        if retry_count > 0 {
            info!("Retry #{} for downloading checksum.txt", retry_count);
            tokio::time::sleep(tokio::time::Duration::from_millis(backoff_ms)).await;
            backoff_ms = std::cmp::min(backoff_ms * 2, 30000);
        }
        
        match download_file(&checksum_url, &checksum_file).await {
            Ok(()) => {
                match fs::read_to_string(&checksum_file).await.map_err(|e| {
                    Error::Internal(format!("Failed to read checksum file: {}", e))
                }) {
                    Ok(content) => break content,
                    Err(e) => {
                        warn!("Failed to read checksum file: {}", e);
                        if retry_count >= max_retries {
                            return Err(Error::Internal(format!("Failed to read checksum file after {} retries", max_retries)));
                        }
                    }
                }
            },
            Err(e) => {
                warn!("Failed to download checksum file: {}", e);
                if retry_count >= max_retries {
                    return Err(Error::Internal(format!("Failed to download checksum file after {} retries", max_retries)));
                }
            }
        }
        
        retry_count += 1;
    };
    
    // Define artifacts to download
    let artifacts = [
        "hook_x86_64.tar.gz",
        "hook_aarch64.tar.gz",
        "hook_latest-lts-x86_64.tar.gz",
        "hook_latest-lts-aarch64.tar.gz"
    ];
    
    // Download and verify each artifact
    for artifact in &artifacts {
        download_and_verify_artifact(artifact, &base_url, &hookos_dir, &checksum_content, 10).await?;
    }
    
    // Extract the downloaded tar.gz files in parallel
    info!("Extracting HookOS artifacts in parallel in {:?}", hookos_dir);
    
    // Create a vector of futures for parallel extraction
    let extract_futures = artifacts.iter().map(|artifact| {
        let artifact_path = hookos_dir.join(artifact);
        let artifact_name = artifact.to_string();
        let dir = hookos_dir.clone();
        
        // Return a future that extracts one artifact
        async move {
            info!("Extracting {}", artifact_name);
            
            // Use tokio::process::Command to run tar
            let output = Command::new("tar")
                .args(["--no-same-permissions", "--overwrite", "-ozxf"])
                .arg(&artifact_path)
                .current_dir(&dir)
                .output()
                .await;
                
            match output {
                Ok(output) => {
                    if !output.status.success() {
                        let stderr = String::from_utf8_lossy(&output.stderr);
                        warn!("Error extracting {}: {}", artifact_name, stderr);
                        false
                    } else {
                        true
                    }
                },
                Err(e) => {
                    warn!("Failed to extract {}: {}", artifact_name, e);
                    false
                }
            }
        }
    }).collect::<Vec<_>>();
    
    // Run all extractions in parallel and collect results
    let results = futures::future::join_all(extract_futures).await;
    
    // Check if all extractions were successful
    let all_successful = results.iter().all(|&success| success);
    
    if all_successful {
        info!("Successfully downloaded and extracted all HookOS artifacts to {:?}", hookos_dir);
    } else {
        warn!("Some HookOS artifacts failed to extract, but continuing anyway");
    }
    
    Ok(())
}

// Keep the get_artifacts_dir function
fn get_artifacts_dir() -> PathBuf {
    const DEFAULT_ARTIFACT_DIR: &str = "/var/lib/dragonfly/ipxe-artifacts";
    const ARTIFACT_DIR_ENV_VAR: &str = "DRAGONFLY_IPXE_ARTIFACT_DIR";

    let dir = env::var(ARTIFACT_DIR_ENV_VAR)
        .unwrap_or_else(|_| {
            // Log at DEBUG level instead of WARN
            debug!("{} not set, using default: {}", ARTIFACT_DIR_ENV_VAR, DEFAULT_ARTIFACT_DIR);
            DEFAULT_ARTIFACT_DIR.to_string()
        });
    PathBuf::from(dir)
}

// Keep the download_file function
async fn download_file(url: &str, dest_path: &StdPath) -> Result<()> {
    let response = reqwest::get(url).await.map_err(|e| {
        Error::Internal(format!("Request failed: {}", e))
    })?;
    
    if !response.status().is_success() {
        return Err(Error::Internal(format!("HTTP error: {}", response.status())));
    }
    
    let bytes = response.bytes().await.map_err(|e| {
        Error::Internal(format!("Failed to read response body: {}", e))
    })?;
    
    fs::write(dest_path, bytes).await.map_err(|e| {
        Error::Internal(format!("Failed to write file: {}", e))
    })?;
    
    Ok(())
}

// Keep the NEW stream_download_with_caching function
async fn stream_download_with_caching(
    url: &str,
    cache_path: &StdPath
) -> Result<ReceiverStream<Result<Bytes>>> {
    // Create parent directory if needed
    if let Some(parent) = cache_path.parent() {
        fs::create_dir_all(parent).await.map_err(|e| Error::Internal(format!("Failed to create directory: {}", e)))?;
    }

    // Check if file is already cached
    if cache_path.exists() {
        info!("Serving cached artifact from: {:?}", cache_path);
        return read_file_as_stream(cache_path).await;
    }
    
    info!("Downloading and caching artifact from: {}", url);
    
    // Start HTTP request with reqwest feature for streaming
    let client = reqwest::Client::new();
    let response = client.get(url).send().await.map_err(|e| Error::Internal(format!("HTTP request failed: {}", e)))?;
    
    if !response.status().is_success() {
        return Err(Error::Internal(format!("HTTP error: {}", response.status())));
    }
    
    let file = fs::File::create(cache_path).await.map_err(|e| Error::Internal(format!("Failed to create cache file: {}", e)))?;
    let file = Arc::new(tokio::sync::Mutex::new(file));
    let (tx, rx) = mpsc::channel::<Result<Bytes>>(32);
    
    let url_clone = url.to_string();
    let cache_path_clone = cache_path.to_path_buf();
    tokio::spawn(async move {
        // Use futures::StreamExt to handle the response body stream
        let mut stream = response.bytes_stream(); 
        let mut error_occurred = false;

        while let Some(chunk_result) = stream.next().await { // Use stream.next().await
            match chunk_result { // chunk_result is Result<Bytes, reqwest::Error>
                Ok(chunk) => {
                    let chunk_clone = chunk.clone();
                    let file_clone = Arc::clone(&file);
                    
                    let write_handle = tokio::spawn(async move {
                        let mut file = file_clone.lock().await;
                        file.write_all(&chunk_clone).await
                    });

                    // Send Ok(chunk) to the stream
                    if tx.send(Ok(chunk)).await.is_err() {
                        warn!("Client stream receiver dropped for {}. Aborting download.", url_clone);
                        error_occurred = true;
                        break;
                    }

                    // Handle potential write error without blocking the stream send
                    match tokio::try_join!(write_handle) { 
                        Ok((Ok(()),)) => {}, 
                        Ok((Err(e),)) => warn!("Failed to write chunk to cache file {}: {}", cache_path_clone.display(), e), 
                        Err(e) => warn!("Cache write task failed (join error) for {}: {}", cache_path_clone.display(), e),
                    }
                },
                Err(e) => { // e is reqwest::Error here
                    error!("Download stream error for {}: {}", url_clone, e);
                    // Send the error wrapped in our Error type
                    let err = Error::Internal(format!("Download stream error: {}", e));
                    if tx.send(Err(err)).await.is_err() {
                         warn!("Client stream receiver dropped while sending error for {}", url_clone);
                    }
                    error_occurred = true;
                    break;
                }
            }
        }
        if !error_occurred { info!("Download complete and cached for {}", url_clone); }
    });
    Ok(tokio_stream::wrappers::ReceiverStream::new(rx))
}

// Keep read_file_as_stream
async fn read_file_as_stream(
    path: &StdPath
) -> Result<ReceiverStream<Result<Bytes>>> {
    let file = fs::File::open(path).await.map_err(|e| Error::Internal(format!("Failed to open file {}: {}", path.display(), e)))?;
    let (tx, rx) = mpsc::channel::<Result<Bytes>>(32);
    let path_buf = path.to_path_buf();
    
    tokio::spawn(async move {
        let mut file = file;
        let mut buffer = vec![0; 65536];
        loop {
            match file.read(&mut buffer).await {
                Ok(n) if n > 0 => {
                    let chunk = Bytes::copy_from_slice(&buffer[0..n]);
                    // Send Ok(chunk) to the stream
                    if tx.send(Ok(chunk)).await.is_err() {
                        warn!("Client stream receiver dropped for file {}", path_buf.display());
                        break;
                    }
                },
                Ok(_) => break, // EOF
                Err(e) => {
                    // Send the error wrapped in our Error type
                    let err = Error::Internal(format!("File read error for {}: {}", path_buf.display(), e));
                    if tx.send(Err(err)).await.is_err() {
                        warn!("Client stream receiver dropped while sending error for {}", path_buf.display());
                    }
                    break;
                }
            }
        }
    });
    Ok(tokio_stream::wrappers::ReceiverStream::new(rx))
}

// Keep create_streaming_response
fn create_streaming_response(
    stream: ReceiverStream<Result<Bytes>>,
    content_type: &str
) -> Response {
    // Map the stream from Result<Bytes> to Result<Frame<Bytes>, BoxError>
    let mapped_stream = stream.map(|result| {
        match result {
            Ok(bytes) => Ok(Frame::data(bytes)), // Wrap Ok bytes in Frame::data
            Err(e) => Err(Box::new(e) as Box<dyn std::error::Error + Send + Sync>),
        }
    });
    let body = StreamBody::new(mapped_stream);
    
    // Build the response using Axum's Body::new()
    Response::builder()
        .status(StatusCode::OK)
        .header(axum::http::header::CONTENT_TYPE, content_type)
        .body(Body::new(body))
        .unwrap_or_else(|_| {
            Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .body(Body::new(Empty::new()))
                .unwrap()
        })
} 

/// Generates the localhost.apkovl.tar.gz file needed by the Dragonfly Agent iPXE script.
async fn generate_agent_apkovl(
    target_apkovl_path: &StdPath, 
    base_url: &str, 
    agent_binary_url: &str,
) -> Result<()> {
    info!("Generating agent APK overlay at: {:?}", target_apkovl_path);

    // 1. Create a temporary directory
    let temp_dir = tempdir().map_err(|e| 
        Error::Internal(format!("Failed to create temp directory for apkovl: {}", e))
    )?;
    let temp_path = temp_dir.path();
    info!("Building apkovl structure in: {:?}", temp_path);

    // 2. Create directory structure
    fs::create_dir_all(temp_path.join("etc/local.d")).await.map_err(|e| Error::Internal(format!("Failed to create dir etc/local.d: {}", e)))?;
    fs::create_dir_all(temp_path.join("etc/apk/protected_paths.d")).await.map_err(|e| Error::Internal(format!("Failed to create dir etc/apk/protected_paths.d: {}", e)))?;
    fs::create_dir_all(temp_path.join("etc/runlevels/default")).await.map_err(|e| Error::Internal(format!("Failed to create dir etc/runlevels/default: {}", e)))?;
    fs::create_dir_all(temp_path.join("usr/local/bin")).await.map_err(|e| Error::Internal(format!("Failed to create dir usr/local/bin: {}", e)))?;

    // 3. Write static files
    fs::write(temp_path.join("etc/hosts"), HOSTS_CONTENT).await.map_err(|e| Error::Internal(format!("Failed to write etc/hosts: {}", e)))?;
    fs::write(temp_path.join("etc/hostname"), HOSTNAME_CONTENT).await.map_err(|e| Error::Internal(format!("Failed to write etc/hostname: {}", e)))?;
    fs::write(temp_path.join("etc/apk/arch"), APK_ARCH_CONTENT).await.map_err(|e| Error::Internal(format!("Failed to write etc/apk/arch: {}", e)))?;
    fs::write(temp_path.join("etc/apk/protected_paths.d/lbu.list"), LBU_LIST_CONTENT).await.map_err(|e| Error::Internal(format!("Failed to write lbu.list: {}", e)))?;
    fs::write(temp_path.join("etc/apk/repositories"), REPOSITORIES_CONTENT).await.map_err(|e| Error::Internal(format!("Failed to write repositories: {}", e)))?;
    fs::write(temp_path.join("etc/apk/world"), WORLD_CONTENT).await.map_err(|e| Error::Internal(format!("Failed to write world: {}", e)))?;
    // Create empty mtab needed by Alpine init
    fs::write(temp_path.join("etc/mtab"), "").await.map_err(|e| Error::Internal(format!("Failed to write etc/mtab: {}", e)))?;
    // Create empty .default_boot_services
    fs::write(temp_path.join("etc/.default_boot_services"), "").await.map_err(|e| Error::Internal(format!("Failed to write .default_boot_services: {}", e)))?;

    // 4. Write dynamic dragonfly-agent.start script
    let start_script_path = temp_path.join("etc/local.d/dragonfly-agent.start");
    
    // Create script content with explicit newline bytes
    let script_content = format!("#!/bin/sh
# Start dragonfly-agent
/usr/local/bin/dragonfly-agent --server {} --setup

exit 0
", base_url);
    
    // Write the file
    fs::write(&start_script_path, script_content).await.map_err(|e| Error::Internal(format!("Failed to write start script: {}", e)))?;
    
    // Make it executable
    set_executable_permission(&start_script_path).await?;

    // 5. Create the symlink (Unchanged, uses std::os::unix)
    let link_target = "/etc/init.d/local";
    let link_path = temp_path.join("etc/runlevels/default/local");
    unix_symlink(link_target, &link_path).map_err(|e| 
        Error::Internal(format!("Failed to create symlink {:?} -> {}: {}", link_path, link_target, e))
    )?;

    // 6. Download the agent binary (Uses download_file which handles errors internally)
    let agent_binary_path = temp_path.join("usr/local/bin/dragonfly-agent");
    download_file(agent_binary_url, &agent_binary_path).await?;
    // Make it executable
    set_executable_permission(&agent_binary_path).await?; // Keep this as is, already fixed

    // 7. Create the tar.gz archive (Unchanged, uses Command which handles errors)
    info!("Creating tarball: {:?}", target_apkovl_path);
    let output = Command::new("tar")
        .arg("-czf")
        .arg(target_apkovl_path)
        .arg("-C")
        .arg(temp_path)
        .arg(".")
        .output()
        .await
        .map_err(|e| Error::Internal(format!("Failed to execute tar command: {}", e)))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error::Internal(format!("Tar command failed: {}", stderr)));
    }

    info!("Successfully generated apkovl: {:?}", target_apkovl_path);

    Ok(())
}

// Helper function to set executable permission (Unix specific)
async fn set_executable_permission(path: &StdPath) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let metadata = fs::metadata(path).await.map_err(|e| 
        Error::Internal(format!("Failed to get metadata for {:?}: {}", path, e))
    )?; // Use map_err instead of ? directly
    let mut perms = metadata.permissions();
    perms.set_mode(0o755); // rwxr-xr-x
    fs::set_permissions(path, perms).await.map_err(|e| 
        Error::Internal(format!("Failed to set executable permission on {:?}: {}", path, e))
    ) // map_err already used here
}

// Content constants
const HOSTS_CONTENT: &str = r#"127.0.0.1       localhost
::1     localhost ip6-localhost ip6-loopback
fe00::  ip6-localnet
ff00::  ip6-mcastprefix
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

// check if HookOS artifacts exist
pub async fn check_hookos_artifacts() -> bool {
    // Get artifact directory
    let hookos_dir = get_artifacts_dir().join("hookos");
    
    // Define the artifacts we expect to find
    let expected_artifacts = [
        "hook_x86_64.tar.gz",
        "hook_aarch64.tar.gz",
        "hook_latest-lts-x86_64.tar.gz",
        "hook_latest-lts-aarch64.tar.gz"
    ];
    
    // Check if the directory exists
    match fs::metadata(&hookos_dir).await {
        Ok(meta) if meta.is_dir() => {
            // Directory exists, check for artifacts
        },
        _ => {
            return false; // Directory doesn't exist
        }
    }
    
    // Check if all artifacts exist
    for artifact in &expected_artifacts {
        let artifact_path = hookos_dir.join(artifact);
        if fs::metadata(&artifact_path).await.is_err() {
            debug!("HookOS artifact {} not found", artifact);
            return false;
        }
    }
    
    // All artifacts exist
    true
}