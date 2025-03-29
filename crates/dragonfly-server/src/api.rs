use axum::{
    extract::{Json, Path, Form, FromRequest, State, Extension},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response, Html, Sse},
    response::sse::{Event, KeepAlive},
    routing::{post, get, delete, put},
    Router,
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
use serde::Serialize;
use crate::auth::AuthSession;
use crate::AppState;
use std::collections::HashMap;
use crate::ui::{MachineListTemplate, WorkflowProgressTemplate};
use askama::Template;
use std::sync::Arc;

use crate::db;

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

// Handler for iPXE script generation
pub async fn ipxe_script(Path(mac): Path<String>) -> Response {
    if !mac.contains(':') || mac.split(':').count() != 6 {
        return (StatusCode::NOT_FOUND, "Not Found").into_response();
    }
    
    info!("Generating iPXE script for MAC: {}", mac);
    
    match db::get_machine_by_mac(&mac).await {
        Ok(Some(_)) => {
            let script = format!("#!ipxe\nchain http://10.7.1.30:8080/hookos.ipxe");
            (StatusCode::OK, [(axum::http::header::CONTENT_TYPE, "text/plain")], script).into_response()
        },
        Ok(None) => {
            let script = format!("#!ipxe\nchain http://10.7.1.30:8080/sparxplug.ipxe");
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

// Update the delete_machine function to use kube-rs instead of kubectl
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

pub async fn get_machine_list(State(state): State<AppState>, headers: HeaderMap) -> Response {
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

            let html = MachineListTemplate {
                machines,
                workflow_infos,
                theme: crate::ui::get_theme_from_cookie(&headers),
                is_authenticated: true, // Since this is an API endpoint, we assume auth is handled by middleware
            }.render().unwrap_or_else(|e| {
                error!("Failed to render machine list template: {}", e);
                "Template error".to_string()
            });

            Html(html).into_response()
        }
        Err(e) => {
            error!("Failed to get machines: {}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, "Database error").into_response()
        }
    }
}

// Add a new API route for HTMX workflow progress updates
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