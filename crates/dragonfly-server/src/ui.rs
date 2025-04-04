use axum::{
    extract::{Query, State},
    http::{header, HeaderMap, StatusCode},
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
    Form, Router,
};
use dragonfly_common::models::{Machine, MachineStatus, DiskInfo};
use tracing::{error, info, warn};
use std::collections::HashMap;
use chrono::{DateTime, Utc, TimeZone};
use cookie::{Cookie, SameSite};
use std::fs;
use serde::Serialize;
use crate::db::{self, get_app_settings, save_app_settings, mark_setup_completed};
use crate::auth::{self, AuthSession, Settings, Credentials};
use crate::mode;
use minijinja::{Error as MiniJinjaError, ErrorKind as MiniJinjaErrorKind};
use std::sync::Arc;
use std::net::{IpAddr, Ipv4Addr};
use crate::tinkerbell::WorkflowInfo;
use uuid::Uuid;

// Import global state
use crate::{AppState, INSTALL_STATE_REF, InstallationState};

// Import format_os_name from api.rs
use crate::api::{format_os_name, get_os_icon, get_os_info};

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

// Update struct for MiniJinja context, matching data from api.rs handler
#[derive(Serialize)] // Use Serialize for MiniJinja
pub struct WorkflowProgressTemplate {
    // Fields provided by get_workflow_progress in api.rs
    pub machine_id: Uuid,
    pub workflow_info: WorkflowInfo, // Not Option<> as api.rs ensures it exists before calling render
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
    pub installation_in_progress: bool,
    pub initial_install_message: String,
    pub initial_animation_class: String,
    pub is_demo_mode: bool,
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
    pub machine_json: String,
    pub theme: String,
    pub is_authenticated: bool,
    pub created_at_formatted: String,
    pub updated_at_formatted: String,
    pub workflow_info_json: String,
    pub machine: Machine,
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
pub struct WelcomeTemplate {
    pub theme: String,
    pub is_authenticated: bool,
    pub hide_footer: bool,
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
pub fn render_minijinja<T: Serialize>(
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

// Function to generate demo machines
fn generate_demo_machines() -> Vec<Machine> {
    let mut machines = Vec::new();
    let base_time = Utc.with_ymd_and_hms(2023, 4, 15, 12, 0, 0).unwrap();
    let base_mac = [0x52, 0x54, 0x00, 0xAB, 0xCD, 0x00];
    let base_ip = Ipv4Addr::new(10, 0, 42, 0);

    // Generate topaz-control[01:03]
    for i in 1..=3 {
        let hostname = format!("topaz-control{:02}", i);
        let mac_suffix = i as u8;
        let ip_suffix = 10 + i as u8;
        machines.push(create_demo_machine(
            &hostname, 
            base_mac, 
            mac_suffix, 
            base_ip, 
            ip_suffix, 
            base_time.clone(), 
            MachineStatus::Ready,
            Some(500), // 500GB disk
        ));
    }

    // Generate topaz-worker[01:06]
    for i in 1..=6 {
        let hostname = format!("topaz-worker{:02}", i);
        let mac_suffix = 10 + i as u8;
        let ip_suffix = 20 + i as u8;
        machines.push(create_demo_machine(
            &hostname, 
            base_mac, 
            mac_suffix, 
            base_ip, 
            ip_suffix, 
            base_time.clone(), 
            MachineStatus::Ready,
            Some(2000), // 2TB disk
        ));
    }

    // Generate cubefs-master[01:03]
    for i in 1..=3 {
        let hostname = format!("cubefs-master{:02}", i);
        let mac_suffix = 20 + i as u8;
        let ip_suffix = 30 + i as u8;
        machines.push(create_demo_machine(
            &hostname, 
            base_mac, 
            mac_suffix,
            base_ip, 
            ip_suffix, 
            base_time.clone(), 
            MachineStatus::Ready,
            Some(500), // 500GB disk
        ));
    }

    // Generate cubefs-datanode[01:06]
    for i in 1..=6 {
        let hostname = format!("cubefs-datanode{:02}", i);
        let mac_suffix = 30 + i as u8;
        let ip_suffix = 40 + i as u8;
        let status = if i <= 5 { 
            MachineStatus::Ready 
        } else { 
            // Make one datanode show as "installing" for variety
            MachineStatus::InstallingOS 
        };
        machines.push(create_demo_machine(
            &hostname, 
            base_mac, 
            mac_suffix, 
            base_ip, 
            ip_suffix, 
            base_time.clone(), 
            status,
            Some(4000), // 4TB disk
        ));
    }

    machines
}

// Helper function to create a demo machine
fn create_demo_machine(
    hostname: &str,
    base_mac: [u8; 6],
    mac_suffix: u8,
    base_ip: Ipv4Addr,
    ip_suffix: u8,
    base_time: DateTime<Utc>,
    status: MachineStatus,
    disk_size_gb: Option<u64>,
) -> Machine {
    // Generate a deterministic UUID based on hostname
    let mut mac = base_mac;
    mac[5] = mac_suffix;
    
    // Use UUID v5 to create a deterministic UUID from the hostname
    // This allows machine details to be found consistently in demo mode
    let namespace = uuid::Uuid::NAMESPACE_DNS;
    let uuid = uuid::Uuid::new_v5(&namespace, hostname.as_bytes());
    let created_at = base_time + chrono::Duration::minutes(mac_suffix as i64);
    let updated_at = created_at + chrono::Duration::hours(1);
    
    let mut ip_octets = base_ip.octets();
    ip_octets[3] = ip_suffix;
    let ip = IpAddr::V4(Ipv4Addr::from(ip_octets));

    // Format MAC address with colons
    let mac_string = format!(
        "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
    );

    // Generate memorable name using BIP39 words based on MAC address
    let memorable_name = dragonfly_common::mac_to_words::mac_to_words_safe(&mac_string);

    // Create a disk to match the requested disk size
    let disk = DiskInfo {
        device: format!("/dev/sda"),
        size_bytes: disk_size_gb.unwrap_or(500) * 1_073_741_824, // Convert GB to bytes
        model: Some(format!("Demo Disk {}", disk_size_gb.unwrap_or(500))),
        calculated_size: Some(format!("{} GB", disk_size_gb.unwrap_or(500))),
    };

    // Create the machine with the correct fields
    Machine {
        id: uuid,
        hostname: Some(hostname.to_string()),
        mac_address: mac_string,
        ip_address: ip.to_string(), // No Option<> here, ip_address is a String
        status,
        os_choice: Some("ubuntu-2204".to_string()),
        os_installed: Some("Ubuntu 22.04".to_string()),
        disks: vec![disk],
        nameservers: vec!["8.8.8.8".to_string(), "1.1.1.1".to_string()],
        memorable_name: Some(memorable_name),
        created_at,
        updated_at,
        bmc_credentials: None,
        installation_progress: 0,
        installation_step: None,
        last_deployment_duration: None,
        // Initialize new hardware fields to None for demo data
        cpu_model: None,
        cpu_cores: None,
        total_ram_bytes: None,
    }
}

#[axum::debug_handler]
pub async fn index(
    State(app_state): State<AppState>,
    headers: HeaderMap,
    auth_session: AuthSession,
) -> Response {
    let theme = get_theme_from_cookie(&headers);
    let is_authenticated = auth_session.user.is_some();
    let require_login = app_state.settings.lock().await.require_login;

    // --- Scenario B Logic --- 
    if app_state.is_demo_mode {
        // Case B.3: Not installed (or explicitly demo) -> Show Demo Experience
        info!("Rendering Demo Experience (root route)");
        // The rest of this function will now handle rendering the demo dashboard
        // Ensure is_demo_mode is passed to the template
    } else if app_state.is_installed {
        // Case B.1 & B.2: Installed
        let current_mode = mode::get_current_mode().await.unwrap_or(None);
        if current_mode.is_none() {
            // Case B.2: Installed, no mode selected -> Show Welcome Screen
            // BUT ONLY if not in installation server mode
            if !app_state.is_installation_server {
                info!("Installed, no mode selected, redirecting to /welcome");
                return Redirect::to("/welcome").into_response();
            } else {
                // We're in installation server mode, so we want to show the installation UI
                info!("Rendering installation UI");
            }
        } else {
            // Case B.1: Installed, mode selected -> Proceed to normal UI (Dashboard)
            info!("Installed, mode selected, rendering normal dashboard");
            // Login check happens *after* this block if needed
        }
    } else {
        // This case means it's *not* demo, *not* installed.
        // Check if it's the installation server running.
        if app_state.is_installation_server {
            // This is the expected state during installation. Proceed normally.
            // The template will handle showing the installation progress UI.
            info!("Install server running, rendering index page for installation progress.");
        } else {
            // It's NOT the install server, so this state is truly unexpected.
            warn!("Root route accessed in unexpected state (not demo, not installed, not install server). Rendering error.");
            let context = ErrorTemplate {
                theme,
                is_authenticated: false, // Assume not authenticated
                title: "Unexpected Server State".to_string(),
                message: "The server is in an unexpected state. Installation might be incomplete or the server requires setup.".to_string(),
                error_details: "Error code: UI_ROOT_UNEXPECTED_STATE_FINAL".to_string(), // Use a distinct code
                back_url: "/".to_string(),
                back_text: "Retry".to_string(),
                show_retry: true,
                retry_url: "/".to_string(),
            };
            return render_minijinja(&app_state, "error.html", context);
        }
    }
    // --- End Scenario B Logic ---

    // Login check (only applies if *not* demo and mode *is* selected)
    if require_login && !is_authenticated && app_state.is_installed {
        let current_mode = mode::get_current_mode().await.unwrap_or(None);
        if current_mode.is_some() { // Only redirect if mode is selected
             info!("Login required, redirecting to /login");
             return Redirect::to("/login").into_response();
        }
    }

    // --- Continue with Dashboard/Demo Rendering --- 
    let installation_in_progress = std::env::var("DRAGONFLY_INSTALL_SERVER_MODE").is_ok() || app_state.is_installation_server;
    let mut initial_install_message = String::new();
    let mut initial_animation_class = String::new();

    // If installing, get initial state
    if installation_in_progress {
        // Clone the Arc out of the RwLock guard before awaiting
        let install_state_arc_mutex: Option<Arc<tokio::sync::Mutex<InstallationState>>> = {
            INSTALL_STATE_REF.read().unwrap().as_ref().cloned()
        };

        if let Some(state_arc_mutex) = install_state_arc_mutex {
            let initial_state = state_arc_mutex.lock().await.clone(); 
            initial_install_message = initial_state.get_message().to_string();
            initial_animation_class = initial_state.get_animation_class().to_string();
        }
    }
    
    // Prepare context for the template
    // Fetch real/demo data based on app_state.is_demo_mode
    let (machines, status_counts, status_counts_json, display_dates) = if !installation_in_progress {
        if app_state.is_demo_mode { // Check the state flag now
            // In demo mode, generate fake demo machines
            let demo_machines = generate_demo_machines();
            let counts = count_machines_by_status(&demo_machines);
            let counts_json = serde_json::to_string(&counts).unwrap_or_else(|_| "{}".to_string());
            let dates = demo_machines.iter()
                .map(|mach| (mach.id.to_string(), format_datetime(&mach.created_at)))
                .collect();
            (demo_machines, counts, counts_json, dates)
        } else {
            // Normal mode - fetch real machines from database
            match db::get_all_machines().await {
                Ok(m) => {
                    let counts = count_machines_by_status(&m);
                    let counts_json = serde_json::to_string(&counts).unwrap_or_else(|_| "{}".to_string());
                    let dates = m.iter()
                        .map(|mach| (mach.id.to_string(), format_datetime(&mach.created_at)))
                        .collect();
                    (m, counts, counts_json, dates)
                },
                Err(e) => {
                    error!("Error fetching machines for index page: {}", e);
                    (vec![], HashMap::new(), "{}".to_string(), HashMap::new())
                }
            }
        }
    } else {
        // Provide empty defaults if installing
        (vec![], HashMap::new(), "{}".to_string(), HashMap::new())
    };

    let context = IndexTemplate {
        title: "Dragonfly".to_string(),
        machines,
        status_counts,
        status_counts_json,
        theme,
        is_authenticated,
        display_dates,
        installation_in_progress,
        initial_install_message,
        initial_animation_class,
        is_demo_mode: app_state.is_demo_mode, // Use the state flag
    };

    render_minijinja(&app_state, "index.html", context)
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

    // --- Scenario B: Mode/Install Check --- 
    // Redirect to welcome if installed but no mode selected
    if app_state.is_installed && !app_state.is_demo_mode { // Don't check mode if in demo
        let current_mode = mode::get_current_mode().await.unwrap_or(None);
        if current_mode.is_none() {
            info!("/machines accessed before mode selection, redirecting to /welcome");
            // Need to return a response that HTMX can use to redirect
            let mut response = Redirect::to("/welcome").into_response();
            response.headers_mut().insert("HX-Redirect", "/welcome".parse().unwrap());
            return response;
        }
    }
    // --- End Scenario B Check ---

    // Login check (applies to both normal and demo mode if require_login is true)
    if require_login && !is_authenticated {
        info!("Login required for /machines, redirecting to /login");
        // HTMX redirect
        let mut response = Redirect::to("/login").into_response();
        response.headers_mut().insert("HX-Redirect", "/login".parse().unwrap());
        return response;
    }

    // Determine if we are in demo mode (using the state flag)
    let is_demo_mode = app_state.is_demo_mode;

    // If in demo mode, show demo machines
    if is_demo_mode {
        // Generate demo machines
        let machines = generate_demo_machines();
        // Create an empty workflow info map
        let workflow_infos = HashMap::new();

        let context = MachineListTemplate {
            machines,
            theme,
            is_authenticated,
            is_admin,
            workflow_infos,
        };
        return render_minijinja(&app_state, "machine_list.html", context);
    } else { // Normal mode
        // Normal mode - fetch machines from database
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
    
    // Check if we are in demo mode
    let is_demo_mode = std::env::var("DRAGONFLY_DEMO_MODE").is_ok();
    
    // Parse UUID from string
    match uuid::Uuid::parse_str(&id) {
        Ok(uuid) => {
            // If in demo mode, find the machine in our demo dataset
            if is_demo_mode {
                let demo_machines = generate_demo_machines();
                // Use string comparison for more reliable matching in templates
                if let Some(machine) = demo_machines.iter().find(|m| m.id.to_string() == uuid.to_string()) {
                    let created_at_formatted = machine.created_at.format("%Y-%m-%d %H:%M:%S UTC").to_string();
                    let updated_at_formatted = machine.updated_at.format("%Y-%m-%d %H:%M:%S UTC").to_string();
                    
                    // Create a mock workflow info if the machine is in installing status
                    let workflow_info = if machine.status == MachineStatus::InstallingOS {
                        Some(crate::tinkerbell::WorkflowInfo {
                            state: "running".to_string(),
                            current_action: Some("Writing disk image".to_string()),
                            progress: 65,
                            tasks: vec![
                                crate::tinkerbell::TaskInfo {
                                    name: "Installing operating system".to_string(),
                                    status: "STATE_RUNNING".to_string(),
                                    started_at: (Utc::now() - chrono::Duration::minutes(15)).to_rfc3339(),
                                    duration: 900, // 15 minutes in seconds
                                    reported_duration: 900,
                                    estimated_duration: 1800, // 30 minutes in seconds
                                    progress: 65,
                                }
                            ],
                            estimated_completion: Some("About 10 minutes remaining".to_string()),
                            template_name: "ubuntu-2204".to_string(),
                        })
                    } else {
                        None
                    };

                    // Serialize machine and workflow_info to JSON strings
                    let machine_json = serde_json::to_string(machine)
                        .unwrap_or_else(|e| {
                            error!("Failed to serialize demo machine to JSON: {}", e);
                            "{}".to_string() // Default to empty JSON object on error
                        });
                    // ADD DEBUG LOG
                    info!("Serialized demo machine JSON: {}", machine_json);
                    
                    let workflow_info_json = serde_json::to_string(&workflow_info)
                        .unwrap_or_else(|e| {
                             error!("Failed to serialize demo workflow info to JSON: {}", e);
                             "null".to_string() // Default to JSON null on error
                         });
                    // ADD DEBUG LOG
                    info!("Serialized demo workflow JSON: {}", workflow_info_json);                         
                    
                    let context = MachineDetailsTemplate {
                        machine_json, // Pass JSON string
                        theme,
                        is_authenticated,
                        created_at_formatted,
                        updated_at_formatted,
                        workflow_info_json, // Pass JSON string
                        machine: machine.clone(), // Pass original struct too
                        workflow_info, // Pass original option too
                    };
                    return render_minijinja(&app_state, "machine_details.html", context);
                } else {
                    // Machine not found in demo mode, show error
                    let context = ErrorTemplate {
                        theme,
                        is_authenticated,
                        title: "Demo Machine Not Found".to_string(),
                        message: "The requested demo machine was not found.".to_string(),
                        error_details: format!("UUID: {}", uuid),
                        back_url: "/machines".to_string(),
                        back_text: "Back to Machines".to_string(),
                        show_retry: false,
                        retry_url: "".to_string(),
                    };
                    return render_minijinja(&app_state, "error.html", context);
                }
            }
            
            // Normal mode - get machine by ID from database
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
                    
                    // Serialize machine and workflow_info to JSON strings
                    let machine_json = serde_json::to_string(&machine)
                        .unwrap_or_else(|e| {
                            error!("Failed to serialize machine {} to JSON: {}", machine.id, e);
                            "{}".to_string() // Default to empty JSON object on error
                        });
                    // ADD DEBUG LOG
                    info!("Serialized machine JSON for {}: {}", machine.id, machine_json);
                    
                    let workflow_info_json = serde_json::to_string(&workflow_info)
                        .unwrap_or_else(|e| {
                             error!("Failed to serialize workflow info for machine {} to JSON: {}", machine.id, e);
                             "null".to_string() // Default to JSON null on error
                         });
                    // ADD DEBUG LOG
                    info!("Serialized workflow JSON for {}: {}", machine.id, workflow_info_json);                         

                    // Replace Askama render with placeholder
                    let context = MachineDetailsTemplate {
                        machine_json, // Pass JSON string
                        theme,
                        is_authenticated,
                        created_at_formatted,
                        updated_at_formatted,
                        workflow_info_json, // Pass JSON string
                        machine: machine.clone(), // Pass original struct too
                        workflow_info, // Pass original option too
                    };
                    // Pass AppState to render_minijinja
                    render_minijinja(&app_state, "machine_details.html", context)
                },
                Ok(None) => {
                    error!("Machine not found: {}", uuid);
                    // Replace Askama render with placeholder
                    let context = ErrorTemplate { // Use ErrorTemplate for consistency
                        theme,
                        is_authenticated,
                        title: "Machine Not Found".to_string(),
                        message: "The requested machine could not be found.".to_string(),
                        error_details: format!("UUID: {}", uuid),
                        back_url: "/machines".to_string(),
                        back_text: "Back to Machines".to_string(),
                        show_retry: false,
                        retry_url: "".to_string(),
                    };
                    // Pass AppState to render_minijinja
                    render_minijinja(&app_state, "error.html", context) // Render error template
                },
                Err(e) => {
                    error!("Error fetching machine {}: {}", uuid, e);
                    // Replace Askama render with placeholder
                    let context = ErrorTemplate { // Use ErrorTemplate
                        theme,
                        is_authenticated,
                        title: "Database Error".to_string(),
                        message: "An error occurred while fetching machine details.".to_string(),
                        error_details: format!("Error: {}", e),
                        back_url: "/machines".to_string(),
                        back_text: "Back to Machines".to_string(),
                        show_retry: false,
                        retry_url: "".to_string(),
                    };
                    // Pass AppState to render_minijinja
                    render_minijinja(&app_state, "error.html", context) // Render error template
                }
            }
        },
        Err(e) => {
            error!("Invalid UUID: {}", e);
            // Replace Askama render with placeholder
            let context = ErrorTemplate { // Use ErrorTemplate
                theme,
                is_authenticated,
                title: "Invalid Request".to_string(),
                message: "The provided machine ID was not a valid format.".to_string(),
                error_details: format!("Invalid UUID: {}", id),
                back_url: "/machines".to_string(),
                back_text: "Back to Machines".to_string(),
                show_retry: false,
                retry_url: "".to_string(),
            };
            // Pass AppState to render_minijinja
            render_minijinja(&app_state, "error.html", context) // Render error template
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
    pub password: Option<String>,
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
        form.password.is_some() || 
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
            admin_username: current_settings.admin_username.clone(),
            admin_password_hash: current_settings.admin_password_hash.clone(),
            oauth_client_id: current_settings.oauth_client_id.clone(),
            oauth_client_secret: current_settings.oauth_client_secret.clone(),
            oauth_auth_url: current_settings.oauth_auth_url.clone(),
            oauth_token_url: current_settings.oauth_token_url.clone(),
            oauth_redirect_url: current_settings.oauth_redirect_url.clone(),
        };

        info!("Saving settings: require_login={}, default_os={:?}, setup_completed={:?}", 
              new_settings.require_login, new_settings.default_os, new_settings.setup_completed);

        // Save the general settings
        if let Err(e) = save_app_settings(&new_settings).await {
            error!("Failed to save settings: {}", e);
            // Handle error, maybe return an error message to the user
            // For now, just log and continue
        } else {
            // Update settings in app state ONLY after successful save
            if let Ok(mut guard) = app_state.settings.try_lock() {
                *guard = new_settings.clone(); // Update the in-memory state
                info!("In-memory AppState settings updated.");
            } else {
                error!("Failed to acquire lock to update in-memory AppState settings.");
                // The settings are saved in DB, but the live state might be stale until restart/reload
            }
        }

        // Update admin password if provided and confirmed
        // Check form.password instead of form.password
        if let (Some(password), Some(confirm)) = (&form.password, &form.password_confirm) {
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
        hide_footer: true,
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
    
    // Save mode to database immediately
    if let Err(e) = mode::save_mode(mode::DeploymentMode::Simple, false).await {
        error!("Failed to save Simple mode to database: {}", e);
        
        // Return error template
        let context = ErrorTemplate {
            theme,
            is_authenticated,
            title: "Setup Failed".to_string(),
            message: "There was a problem setting up Simple mode.".to_string(),
            error_details: format!("Failed to save mode: {}", e),
            back_url: "/".to_string(),
            back_text: "Back to Dashboard".to_string(),
            show_retry: true,
            retry_url: "/setup/simple".to_string(),
        };
        return render_minijinja(&app_state, "error.html", context);
    }
    
    // Mark setup as completed immediately
    if let Err(e) = mark_setup_completed(true).await {
        error!("Failed to mark setup as completed: {}", e);
    } else {
        info!("Setup marked as completed");
        
        // Also update the in-memory settings
        let mut settings = app_state.settings.lock().await;
        settings.setup_completed = true;
    }
    
    // Configure the system for Simple mode in the background
    let event_manager = app_state.event_manager.clone();
    tokio::spawn(async move {
        match mode::configure_simple_mode().await {
            Ok(_) => {
                info!("Simple mode configuration completed successfully in background");
                // Send event for successful configuration
                let _ = event_manager.send("mode_configured:simple".to_string());
            },
            Err(e) => {
                error!("Background Simple mode configuration failed: {}", e);
                // Send event for failed configuration
                let _ = event_manager.send(format!("mode_configuration_failed:simple:{}", e));
            }
        }
    });
    
    // Immediately redirect to home page
    info!("Saved Simple mode and initiated background configuration, redirecting to home");
    Redirect::to("/").into_response()
}

pub async fn setup_flight(
    State(app_state): State<crate::AppState>,
    headers: HeaderMap,
    auth_session: AuthSession,
) -> Response {
    // Get theme preference from cookie
    let theme = get_theme_from_cookie(&headers);
    let is_authenticated = auth_session.user.is_some();
    
    // Save mode to database immediately
    if let Err(e) = mode::save_mode(mode::DeploymentMode::Flight, false).await {
        error!("Failed to save Flight mode to database: {}", e);
        
        // Return error template
        let context = ErrorTemplate {
            theme,
            is_authenticated,
            title: "Setup Failed".to_string(),
            message: "There was a problem setting up Flight mode.".to_string(),
            error_details: format!("Failed to save mode: {}", e),
            back_url: "/".to_string(),
            back_text: "Back to Dashboard".to_string(),
            show_retry: true,
            retry_url: "/setup/flight".to_string(),
        };
        return render_minijinja(&app_state, "error.html", context);
    }
    
    // Mark setup as completed immediately
    if let Err(e) = mark_setup_completed(true).await {
        error!("Failed to mark setup as completed: {}", e);
    } else {
        info!("Setup marked as completed");
        
        // Also update the in-memory settings
        let mut settings = app_state.settings.lock().await;
        settings.setup_completed = true;
    }
    
    // Configure the system for Flight mode in the background
    let event_manager = app_state.event_manager.clone();
    tokio::spawn(async move {
        match mode::configure_flight_mode().await {
            Ok(_) => {
                info!("Flight mode configuration completed successfully in background");
                // Send event for successful configuration
                let _ = event_manager.send("mode_configured:flight".to_string());
            },
            Err(e) => {
                error!("Background Flight mode configuration failed: {}", e);
                // Send event for failed configuration
                let _ = event_manager.send(format!("mode_configuration_failed:flight:{}", e));
            }
        }
    });
    
    // Immediately redirect to home page
    info!("Saved Flight mode and initiated background configuration, redirecting to home");
    Redirect::to("/").into_response()
}

pub async fn setup_swarm(
    State(app_state): State<crate::AppState>,
    headers: HeaderMap,
    auth_session: AuthSession,
) -> Response {
    // Get theme preference from cookie
    let theme = get_theme_from_cookie(&headers);
    let is_authenticated = auth_session.user.is_some();
    
    // Save mode to database immediately
    if let Err(e) = mode::save_mode(mode::DeploymentMode::Swarm, false).await {
        error!("Failed to save Swarm mode to database: {}", e);
        
        // Return error template
        let context = ErrorTemplate {
            theme,
            is_authenticated,
            title: "Setup Failed".to_string(),
            message: "There was a problem setting up Swarm mode.".to_string(),
            error_details: format!("Failed to save mode: {}", e),
            back_url: "/".to_string(),
            back_text: "Back to Dashboard".to_string(),
            show_retry: true,
            retry_url: "/setup/swarm".to_string(),
        };
        return render_minijinja(&app_state, "error.html", context);
    }
    
    // Mark setup as completed immediately
    if let Err(e) = mark_setup_completed(true).await {
        error!("Failed to mark setup as completed: {}", e);
    } else {
        info!("Setup marked as completed");
        
        // Also update the in-memory settings
        let mut settings = app_state.settings.lock().await;
        settings.setup_completed = true;
    }
    
    // Configure the system for Swarm mode in the background
    let event_manager = app_state.event_manager.clone();
    tokio::spawn(async move {
        match mode::configure_swarm_mode().await {
            Ok(_) => {
                info!("Swarm mode configuration completed successfully in background");
                // Send event for successful configuration
                let _ = event_manager.send("mode_configured:swarm".to_string());
            },
            Err(e) => {
                error!("Background Swarm mode configuration failed: {}", e);
                // Send event for failed configuration
                let _ = event_manager.send(format!("mode_configuration_failed:swarm:{}", e));
            }
        }
    });
    
    // Immediately redirect to home page
    info!("Saved Swarm mode and initiated background configuration, redirecting to home");
    Redirect::to("/").into_response()
}

// Environment setup for MiniJinja
pub fn setup_minijinja_environment(env: &mut minijinja::Environment) -> Result<(), anyhow::Error> {
    // Add OS name formatter
    env.add_filter("format_os", |os: &str| -> String {
        format_os_name(os)
    });
    
    // Add OS icon formatter
    env.add_filter("format_os_icon", |os: &str| -> String {
        get_os_icon(os)
    });
    
    // Add combined OS info formatter that returns a serializable struct
    env.add_filter("get_os_info", |os: &str| -> minijinja::Value {
        let info = get_os_info(os);
        minijinja::value::Value::from_serialize(&info)
    });
    
    // Register datetime formatting filter
    env.add_filter("datetime_format", |args: &[minijinja::Value]| -> Result<String, minijinja::Error> {
        if args.len() < 2 {
            return Err(minijinja::Error::new(
                minijinja::ErrorKind::InvalidOperation,
                "datetime_format requires a datetime and format string"
            ));
        }
        
        // Extract the datetime from the first argument
        let dt_str = args[0].as_str().ok_or_else(|| {
            minijinja::Error::new(
                minijinja::ErrorKind::InvalidOperation,
                "datetime must be a string in ISO format"
            )
        })?;
        
        // Parse the datetime
        let dt = match chrono::DateTime::parse_from_rfc3339(dt_str) {
            Ok(dt) => dt.with_timezone(&chrono::Utc),
            Err(_) => {
                return Err(minijinja::Error::new(
                    minijinja::ErrorKind::InvalidOperation,
                    "could not parse datetime string"
                ));
            }
        };
        
        // Extract the format string
        let fmt = args[1].as_str().ok_or_else(|| {
            minijinja::Error::new(
                minijinja::ErrorKind::InvalidOperation,
                "format must be a string"
            )
        })?;
        
        // Format the datetime
        Ok(dt.format(fmt).to_string())
    });
    
    // Set up more configuration as needed
    env.add_global("now", minijinja::Value::from(chrono::Utc::now().to_rfc3339()));
    
    // Add custom filter for robust JSON serialization
    env.add_filter("to_json", |value: minijinja::Value| -> Result<String, minijinja::Error> {
        match serde_json::to_string(&value) {
            Ok(s) => Ok(s),
            Err(e) => Err(minijinja::Error::new(
                minijinja::ErrorKind::InvalidOperation,
                format!("Failed to serialize value to JSON: {}", e)
            )),
        }
    });
    
    Ok(())
}

// ---- Alert Messages ----

#[derive(Serialize, Debug, Clone)]
pub struct AlertMessage {
    level: String, // e.g., "success", "error", "info", "warning"
    message: String,
}

impl AlertMessage {
    pub fn success(message: &str) -> Self {
        AlertMessage { level: "success".to_string(), message: message.to_string() }
    }
    pub fn error(message: &str) -> Self {
        AlertMessage { level: "error".to_string(), message: message.to_string() }
    }
    // Add info and warning if needed
}

// Trait to add alert messages to responses (e.g., via cookies)
pub trait AddAlert {
    fn add_alert(self, alert: AlertMessage) -> Self;
}

impl AddAlert for Response {
    fn add_alert(mut self, alert: AlertMessage) -> Self {
        match serde_json::to_string(&alert) {
            Ok(json_alert) => {
                // Use Cookie builder for better configuration
                let mut cookie = Cookie::build(("dragonfly_alert", json_alert))
                    .path("/")
                    // Make SameSite::Lax for broader compatibility with redirects
                    .same_site(SameSite::Lax)
                    // Set HttpOnly to false so JavaScript can read it
                    .http_only(false)
                    // Set MaxAge to 0 so it's a session cookie (or short duration)
                    .max_age(time::Duration::seconds(5)); // Expires after 5 seconds

                // Add the cookie header
                self.headers_mut().append(
                    header::SET_COOKIE,
                    cookie.finish().to_string().parse().unwrap(),
                );
            }
            Err(e) => {
                error!("Failed to serialize alert message: {}", e);
                // Optionally add a fallback mechanism or just log the error
            }
        }
        self
    }
} 