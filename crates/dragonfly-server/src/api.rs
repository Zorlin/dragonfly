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
use tracing::{error, info, warn};
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
        .route("/ipxe/{*path}", get(serve_ipxe_artifact))
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

// Function to serve an iPXE artifact file from a configured directory
// NOTE: This function is implemented but NOT currently routed in api_router.
// It needs to be added to the router manually if intended to be used.
async fn serve_ipxe_artifact(Path(requested_path): Path<String>) -> Response {
    // Define constants for directories and URLs
    const DEFAULT_ARTIFACT_DIR: &str = "/var/lib/dragonfly/ipxe-artifacts";
    const ARTIFACT_DIR_ENV_VAR: &str = "DRAGONFLY_IPXE_ARTIFACT_DIR";
    
    // Get the base directory from env var or use default
    let base_dir = env::var(ARTIFACT_DIR_ENV_VAR)
        .unwrap_or_else(|_| {
            warn!("{} not set, using default: {}", ARTIFACT_DIR_ENV_VAR, DEFAULT_ARTIFACT_DIR);
            DEFAULT_ARTIFACT_DIR.to_string()
        });
    let base_path = PathBuf::from(base_dir);
    
    // Path sanitization
    if requested_path.contains("..") || requested_path.contains('/') || requested_path.contains('\\') {
        warn!("Attempted iPXE artifact path traversal: {}", requested_path);
        return (StatusCode::BAD_REQUEST, "Invalid artifact path").into_response();
    }
    
    let artifact_path = base_path.join(&requested_path);
    
    // If file exists locally, serve it directly
    if artifact_path.exists() {
        // Determine content type based on file extension
        let content_type = if requested_path.ends_with(".ipxe") {
            "text/plain"
        } else {
            "application/octet-stream"  // For binary files like kernel/initrd
        };
        
        match fs::read(&artifact_path).await {
            Ok(content) => {
                info!("Successfully served iPXE artifact: {}", requested_path);
                (StatusCode::OK, [(axum::http::header::CONTENT_TYPE, content_type)], content).into_response()
            },
            Err(e) => {
                error!("Failed to read iPXE artifact file {:?}: {}", artifact_path, e);
                (StatusCode::INTERNAL_SERVER_ERROR, "Error reading iPXE artifact").into_response()
            }
        }
    } else {
        // File doesn't exist - need to pull from remote and stream
        info!("Artifact {} not found locally, streaming from remote", requested_path);
        
        // Handle iPXE script generation differently - these are generated not downloaded
        if requested_path.ends_with(".ipxe") {
            // [Code for generating ipxe scripts remains the same]
        }

        // For binary artifacts that need to be downloaded and streamed
        let remote_url = match requested_path.as_str() {
            // Dragonfly Agent iPXE artifacts
            "dragonfly-agent/modloop" => "https://dl-cdn.alpinelinux.org/alpine/latest-stable/releases/x86_64/netboot-3.21.3/modloop-lts",
            "dragonfly-agent/vmlinuz-lts" => "https://dl-cdn.alpinelinux.org/alpine/latest-stable/releases/x86_64/netboot-3.21.3/vmlinuz-lts",
            "dragonfly-agent/initramfs-lts" => "https://dl-cdn.alpinelinux.org/alpine/latest-stable/releases/x86_64/netboot-3.21.3/initramfs-lts",
            // Add other mappings as needed
            _ => {
                warn!("Unknown artifact requested: {}", requested_path);
                return (StatusCode::NOT_FOUND, "Unknown iPXE artifact").into_response();
            }
        };
        
        // Create a streaming response using Axum's streaming capabilities
        let mapped_stream = streaming_download(remote_url, &artifact_path).map(|result| {
            result.map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)
        });
        (
            StatusCode::OK, 
            [(axum::http::header::CONTENT_TYPE, "application/octet-stream")],
            Body::new(http_body_util::StreamBody::new(mapped_stream))
        ).into_response()
    }
}

// Function to download and stream simultaneously
fn streaming_download(url: &str, dest_path: &PathBuf) -> impl Stream<Item = Result<Bytes>> {
    let url = url.to_string();
    let dest_path = dest_path.clone();
    
    // Create a stream that processes the download
    stream::try_unfold(
        (None, None, false),
        move |(mut client_opt, mut file_opt, mut complete)| {
            let url = url.clone();
            let dest_path = dest_path.clone();
            
            async move {
                // If complete, end the stream
                if complete {
                    return Ok(None);
                }
                
                // Initialize on first call
                if client_opt.is_none() {
                    match reqwest::Client::new().get(&url).send().await {
                        Ok(resp) => {
                            if !resp.status().is_success() {
                                return Err(Error::Internal(format!("HTTP error: {}", resp.status())));
                            }
                            client_opt = Some(resp);
                        },
                        Err(e) => {
                            return Err(Error::Internal(format!("Failed to start download: {}", e)));
                        }
                    }
                    
                    // Open file for writing
                    match fs::File::create(&dest_path).await {
                        Ok(file) => file_opt = Some(file),
                        Err(e) => {
                            return Err(Error::Internal(format!("Failed to create cache file: {}", e)));
                        }
                    }
                }
                
                // Get next chunk from the HTTP stream
                if let Some(client) = &mut client_opt {
                    match client.chunk().await {
                        Ok(Some(chunk)) => {
                            // Write chunk to cache file
                            let file_clone = Arc::clone(&file_opt);
                            let mut file_guard = file_clone.lock().await;
                            if let Err(e) = file_guard.write_all(&chunk).await {
                                warn!("Failed to write to cache, continuing stream: {}", e);
                                // Note: we continue serving even if caching fails
                            }
                            
                            // Return chunk to client and continue
                            Ok(Some((chunk, (client_opt, file_opt, false))))
                        },
                        Ok(None) => {
                            // End of stream reached
                            info!("Download complete, cached to {:?}", dest_path);
                            Ok(Some((Bytes::new(), (None, None, true))))
                        },
                        Err(e) => {
                            Err(Error::Internal(format!("Download error: {}", e)))
                        }
                    }
                } else {
                    Err(Error::Internal("Client not initialized"))
                }
            }
        }
    )
    .filter_map(|result| async move {
        match result {
            Ok(bytes) if !bytes.is_empty() => Some(Ok(bytes)),
            Ok(_) => None,  // Skip empty chunks at stream end
            Err(e) => Some(Err(e)),
        }
    })
}

/// Verify a file against its SHA512 checksum
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

/// Download and verify an artifact with retries
async fn download_and_verify_artifact(
    artifact_name: &str,
    base_url: &str,
    dest_dir: &StdPath,
    checksums_content: &str,
    max_retries: usize,
) -> Result<()> {
    let dest_file = dest_dir.join(artifact_name);
    let url = format!("{}/{}", base_url, artifact_name);
    
    // Extract expected checksum from checksums file
    let expected_checksum = match checksums_content.lines()
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

/// Download HookOS artifacts
pub async fn download_hookos_artifacts(version: &str) -> Result<()> {
    // Get artifact directory
    let hookos_dir = get_artifacts_dir().join("hookos");
    fs::create_dir_all(&hookos_dir).await.map_err(|e| {
        Error::Internal(format!("Failed to create hookos directory: {}", e))
    })?;
    
    // Define base URL
    let base_url = format!("https://github.com/tinkerbell/hookos/releases/download/{}", version);
    
    // First download checksums file
    let checksums_file = hookos_dir.join("checksums.txt");
    let checksums_url = format!("{}/checksums.txt", base_url);
    
    // Try to download checksums with retries
    let mut retry_count = 0;
    let mut backoff_ms = 100;
    let max_retries = 10;
    
    let checksums_content = loop {
        if retry_count > 0 {
            info!("Retry #{} for downloading checksums.txt", retry_count);
            tokio::time::sleep(tokio::time::Duration::from_millis(backoff_ms)).await;
            backoff_ms = std::cmp::min(backoff_ms * 2, 30000);
        }
        
        match download_file(&checksums_url, &checksums_file).await {
            Ok(()) => {
                match fs::read_to_string(&checksums_file).await.map_err(|e| {
                    Error::Internal(format!("Failed to read checksums file: {}", e))
                }) {
                    Ok(content) => break content,
                    Err(e) => {
                        warn!("Failed to read checksums file: {}", e);
                        if retry_count >= max_retries {
                            return Err(Error::Internal(format!("Failed to read checksums file after {} retries", max_retries)));
                        }
                    }
                }
            },
            Err(e) => {
                warn!("Failed to download checksums file: {}", e);
                if retry_count >= max_retries {
                    return Err(Error::Internal(format!("Failed to download checksums file after {} retries", max_retries)));
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
        download_and_verify_artifact(artifact, &base_url, &hookos_dir, &checksums_content, 10).await?;
    }
    
    info!("Successfully downloaded all HookOS artifacts to {:?}", hookos_dir);
    Ok(())
}

/// Get the configured artifacts directory or use default
fn get_artifacts_dir() -> PathBuf {
    const DEFAULT_ARTIFACT_DIR: &str = "/var/lib/dragonfly/ipxe-artifacts";
    const ARTIFACT_DIR_ENV_VAR: &str = "DRAGONFLY_IPXE_ARTIFACT_DIR";

    let dir = env::var(ARTIFACT_DIR_ENV_VAR)
        .unwrap_or_else(|_| {
            warn!("{} not set, using default: {}", ARTIFACT_DIR_ENV_VAR, DEFAULT_ARTIFACT_DIR);
            DEFAULT_ARTIFACT_DIR.to_string()
        });
    PathBuf::from(dir)
}

/// Download a file with error handling
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

async fn stream_download_with_caching(
    url: &str,
    cache_path: &StdPath
) -> Result<ReceiverStream<Result<Bytes, Error>>> {
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
        let mut stream = response.bytes_stream();
        let mut error_occurred = false;

        while let Some(chunk_result) = stream.next().await {
            match chunk_result {
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
                    // Use tokio::try_join! macro
                    match tokio::try_join!(write_handle) { // Correct path: tokio::try_join!
                        Ok(Ok(_)) => {},
                        Ok(Err(e)) => warn!("Failed to write chunk to cache file {}: {}", cache_path_clone.display(), e),
                        Err(e) => warn!("Cache write task failed (join error) for {}: {}", cache_path_clone.display(), e),
                    }
                },
                Err(e) => {
                    error!("Download stream error for {}: {}", url_clone, e);
                    // Send the error wrapped in our Error type
                    let err = Error::Internal(format!("Download error: {}", e));
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

/// Corrected read file as stream function
async fn read_file_as_stream(
    path: &StdPath
) -> Result<ReceiverStream<Result<Bytes, Error>>> {
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

/// Corrected create streaming response function
fn create_streaming_response(
    stream: ReceiverStream<Result<Bytes, Error>>,
    content_type: &str
) -> Response {
    // Map the stream from Result<Bytes, Error> to Result<Frame<Bytes>, Box<dyn std::error::Error + Send + Sync>>
    let mapped_stream = stream.map(|result| {
        match result {
            Ok(bytes) => Ok(Frame::data(bytes)), // Wrap Bytes in Frame::data()
            Err(e) => Err(Box::new(e) as Box<dyn std::error::Error + Send + Sync>),
        }
    });
    let body = StreamBody::new(mapped_stream);
    
    // Build the response using Axum's Body::new()
    Response::builder()
        .status(StatusCode::OK)
        .header(axum::http::header::CONTENT_TYPE, content_type)
        .body(Body::new(body)) // Correct usage
        .unwrap_or_else(|_| {
            Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .body(Body::new(Empty::new())) // Correct usage
                .unwrap()
        })
} 