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

// Make stub functions public
// Stub for serving iPXE artifacts
pub async fn serve_ipxe_artifact(Path(path): Path<String>) -> Response {
    info!("Request to serve iPXE artifact: {}", path);
    // TODO: Implement logic to serve the actual iPXE files (e.g., hookos.ipxe, dragonfly-agent.ipxe)
    // This might involve reading files from a specific directory.
    let content = format!("#!ipxe\\n# Placeholder for {}", path);
    (StatusCode::OK, [(axum::http::header::CONTENT_TYPE, "text/plain")], content).into_response()
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