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
use std::sync::Arc;
use serde_json;
use uuid;
use time;
use cookie::{Cookie, SameSite};
use tokio::sync::Mutex;
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

// Enum for theme options
#[derive(Debug, Clone, PartialEq)]
pub enum Theme {
    Light,
    Dark,
    System,
}

impl Theme {
    pub fn from_str(s: &str) -> Self {
        match s {
            "dark" => Theme::Dark,
            "light" => Theme::Light,
            _ => Theme::System,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Theme::Dark => "dark",
            Theme::Light => "light",
            Theme::System => "system",
        }
    }
}

// Extract theme from cookies
fn get_theme_from_cookie(headers: &axum::http::HeaderMap) -> Theme {
    if let Some(cookie_header) = headers.get(header::COOKIE) {
        if let Ok(cookie_str) = cookie_header.to_str() {
            for cookie_pair in cookie_str.split(';') {
                let cookie = Cookie::parse(cookie_pair.trim()).ok();
                if let Some(c) = cookie {
                    if c.name() == "dragonfly_theme" {
                        return Theme::from_str(c.value());
                    }
                }
            }
        }
    }
    Theme::System
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
    #[template(escape = "none")]
    pub initial_password: Option<String>,
}

enum UiTemplate {
    Index(IndexTemplate),
    MachineList(MachineListTemplate),
    MachineDetails(MachineDetailsTemplate),
    Settings(SettingsTemplate),
}

impl IntoResponse for UiTemplate {
    fn into_response(self) -> axum::response::Response {
        match self {
            UiTemplate::Index(template) => HtmlTemplate(template).into_response(),
            UiTemplate::MachineList(template) => HtmlTemplate(template).into_response(),
            UiTemplate::MachineDetails(template) => HtmlTemplate(template).into_response(),
            UiTemplate::Settings(template) => HtmlTemplate(template).into_response(),
        }
    }
}

// Helper struct to implement IntoResponse for templates
struct HtmlTemplate<T>(T);

impl<T> IntoResponse for HtmlTemplate<T>
where
    T: Template,
{
    fn into_response(self) -> axum::response::Response {
        match self.0.render() {
            Ok(html) => Html(html).into_response(),
            Err(err) => {
                eprintln!("Template error: {}", err);
                StatusCode::INTERNAL_SERVER_ERROR.into_response()
            }
        }
    }
}

pub fn ui_router() -> Router {
    Router::new()
        .route("/", get(index))
        .route("/machines", get(machine_list))
        .route("/machines/:id", get(machine_details))
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
    headers: axum::http::HeaderMap,
    auth_session: AuthSession,
) -> impl IntoResponse {
    // Get theme preference from cookie
    let theme = get_theme_from_cookie(&headers).as_str();
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
                theme: theme.to_string(),
                is_authenticated,
            })
        },
        Err(e) => {
            error!("Error fetching machines for index page: {}", e);
            UiTemplate::Index(IndexTemplate {
                title: "Dragonfly".to_string(),
                machines: vec![],
                status_counts: HashMap::new(),
                status_counts_json: "{}".to_string(),
                theme: theme.to_string(),
                is_authenticated,
            })
        }
    }
}

pub async fn machine_list(
    headers: axum::http::HeaderMap,
    auth_session: AuthSession,
) -> impl IntoResponse {
    // Get theme preference from cookie
    let theme = get_theme_from_cookie(&headers).as_str();
    let is_authenticated = auth_session.user.is_some();
    
    match db::get_all_machines().await {
        Ok(machines) => {
            // Only log if we actually have machines to report
            if !machines.is_empty() {
                info!("Found {} machines", machines.len());
            }
            
            UiTemplate::MachineList(MachineListTemplate { 
                machines,
                theme: theme.to_string(),
                is_authenticated,
            })
        },
        Err(e) => {
            error!("Error fetching machines for machine list page: {}", e);
            UiTemplate::MachineList(MachineListTemplate { 
                machines: vec![],
                theme: theme.to_string(),
                is_authenticated,
            })
        }
    }
}

pub async fn machine_details(
    axum::extract::Path(id): axum::extract::Path<String>,
    headers: axum::http::HeaderMap,
    auth_session: AuthSession,
) -> impl IntoResponse {
    // Get theme preference from cookie
    let theme = get_theme_from_cookie(&headers).as_str();
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
                        theme: theme.to_string(),
                        is_authenticated,
                    })
                },
                Ok(None) => {
                    error!("Machine not found: {}", uuid);
                    // Return to index page with error
                    UiTemplate::Index(IndexTemplate {
                        title: "Dragonfly - Machine Not Found".to_string(),
                        machines: vec![],
                        status_counts: HashMap::new(),
                        status_counts_json: "{}".to_string(),
                        theme: theme.to_string(),
                        is_authenticated,
                    })
                },
                Err(e) => {
                    error!("Error fetching machine {}: {}", uuid, e);
                    // Return to index page with error
                    UiTemplate::Index(IndexTemplate {
                        title: "Dragonfly - Error".to_string(),
                        machines: vec![],
                        status_counts: HashMap::new(),
                        status_counts_json: "{}".to_string(),
                        theme: theme.to_string(),
                        is_authenticated,
                    })
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
                theme: theme.to_string(),
                is_authenticated,
            })
        }
    }
}

// Handler for theme toggling
pub async fn toggle_theme(
    Query(params): Query<HashMap<String, String>>,
) -> impl IntoResponse {
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
        axum::response::Redirect::to(&return_to)
    )
}

// Handler for the settings page
pub async fn settings_page(
    headers: axum::http::HeaderMap,
    auth_session: AuthSession,
    settings_state: State<Arc<Mutex<Settings>>>,
) -> impl IntoResponse {
    if !auth_session.user.is_some() {
        return Redirect::to("/login").into_response();
    }
    
    // Get current theme from cookie
    let theme = get_theme_from_cookie(&headers).as_str();
    
    // Get current admin username
    let admin_username = if let Some(user) = &auth_session.user {
        user.username.clone()
    } else {
        "admin".to_string()
    };
    
    // Get current settings
    let settings_guard = settings_state.lock().await;
    let require_login = settings_guard.require_login;
    drop(settings_guard);
    
    let is_authenticated = auth_session.user.is_some();
    
    // Check if initial password file exists
    let initial_password = match fs::read_to_string(".admin_password.txt") {
        Ok(password) => Some(password),
        Err(_) => None,
    };
    
    HtmlTemplate(SettingsTemplate {
        theme: theme.to_string(),
        is_authenticated,
        admin_username,
        require_login,
        initial_password,
    }).into_response()
}

#[derive(serde::Deserialize)]
pub struct SettingsForm {
    pub theme: String,
    pub require_login: Option<String>,
    pub username: String,
    pub password: Option<String>,
    pub password_confirm: Option<String>,
}

// Handler for settings form submission
pub async fn update_settings(
    Form(form): Form<SettingsForm>,
    headers: axum::http::HeaderMap,
    auth_session: AuthSession,
    backend: State<crate::auth::AdminBackend>,
    settings_state: State<Arc<Mutex<Settings>>>,
) -> impl IntoResponse {
    if !auth_session.user.is_some() {
        return Redirect::to("/login").into_response();
    }
    
    // Update theme preference
    let theme = form.theme.clone();
    let mut cookie = Cookie::new("dragonfly_theme", theme);
    cookie.set_path("/");
    cookie.set_max_age(time::Duration::days(365));
    cookie.set_http_only(true);
    cookie.set_same_site(SameSite::Lax);
    
    // Update settings
    let mut settings = settings_state.lock().await;
    settings.require_login = form.require_login.is_some();
    drop(settings);
    
    // Save settings to disk
    let _ = save_settings(&Settings {
        require_login: form.require_login.is_some(),
    });
    
    // Update admin credentials if a password was provided
    if let Some(password) = form.password {
        if !password.is_empty() {
            // Update credentials
            let _ = backend.update_credentials(form.username, password).await;
            
            // Delete the initial password file if it exists
            let _ = fs::remove_file(".admin_password.txt");
        }
    }
    
    // Set cookie and redirect to settings
    (
        [(header::SET_COOKIE, cookie.to_string())],
        Redirect::to("/settings")
    ).into_response()
} 