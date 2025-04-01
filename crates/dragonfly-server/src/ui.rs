use axum::{
    extract::{Query, State},
    http::{header, HeaderMap, StatusCode},
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
    Form, Router,
};
use dragonfly_common::models::{Machine, MachineStatus};
use tracing::{error, info, warn};
use std::collections::HashMap;
use chrono::{DateTime, Utc};
use cookie::{Cookie, SameSite};
use std::fs;
use serde::Serialize;
use crate::db::{self, get_app_settings, save_app_settings, mark_setup_completed};
use crate::auth::{self, AuthSession, Settings, Credentials};
use crate::mode;
use minijinja::{Environment, Error as MiniJinjaError, ErrorKind as MiniJinjaErrorKind};

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
    "light".to_string()
}

#[derive(Serialize)]
pub struct IndexTemplate {
    pub title: String,
    pub machines: Vec<Machine>,
    pub status_counts: HashMap<String, usize>,
    pub status_counts_json: String,
    pub theme: String,
    pub is_authenticated: bool,
    pub display_dates: HashMap<String, String>,
}

#[derive(Serialize)]
pub struct MachineListTemplate {
    pub machines: Vec<Machine>,
    pub theme: String,
    pub is_authenticated: bool,
    pub is_admin: bool,
    pub workflow_infos: HashMap<uuid::Uuid, crate::tinkerbell::WorkflowInfo>,
}

#[derive(Serialize)]
pub struct MachineDetailsTemplate {
    pub machine: Machine,
    pub theme: String,
    pub is_authenticated: bool,
    pub created_at_formatted: String,
    pub updated_at_formatted: String,
    pub workflow_info: Option<crate::tinkerbell::WorkflowInfo>,
}

#[derive(Serialize)]
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

#[derive(Serialize)]
pub struct WorkflowProgressTemplate {
    pub id: String,
    pub current_task_name: String,
    pub current_action_index: i64,
    pub current_action_name: String,
    pub current_action_status: String,
    pub total_number_of_actions: i64,
}

#[derive(Serialize)]
pub struct WelcomeTemplate {
    pub theme: String,
    pub is_authenticated: bool,
}

#[derive(Serialize)]
pub struct ErrorTemplate {
    pub theme: String,
    pub is_authenticated: bool,
    pub title: String,
    pub message: String,
    pub error_details: String,
    pub back_url: String,
    pub back_text: String,
    pub show_retry: bool,
    pub retry_url: String,
}

// Updated render_minijinja function
fn render_minijinja<T: Serialize>(
    app_state: &crate::AppState,
    template_name: &str, 
    context: T
) -> Response {
    // Get the environment based on the mode (static or reloading)
    let render_result = match &app_state.template_env {
        crate::TemplateEnv::Static(env) => {
            env.get_template(template_name)
               .and_then(|tmpl| tmpl.render(context))
        }
        #[cfg(debug_assertions)]
        crate::TemplateEnv::Reloading(reloader) => {
            // Acquire the environment from the reloader
            match reloader.acquire_env() {
                Ok(env) => {
                    env.get_template(template_name)
                       .and_then(|tmpl| tmpl.render(context))
                }
                Err(e) => {
                    error!("Failed to acquire MiniJinja env from reloader: {}", e);
                    // Convert minijinja::Error to rendering result error
                    Err(MiniJinjaError::new(MiniJinjaErrorKind::InvalidOperation, 
                        format!("Failed to acquire env from reloader: {}", e)))
                }
            }
        }
    };

    // Handle the final rendering result
    match render_result {
        Ok(html) => Html(html).into_response(),
        Err(e) => {
            error!("MiniJinja render/load error for {}: {}", template_name, e);
            (StatusCode::INTERNAL_SERVER_ERROR, format!("Template error: {}", e)).into_response()
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
        .route("/welcome", get(welcome_page))
        .route("/setup", get(welcome_page)) // Alias for welcome
        .route("/setup/simple", get(setup_simple))
        .route("/setup/flight", get(setup_flight))
        .route("/setup/swarm", get(setup_swarm))
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
    // Check if we're in setup mode from command line or this is the first run
    if app_state.setup_mode || app_state.first_run {
        return Redirect::to("/welcome").into_response();
    }
    
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
            
            // Replace Askama render with placeholder
            let context = IndexTemplate {
                title: "Dragonfly".to_string(),
                machines,
                status_counts,
                status_counts_json,
                theme,
                is_authenticated,
                display_dates,
            };
            // Pass AppState to render_minijinja
            render_minijinja(&app_state, "index.html", context)
        },
        Err(e) => {
            error!("Error fetching machines for index page: {}", e);
            // Replace Askama render with placeholder
            let context = IndexTemplate {
                title: "Dragonfly".to_string(),
                machines: vec![],
                status_counts: HashMap::new(),
                status_counts_json: "{}".to_string(),
                theme: "system".to_string(),
                is_authenticated,
                display_dates: HashMap::new(),
            };
            // Pass AppState to render_minijinja
            render_minijinja(&app_state, "index.html", context)
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

            // Replace Askama render with placeholder
            let context = MachineListTemplate {
                machines,
                theme,
                is_authenticated,
                is_admin,
                workflow_infos,
            };
            // Pass AppState to render_minijinja
            render_minijinja(&app_state, "machine_list.html", context)
        },
        Err(e) => {
            error!("Error fetching machines for machine list page: {}", e);
            // Replace Askama render with placeholder
            let context = MachineListTemplate {
                machines: vec![],
                theme,
                is_authenticated,
                is_admin,
                workflow_infos: HashMap::new(),
            };
            // Pass AppState to render_minijinja
            render_minijinja(&app_state, "machine_list.html", context)
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
                    
                    // Replace Askama render with placeholder
                    let context = MachineDetailsTemplate {
                        machine,
                        theme,
                        is_authenticated,
                        created_at_formatted,
                        updated_at_formatted,
                        workflow_info,
                    };
                    // Pass AppState to render_minijinja
                    render_minijinja(&app_state, "machine_details.html", context)
                },
                Ok(None) => {
                    error!("Machine not found: {}", uuid);
                    // Replace Askama render with placeholder
                    let context = IndexTemplate {
                        title: "Dragonfly - Machine Not Found".to_string(),
                        machines: vec![],
                        status_counts: HashMap::new(),
                        status_counts_json: "{}".to_string(),
                        theme: "system".to_string(),
                        is_authenticated,
                        display_dates: HashMap::new(),
                    };
                    // Pass AppState to render_minijinja
                    render_minijinja(&app_state, "index.html", context)
                },
                Err(e) => {
                    error!("Error fetching machine {}: {}", uuid, e);
                    // Replace Askama render with placeholder
                    let context = IndexTemplate {
                        title: "Dragonfly - Error".to_string(),
                        machines: vec![],
                        status_counts: HashMap::new(),
                        status_counts_json: "{}".to_string(),
                        theme: "system".to_string(),
                        is_authenticated,
                        display_dates: HashMap::new(),
                    };
                    // Pass AppState to render_minijinja
                    render_minijinja(&app_state, "index.html", context)
                }
            }
        },
        Err(e) => {
            error!("Invalid UUID: {}", e);
            // Replace Askama render with placeholder
            let context = IndexTemplate {
                title: "Dragonfly - Invalid UUID".to_string(),
                machines: vec![],
                status_counts: HashMap::new(),
                status_counts_json: "{}".to_string(),
                theme: "system".to_string(),
                is_authenticated,
                display_dates: HashMap::new(),
            };
            // Pass AppState to render_minijinja
            render_minijinja(&app_state, "index.html", context)
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
    
    // Replace Askama render with placeholder
    let context = SettingsTemplate {
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
    };
    // Pass AppState to render_minijinja
    render_minijinja(&app_state, "settings.html", context)
}

#[derive(serde::Deserialize)]
pub struct SettingsForm {
    pub theme: String,
    pub require_login: Option<String>,
    pub default_os: Option<String>,
    pub username: Option<String>,
    pub admin_password: Option<String>,
    pub password_confirm: Option<String>,
    pub setup_completed: Option<String>,
}

// Handler for settings form submission
pub async fn update_settings(
    State(app_state): State<crate::AppState>,
    mut auth_session: AuthSession,
    Form(form): Form<SettingsForm>,
) -> Response {
    let is_authenticated = auth_session.user.is_some();
    let theme = form.theme.clone();
    
    // Only require admin authentication for admin settings
    // If trying to change admin settings but not authenticated, redirect to login
    if (form.require_login.is_some() || 
        form.default_os.is_some() || 
        form.username.is_some() || 
        form.admin_password.is_some() || 
        form.password_confirm.is_some() ||
        form.setup_completed.is_some()) && !is_authenticated {
        return Redirect::to("/login").into_response();
    }

    // Only update admin settings if user is authenticated
    if is_authenticated {
        // Load current settings to get existing setup_completed value
        let current_settings = match get_app_settings().await {
            Ok(settings) => settings,
            Err(e) => {
                error!("Failed to load current settings: {}", e);
                // Return an error response or use defaults
                Settings::default()
            }
        };

        // Construct the new settings, preserving existing setup_completed
        let new_settings = Settings {
            require_login: form.require_login.is_some(),
            // Handle optional default_os correctly by filtering out empty strings
            default_os: form.default_os.filter(|os| !os.is_empty()),
            // Use the setup_completed value from the form if present (checkbox is checked),
            // otherwise keep the current value from the database.
            setup_completed: form.setup_completed.is_some().then_some(true).unwrap_or(current_settings.setup_completed),
        };

        // Save the general settings
        if let Err(e) = save_app_settings(&new_settings).await {
            error!("Failed to save settings: {}", e);
            // Handle error, maybe return an error message to the user
            // For now, just log and continue
        }

        // Update admin password if provided and confirmed
        // Check form.admin_password instead of form.password
        if let (Some(password), Some(confirm)) = (&form.admin_password, &form.password_confirm) {
            if !password.is_empty() && password == confirm {
                // Load current credentials to get username (or use default 'admin')
                let username = match auth::load_credentials().await {
                    Ok(creds) => creds.username,
                    Err(_) => {
                        warn!("Could not load current credentials, defaulting username to 'admin' for password change.");
                        "admin".to_string()
                    }
                };

                // Hash the new password
                match Credentials::create(username, password.clone()) {
                    Ok(new_creds) => {
                        if let Err(e) = auth::save_credentials(&new_creds).await {
                            error!("Failed to save new admin password: {}", e);
                            // Handle credential saving error
                        } else {
                            // Password updated successfully, delete initial password file if it exists
                            if std::path::Path::new("initial_password.txt").exists() {
                                if let Err(e) = std::fs::remove_file("initial_password.txt") {
                                    warn!("Failed to remove initial_password.txt: {}", e);
                                }
                            }
                            // Force logout after password change
                            let _ = auth_session.logout().await;
                            return Redirect::to("/login?message=password_updated").into_response();
                        }
                    }
                    Err(e) => {
                        error!("Failed to hash new password: {}", e);
                        // Handle hashing error (e.g., display message to user)
                    }
                }
            }
        }
    }

    // Theme can be updated by all users (even non-authenticated)
    // Create cookie with proper builder pattern
    let mut cookie = Cookie::new("dragonfly_theme", theme);
    cookie.set_path("/");
    cookie.set_max_age(time::Duration::days(365));
    cookie.set_same_site(SameSite::Lax);
    
    // Set cookie header and redirect back to settings page
    (
        [(header::SET_COOKIE, cookie.to_string())],
        Redirect::to("/settings")
    ).into_response()
}

// New handler for welcome page
pub async fn welcome_page(
    State(app_state): State<crate::AppState>,
    headers: HeaderMap,
    auth_session: AuthSession,
) -> Response {
    // Get theme preference from cookie
    let theme = get_theme_from_cookie(&headers);
    let is_authenticated = auth_session.user.is_some();
    
    // Replace Askama render with placeholder
    let context = WelcomeTemplate {
        theme,
        is_authenticated,
    };
    // Pass AppState to render_minijinja
    render_minijinja(&app_state, "welcome.html", context)
}

// Handlers for the different setup modes
pub async fn setup_simple(
    State(app_state): State<crate::AppState>,
    headers: HeaderMap,
    auth_session: AuthSession,
) -> Response {
    // Get theme preference from cookie
    let theme = get_theme_from_cookie(&headers);
    let is_authenticated = auth_session.user.is_some();
    
    // Configure the system for Simple mode
    match mode::configure_simple_mode().await {
        Ok(_) => {
            info!("System configured for Simple mode");
            
            // Fix this: Mark setup as completed by passing bool instead of &AppState
            if let Err(e) = mark_setup_completed(true).await {
                error!("Failed to mark setup as completed: {}", e);
            } else {
                info!("Setup marked as completed");
                
                // Also update the in-memory settings
                let mut settings = app_state.settings.lock().await;
                settings.setup_completed = true;
            }
            
            // Redirect to main page
            Redirect::to("/").into_response()
        },
        Err(e) => {
            error!("Failed to configure system for Simple mode: {}", e);
            
            // Replace Askama render with placeholder
            let context = ErrorTemplate {
                theme,
                is_authenticated,
                title: "Setup Failed".to_string(),
                message: "There was a problem setting up Simple mode.".to_string(),
                error_details: format!("{}", e),
                back_url: "/".to_string(),
                back_text: "Back to Dashboard".to_string(),
                show_retry: true,
                retry_url: "/setup/simple".to_string(),
            };
            // Pass AppState to render_minijinja
            render_minijinja(&app_state, "error.html", context)
        }
    }
}

pub async fn setup_flight(
    State(app_state): State<crate::AppState>,
    headers: HeaderMap,
    auth_session: AuthSession,
) -> Response {
    // Get theme preference from cookie
    let theme = get_theme_from_cookie(&headers);
    let is_authenticated = auth_session.user.is_some();
    
    // Configure the system for Flight mode
    match mode::configure_flight_mode().await {
        Ok(_) => {
            info!("System configured for Flight mode - k3s deployment started in background");
            
            // Fix this: Mark setup as completed by passing bool instead of &AppState
            if let Err(e) = mark_setup_completed(true).await {
                error!("Failed to mark setup as completed: {}", e);
            } else {
                info!("Setup marked as completed");
                
                // Also update the in-memory settings
                let mut settings = app_state.settings.lock().await;
                settings.setup_completed = true;
            }

            // Redirect to a flight status page that shows installation progress
            // For now, just redirect to main page
            Redirect::to("/").into_response()
        },
        Err(e) => {
            error!("Failed to configure system for Flight mode: {}", e);
            
            // Replace Askama render with placeholder
            let context = ErrorTemplate {
                theme,
                is_authenticated,
                title: "Setup Failed".to_string(),
                message: "There was a problem setting up Flight mode.".to_string(),
                error_details: format!("{}", e),
                back_url: "/".to_string(),
                back_text: "Back to Dashboard".to_string(),
                show_retry: true,
                retry_url: "/setup/flight".to_string(),
            };
            // Pass AppState to render_minijinja
            render_minijinja(&app_state, "error.html", context)
        }
    }
}

pub async fn setup_swarm(
    State(app_state): State<crate::AppState>,
    headers: HeaderMap,
    auth_session: AuthSession,
) -> Response {
    // Get theme preference from cookie
    let theme = get_theme_from_cookie(&headers);
    let is_authenticated = auth_session.user.is_some();
    
    // Configure the system for Swarm mode
    match mode::configure_swarm_mode().await {
        Ok(_) => {
            info!("System configured for Swarm mode");
            
            // Fix this: Mark setup as completed by passing bool instead of &AppState
            if let Err(e) = mark_setup_completed(true).await {
                error!("Failed to mark setup as completed: {}", e);
            } else {
                info!("Setup marked as completed");
                
                // Also update the in-memory settings
                let mut settings = app_state.settings.lock().await;
                settings.setup_completed = true;
            }
            
            // Redirect to main page
            Redirect::to("/").into_response()
        },
        Err(e) => {
            error!("Failed to configure system for Swarm mode: {}", e);
            
            // Replace Askama render with placeholder
            let context = ErrorTemplate {
                theme,
                is_authenticated,
                title: "Setup Failed".to_string(),
                message: "There was a problem setting up Swarm mode.".to_string(),
                error_details: format!("{}", e),
                back_url: "/".to_string(),
                back_text: "Back to Dashboard".to_string(),
                show_retry: true,
                retry_url: "/setup/swarm".to_string(),
            };
            // Pass AppState to render_minijinja
            render_minijinja(&app_state, "error.html", context)
        }
    }
} 