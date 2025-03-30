use askama::Template;
use axum::{
    extract::{Query, State},
    http::{header, HeaderMap, StatusCode},
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
    Form, Router,
};
use dragonfly_common::models::{Machine, MachineStatus};
use tracing::{error, info};
use std::collections::HashMap;
use serde_json;
use uuid;
use chrono::{DateTime, Utc};
use cookie::{Cookie, SameSite};
use std::fs;

use crate::db;
use crate::auth::{self, AuthSession, Settings, Credentials};
use crate::filters;
use crate::tinkerbell::WorkflowInfo;

// Extract theme from cookies
pub fn get_theme_from_cookie(headers: &HeaderMap) -> String {
    if let Some(cookie_header) = headers.get(header::COOKIE) {
        if let Ok(cookie_str) = cookie_header.to_str() {
            for cookie_pair in cookie_str.split(';') {
                if let Ok(cookie) = Cookie::parse(cookie_pair.trim()) {
                    if cookie.name() == "dragonfly_theme" {
                        return cookie.value().to_string();
                    }
                }
            }
        }
    }
    "system".to_string()
}

#[derive(Template)]
#[template(path = "index.html")]
pub struct IndexTemplate {
    pub title: String,
    pub machines: Vec<Machine>,
    pub status_counts: HashMap<String, usize>,
    pub status_counts_json: String,
    pub theme: String,
    pub is_authenticated: bool,
    pub display_dates: HashMap<String, String>,
}

#[derive(Template)]
#[template(path = "machine_list.html")]
pub struct MachineListTemplate {
    pub machines: Vec<Machine>,
    pub theme: String,
    pub is_authenticated: bool,
    pub is_admin: bool,
    pub workflow_infos: HashMap<uuid::Uuid, crate::tinkerbell::WorkflowInfo>,
}

#[derive(Template)]
#[template(path = "machine_details.html")]
pub struct MachineDetailsTemplate {
    pub machine: Machine,
    pub theme: String,
    pub is_authenticated: bool,
    pub created_at_formatted: String,
    pub updated_at_formatted: String,
    pub workflow_info: Option<crate::tinkerbell::WorkflowInfo>,
}

#[derive(Template)]
#[template(path = "settings.html")]
pub struct SettingsTemplate {
    pub theme: String,
    pub is_authenticated: bool,
    pub admin_username: String,
    pub require_login: bool,
    pub default_os_none: bool,
    pub default_os_ubuntu2204: bool,
    pub default_os_ubuntu2404: bool,
    pub default_os_debian12: bool,
    pub default_os_proxmox: bool,
    pub default_os_talos: bool,
    pub has_initial_password: bool,
    pub rendered_password: String,
    pub show_admin_settings: bool,
    pub error_message: Option<String>,
}

// Add a new template for just the workflow progress section
#[derive(Template)]
#[template(path = "partials/workflow_progress.html")]
pub struct WorkflowProgressTemplate {
    pub machine: Machine,
    pub workflow_info: Option<WorkflowInfo>,
}

enum UiTemplate {
    Index(IndexTemplate),
    MachineList(MachineListTemplate),
    MachineDetails(MachineDetailsTemplate),
    Settings(SettingsTemplate),
}

impl IntoResponse for UiTemplate {
    fn into_response(self) -> Response {
        match self {
            UiTemplate::Index(template) => {
                match template.render() {
                    Ok(html) => Html(html).into_response(),
                    Err(err) => {
                        eprintln!("Template error: {}", err);
                        StatusCode::INTERNAL_SERVER_ERROR.into_response()
                    }
                }
            },
            UiTemplate::MachineList(template) => {
                match template.render() {
                    Ok(html) => Html(html).into_response(),
                    Err(err) => {
                        eprintln!("Template error: {}", err);
                        StatusCode::INTERNAL_SERVER_ERROR.into_response()
                    }
                }
            },
            UiTemplate::MachineDetails(template) => {
                match template.render() {
                    Ok(html) => Html(html).into_response(),
                    Err(err) => {
                        eprintln!("Template error: {}", err);
                        StatusCode::INTERNAL_SERVER_ERROR.into_response()
                    }
                }
            },
            UiTemplate::Settings(template) => {
                match template.render() {
                    Ok(html) => Html(html).into_response(),
                    Err(err) => {
                        eprintln!("Template error: {}", err);
                        StatusCode::INTERNAL_SERVER_ERROR.into_response()
                    }
                }
            },
        }
    }
}

// Create router with state
pub fn ui_router() -> Router<crate::AppState> {
    Router::new()
        .route("/", get(index))
        .route("/machines", get(machine_list))
        .route("/machines/{id}", get(machine_details))
        .route("/theme/toggle", get(toggle_theme))
        .route("/settings", get(settings_page))
        .route("/settings", post(update_settings))
}

// Count machines by status and return a HashMap
fn count_machines_by_status(machines: &[Machine]) -> HashMap<String, usize> {
    let mut counts = HashMap::new();
    
    // Initialize counts for all statuses to ensure they're present in the chart
    counts.insert("Existing OS".to_string(), 0);
    counts.insert("Awaiting OS Assignment".to_string(), 0);
    counts.insert("Installing OS".to_string(), 0);
    counts.insert("Ready".to_string(), 0);
    counts.insert("Offline".to_string(), 0);
    counts.insert("Error".to_string(), 0);
    
    // Count actual statuses
    for machine in machines {
        let status_key = match &machine.status {
            MachineStatus::ExistingOS => "Existing OS",
            MachineStatus::AwaitingAssignment => "Awaiting OS Assignment",
            MachineStatus::InstallingOS => "Installing OS",
            MachineStatus::Ready => "Ready",
            MachineStatus::Offline => "Offline",
            MachineStatus::Error(_) => "Error",
        };
        
        *counts.get_mut(status_key).unwrap() += 1;
    }
    
    counts
}

// Helper to format DateTime<Utc> to a friendly string
fn format_datetime(dt: &DateTime<Utc>) -> String {
    dt.format("%Y-%m-%d %H:%M:%S UTC").to_string()
}

pub async fn index(
    State(app_state): State<crate::AppState>,
    headers: HeaderMap,
    auth_session: AuthSession,
) -> Response {
    // Get theme preference from cookie
    let theme = get_theme_from_cookie(&headers);
    let is_authenticated = auth_session.user.is_some();
    
    // Check if login is required site-wide
    let require_login = app_state.settings.lock().await.require_login;
    
    // If require_login is enabled and user is not authenticated,
    // redirect to login page
    if require_login && !is_authenticated {
        return Redirect::to("/login").into_response();
    }
    
    match db::get_all_machines().await {
        Ok(machines) => {
            info!("Rendering index page with {} machines", machines.len());
            
            // Count machines by status
            let status_counts = count_machines_by_status(&machines);
            
            // Convert status counts to JSON for the chart
            let status_counts_json = serde_json::to_string(&status_counts)
                .unwrap_or_else(|_| "{}".to_string());

            // Prepare display dates
            let mut display_dates = HashMap::new();
            for machine in &machines {
                // Store date with UUID as string key for template access
                display_dates.insert(machine.id.to_string(), format_datetime(&machine.created_at));
            }
            
            UiTemplate::Index(IndexTemplate {
                title: "Dragonfly".to_string(),
                machines,
                status_counts,
                status_counts_json,
                theme,
                is_authenticated,
                display_dates,
            }).into_response()
        },
        Err(e) => {
            error!("Error fetching machines for index page: {}", e);
            UiTemplate::Index(IndexTemplate {
                title: "Dragonfly".to_string(),
                machines: vec![],
                status_counts: HashMap::new(),
                status_counts_json: "{}".to_string(),
                theme: "system".to_string(),
                is_authenticated,
                display_dates: HashMap::new(),
            }).into_response()
        }
    }
}

pub async fn machine_list(
    State(app_state): State<crate::AppState>,
    headers: HeaderMap,
    auth_session: AuthSession,
) -> Response {
    let theme = get_theme_from_cookie(&headers);
    let is_authenticated = auth_session.user.is_some();
    let is_admin = is_authenticated;

    let require_login = app_state.settings.lock().await.require_login;
    if require_login && !is_authenticated {
        return Redirect::to("/login").into_response();
    }

    match db::get_all_machines().await {
        Ok(machines) => {
            let mut workflow_infos = HashMap::new();
            for machine in &machines {
                if machine.status == MachineStatus::InstallingOS {
                    match crate::tinkerbell::get_workflow_info(machine).await {
                        Ok(Some(info)) => {
                            workflow_infos.insert(machine.id, info);
                        }
                        Ok(None) => { /* No active workflow found */ }
                        Err(e) => {
                            error!("Error fetching workflow info for machine {}: {}", machine.id, e);
                            // Optionally insert a default/error state info
                        }
                    }
                }
            }

            UiTemplate::MachineList(MachineListTemplate {
                machines,
                theme,
                is_authenticated,
                is_admin,
                workflow_infos,
            }).into_response()
        },
        Err(e) => {
            error!("Error fetching machines for machine list page: {}", e);
            UiTemplate::MachineList(MachineListTemplate {
                machines: vec![],
                theme,
                is_authenticated,
                is_admin,
                workflow_infos: HashMap::new(),
            }).into_response()
        }
    }
}

pub async fn machine_details(
    State(app_state): State<crate::AppState>,
    axum::extract::Path(id): axum::extract::Path<String>,
    headers: HeaderMap,
    auth_session: AuthSession,
) -> Response {
    // Get theme preference from cookie
    let theme = get_theme_from_cookie(&headers);
    let is_authenticated = auth_session.user.is_some();
    
    // Check if login is required site-wide
    let require_login = app_state.settings.lock().await.require_login;
    
    // If require_login is enabled and user is not authenticated,
    // redirect to login page
    if require_login && !is_authenticated {
        return Redirect::to("/login").into_response();
    }
    
    // Parse UUID from string
    match uuid::Uuid::parse_str(&id) {
        Ok(uuid) => {
            // Get machine by ID
            match db::get_machine_by_id(&uuid).await {
                Ok(Some(machine)) => {
                    info!("Rendering machine details page for machine {}", uuid);
                    
                    // Format dates before constructing the template
                    let created_at_formatted = machine.created_at.format("%Y-%m-%d %H:%M:%S UTC").to_string();
                    let updated_at_formatted = machine.updated_at.format("%Y-%m-%d %H:%M:%S UTC").to_string();
                    
                    // Fetch workflow information for this machine if it's installing OS
                    let workflow_info = if machine.status == MachineStatus::InstallingOS {
                        match crate::tinkerbell::get_workflow_info(&machine).await {
                            Ok(info) => {
                                if let Some(info) = &info {
                                    info!("Found workflow information for machine {}: state={}, progress={}%", 
                                         uuid, info.state, info.progress);
                                }
                                info
                            },
                            Err(e) => {
                                error!("Error fetching workflow information for machine {}: {}", uuid, e);
                                None
                            }
                        }
                    } else {
                        None
                    };
                    
                    UiTemplate::MachineDetails(MachineDetailsTemplate { 
                        machine,
                        theme,
                        is_authenticated,
                        created_at_formatted,
                        updated_at_formatted,
                        workflow_info,
                    }).into_response()
                },
                Ok(None) => {
                    error!("Machine not found: {}", uuid);
                    // Return to index page with error
                    UiTemplate::Index(IndexTemplate {
                        title: "Dragonfly - Machine Not Found".to_string(),
                        machines: vec![],
                        status_counts: HashMap::new(),
                        status_counts_json: "{}".to_string(),
                        theme: "system".to_string(),
                        is_authenticated,
                        display_dates: HashMap::new(),
                    }).into_response()
                },
                Err(e) => {
                    error!("Error fetching machine {}: {}", uuid, e);
                    // Return to index page with error
                    UiTemplate::Index(IndexTemplate {
                        title: "Dragonfly - Error".to_string(),
                        machines: vec![],
                        status_counts: HashMap::new(),
                        status_counts_json: "{}".to_string(),
                        theme: "system".to_string(),
                        is_authenticated,
                        display_dates: HashMap::new(),
                    }).into_response()
                }
            }
        },
        Err(e) => {
            error!("Invalid UUID: {}", e);
            // Return to index page with error
            UiTemplate::Index(IndexTemplate {
                title: "Dragonfly - Invalid UUID".to_string(),
                machines: vec![],
                status_counts: HashMap::new(),
                status_counts_json: "{}".to_string(),
                theme: "system".to_string(),
                is_authenticated,
                display_dates: HashMap::new(),
            }).into_response()
        }
    }
}

// Handler for theme toggling
pub async fn toggle_theme(
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    // Get theme from URL parameters, default to "light"
    let theme = params.get("theme").cloned().unwrap_or_else(|| "light".to_string());
    
    // Create cookie with proper builder pattern
    let mut cookie = Cookie::new("dragonfly_theme", theme);
    cookie.set_path("/");
    cookie.set_max_age(time::Duration::days(365));
    cookie.set_http_only(true);
    cookie.set_same_site(SameSite::Lax);
    
    // Get the return URL from parameters or default to home page
    let return_to = params.get("return_to").cloned().unwrap_or_else(|| "/".to_string());
    
    // Set cookie header and redirect
    (
        [(header::SET_COOKIE, cookie.to_string())],
        Redirect::to(&return_to)
    ).into_response()
}

// Handler for the settings page
pub async fn settings_page(
    State(app_state): State<crate::AppState>,
    auth_session: AuthSession,
    headers: HeaderMap,
) -> Response {
    // Get current theme from cookie
    let theme = get_theme_from_cookie(&headers);
    
    // Check if user is authenticated
    let is_authenticated = auth_session.user.is_some();
    
    // Get current settings
    let settings_lock = app_state.settings.lock().await;
    let require_login = settings_lock.require_login;
    let default_os = settings_lock.default_os.clone();
    drop(settings_lock);
    
    // If require_login is enabled and user is not authenticated,
    // redirect to login page
    if require_login && !is_authenticated {
        return Redirect::to("/login").into_response();
    }
    
    let show_admin_settings = is_authenticated;
    
    // Get admin username if authenticated
    let admin_username = if let Some(user) = &auth_session.user {
        user.username.clone()
    } else {
        "admin".to_string()
    };
    
    // Check if initial password file exists (only for admins)
    let (has_initial_password, rendered_password) = if is_authenticated {
        info!("Checking for initial password file at: initial_password.txt");
        let current_dir = match std::env::current_dir() {
            Ok(dir) => dir.display().to_string(),
            Err(_) => "unknown".to_string(),
        };
        info!("Current directory: {}", current_dir);
        
        match fs::read_to_string("initial_password.txt") {
            Ok(password) => {
                info!("Found initial password file, will display to admin");
                (true, password)
            },
            Err(e) => {
                info!("No initial password file found: {}", e);
                (false, String::new())
            }
        }
    } else {
        (false, String::new())
    };
    
    UiTemplate::Settings(SettingsTemplate {
        theme,
        is_authenticated,
        admin_username,
        require_login,
        default_os_none: default_os.is_none(),
        default_os_ubuntu2204: default_os.as_deref() == Some("ubuntu-2204"),
        default_os_ubuntu2404: default_os.as_deref() == Some("ubuntu-2404"),
        default_os_debian12: default_os.as_deref() == Some("debian-12"),
        default_os_proxmox: default_os.as_deref() == Some("proxmox"),
        default_os_talos: default_os.as_deref() == Some("talos"),
        has_initial_password,
        rendered_password,
        show_admin_settings,
        error_message: None,
    }).into_response()
}

#[derive(serde::Deserialize)]
pub struct SettingsForm {
    pub theme: String,
    pub require_login: Option<String>,
    pub default_os: Option<String>,
    pub username: Option<String>,
    pub old_password: Option<String>,
    pub password: Option<String>,
    pub password_confirm: Option<String>,
}

// Handler for settings form submission
pub async fn update_settings(
    State(app_state): State<crate::AppState>,
    auth_session: AuthSession,
    Form(form): Form<SettingsForm>,
) -> Response {
    // Check if user is authenticated
    let is_authenticated = auth_session.user.is_some();
    
    // If updating password, verify credentials first
    if let (Some(_old_password), Some(password), Some(password_confirm)) = 
        (&form.old_password, &form.password, &form.password_confirm) {
        
        if !password.is_empty() && password == password_confirm {
            // Only allow password update if authenticated
            if is_authenticated {
                let username = form.username.unwrap_or_else(|| "admin".to_string());
                
                // Create new credentials directly
                match Credentials::create(username, password.clone()) {
                    Ok(new_credentials) => {
                        // Save them
                        if let Err(e) = auth::save_credentials(&new_credentials).await {
                            return Response::builder()
                                .status(StatusCode::INTERNAL_SERVER_ERROR)
                                .body(format!("Failed to save credentials: {}", e).into())
                                .unwrap();
                        }
                    },
                    Err(e) => {
                        return Response::builder()
                            .status(StatusCode::INTERNAL_SERVER_ERROR)
                            .body(format!("Failed to create credentials: {}", e).into())
                            .unwrap();
                    }
                }
            }
        }
    }
    
    // Update settings
    let mut settings = match app_state.settings.try_lock() {
        Ok(guard) => (*guard).clone(),
        Err(_) => Settings::default(),
    };
    
    // Update require_login setting
    settings.require_login = form.require_login.is_some();
    
    // Update default OS setting
    match form.default_os.as_deref() {
        Some("none") => settings.default_os = None,
        Some(os) if !os.is_empty() => settings.default_os = Some(os.to_string()),
        _ => {}
    }
    
    // Save the updated settings
    if let Err(e) = auth::save_settings(&settings).await {
        error!("Failed to save settings: {}", e);
    }
    
    // Update settings in app state
    if let Ok(mut guard) = app_state.settings.try_lock() {
        *guard = settings;
    }
    
    // Redirect back to settings page
    Redirect::to("/settings").into_response()
} 