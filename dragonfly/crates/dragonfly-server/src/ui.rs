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
use time;
use cookie::{Cookie, SameSite};
use std::fs;

use crate::db;
use crate::auth::{AuthSession, Settings, save_settings};

// Filters for Askama templates
mod filters {
    use askama::Result;

    pub fn length<T>(collection: &[T]) -> Result<usize> {
        Ok(collection.len())
    }
    
    pub fn string<T: std::fmt::Display>(value: T) -> Result<String> {
        Ok(format!("{}", value))
    }

    pub fn join_vec(vec: &[String], separator: &str) -> Result<String> {
        Ok(vec.join(separator))
    }
    
    // Helper to safely unwrap Option<String> values in templates
    pub fn unwrap_or<'a>(opt: &'a Option<String>, default: &'a str) -> Result<&'a str> {
        match opt {
            Some(s) => Ok(s.as_str()),
            None => Ok(default),
        }
    }
}

// Extract theme from cookies
fn get_theme_from_cookie(headers: &HeaderMap) -> String {
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
}

#[derive(Template)]
#[template(path = "machine_list.html")]
pub struct MachineListTemplate {
    pub machines: Vec<Machine>,
    pub theme: String,
    pub is_authenticated: bool,
}

#[derive(Template)]
#[template(path = "machine_details.html")]
pub struct MachineDetailsTemplate {
    pub machine: Machine,
    pub theme: String,
    pub is_authenticated: bool,
}

#[derive(Template)]
#[template(path = "settings.html")]
pub struct SettingsTemplate {
    pub theme: String,
    pub is_authenticated: bool,
    pub admin_username: String,
    pub require_login: bool,
    pub has_initial_password: bool,
    pub rendered_password: String,
    pub show_admin_settings: bool,
    pub error_message: Option<String>,
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

pub async fn index(
    headers: HeaderMap,
    auth_session: AuthSession,
) -> Response {
    // Get theme preference from cookie
    let theme = get_theme_from_cookie(&headers);
    let is_authenticated = auth_session.user.is_some();
    
    match db::get_all_machines().await {
        Ok(machines) => {
            info!("Rendering index page with {} machines", machines.len());
            
            // Count machines by status
            let status_counts = count_machines_by_status(&machines);
            
            // Convert status counts to JSON for the chart
            let status_counts_json = serde_json::to_string(&status_counts)
                .unwrap_or_else(|_| "{}".to_string());
            
            UiTemplate::Index(IndexTemplate {
                title: "Dragonfly".to_string(),
                machines,
                status_counts,
                status_counts_json,
                theme,
                is_authenticated,
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
            }).into_response()
        }
    }
}

pub async fn machine_list(
    headers: HeaderMap,
    auth_session: AuthSession,
) -> Response {
    // Get theme preference from cookie
    let theme = get_theme_from_cookie(&headers);
    let is_authenticated = auth_session.user.is_some();
    
    match db::get_all_machines().await {
        Ok(machines) => {
            // Only log if we actually have machines to report
            if !machines.is_empty() {
                info!("Found {} machines", machines.len());
            }
            
            UiTemplate::MachineList(MachineListTemplate { 
                machines,
                theme,
                is_authenticated,
            }).into_response()
        },
        Err(e) => {
            error!("Error fetching machines for machine list page: {}", e);
            UiTemplate::MachineList(MachineListTemplate { 
                machines: vec![],
                theme: "system".to_string(),
                is_authenticated,
            }).into_response()
        }
    }
}

pub async fn machine_details(
    axum::extract::Path(id): axum::extract::Path<String>,
    headers: HeaderMap,
    auth_session: AuthSession,
) -> Response {
    // Get theme preference from cookie
    let theme = get_theme_from_cookie(&headers);
    let is_authenticated = auth_session.user.is_some();
    
    // Parse UUID from string
    match uuid::Uuid::parse_str(&id) {
        Ok(uuid) => {
            // Get machine by ID
            match db::get_machine_by_id(&uuid).await {
                Ok(Some(machine)) => {
                    info!("Rendering machine details page for machine {}", uuid);
                    UiTemplate::MachineDetails(MachineDetailsTemplate { 
                        machine,
                        theme,
                        is_authenticated,
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
    let show_admin_settings = is_authenticated;
    
    // Get admin username if authenticated
    let admin_username = if let Some(user) = &auth_session.user {
        user.username.clone()
    } else {
        "admin".to_string()
    };
    
    // Get current settings
    let require_login = app_state.settings.lock().await.require_login;
    
    // Check if initial password file exists (only for admins)
    let (has_initial_password, rendered_password) = if is_authenticated {
        match fs::read_to_string(".admin_password.txt") {
            Ok(password) => (true, password),
            Err(_) => (false, String::new()),
        }
    } else {
        (false, String::new())
    };
    
    UiTemplate::Settings(SettingsTemplate {
        theme,
        is_authenticated,
        admin_username,
        require_login,
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
    // Update theme preference (allowed for all users)
    let theme = form.theme.clone();
    let mut cookie = Cookie::new("dragonfly_theme", theme);
    cookie.set_path("/");
    cookie.set_max_age(time::Duration::days(365));
    cookie.set_http_only(true);
    cookie.set_same_site(SameSite::Lax);
    
    // Apply admin settings if authenticated
    if auth_session.user.is_some() {
        // Update require_login setting
        let mut settings = app_state.settings.lock().await;
        settings.require_login = form.require_login.is_some();
        drop(settings);
        
        // Save settings to disk
        let _ = save_settings(&Settings {
            require_login: form.require_login.is_some(),
        });
        
        // Update admin credentials if old password and new password are provided
        if let (Some(old_password), Some(password), Some(username)) = (form.old_password, form.password.clone(), form.username) {
            if !password.is_empty() {
                // Verify old password first
                let user = auth_session.user.as_ref().unwrap();
                
                // Create credentials to verify
                let verify_creds = crate::auth::Credentials {
                    username: user.username.clone(),
                    password: Some(old_password),
                    password_hash: String::new(),
                };
                
                // Attempt to verify the old password
                match app_state.auth_backend.verify_credentials(verify_creds).await {
                    Ok(true) => {
                        // Old password verified, update to new password
                        let _ = app_state.auth_backend.update_credentials(username, password).await;
                        
                        // Delete the initial password file if it exists
                        let _ = fs::remove_file(".admin_password.txt");
                    },
                    _ => {
                        // Old password verification failed
                        return Redirect::to("/settings?error=invalid_password").into_response();
                    }
                }
            }
        }
    }
    
    // Set cookie and redirect to settings
    (
        [(header::SET_COOKIE, cookie.to_string())],
        Redirect::to("/settings")
    ).into_response()
} 