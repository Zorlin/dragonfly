use axum::{routing::{get}, extract::Extension, Router, response::{IntoResponse}, http::StatusCode};
use axum_login::{AuthManagerLayerBuilder};
use tower_sessions::{SessionManagerLayer};
use tower_sessions_sqlx_store::SqliteStore;
use std::sync::{Arc};
use tokio::sync::Mutex;
use tower_http::trace;
use tower_http::trace::TraceLayer;
use tracing::{info, Level, error, warn};
use std::net::SocketAddr;
use tower_cookies::CookieManagerLayer;
use tower_http::services::ServeDir;
use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::watch;
use std::process::Command;
use anyhow::Context;
use listenfd::ListenFd;

use crate::auth::{AdminBackend, auth_router, load_credentials, generate_default_credentials, load_settings, Settings};
use crate::db::init_db;
use crate::event_manager::EventManager;

// Add MiniJinja imports
use minijinja::path_loader;
use minijinja::{Environment};
use minijinja_autoreload::AutoReloader;
use std::sync::atomic::{AtomicBool, Ordering};

// Add Serialize for the enum
use serde::Serialize;

mod auth;
mod api;
mod db;
mod filters; // Uncomment unused module
pub mod ui;
pub mod tinkerbell;
pub mod event_manager;
pub mod os_templates;
pub mod mode;
// Remove missing module declarations
// pub mod state;
// pub mod utils;

// Global static for accessing event manager from other modules
use std::sync::RwLock;
use once_cell::sync::Lazy;
pub static EVENT_MANAGER_REF: Lazy<RwLock<Option<std::sync::Arc<EventManager>>>> = Lazy::new(|| {
    RwLock::new(None)
});

// Global static for accessing installation state during install
pub static INSTALL_STATE_REF: Lazy<RwLock<Option<Arc<Mutex<InstallationState>>>>> = Lazy::new(|| {
    RwLock::new(None)
});

// Enum to hold either static or reloading environment
#[derive(Clone)]
pub enum TemplateEnv {
    Static(Arc<Environment<'static>>),
    #[cfg(debug_assertions)]
    Reloading(Arc<AutoReloader>),
}

// Define the InstallationState enum here or import it
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum InstallationState {
    WaitingSudo,
    InstallingK3s,
    WaitingK3s,
    DeployingTinkerbell,
    DeployingDragonfly,
    Ready,
    Failed(String), // Add Failed variant with error message
}

impl InstallationState {
    pub fn get_message(&self) -> &str {
        match self {
            InstallationState::WaitingSudo => "Dragonfly is ready to install. Enter your password in your install window — let's do this.",
            InstallationState::InstallingK3s => "Dragonfly is installing k3s.",
            InstallationState::WaitingK3s => "Dragonfly is waiting for k3s to be ready.",
            InstallationState::DeployingTinkerbell => "Dragonfly is deploying Tinkerbell.",
            InstallationState::DeployingDragonfly => "Dragonfly is deploying... Dragonfly.",
            InstallationState::Ready => "Dragonfly is ready.",
            InstallationState::Failed(_) => "Installation failed. Check installer logs for details.", // Message for failed state
        }
    }
    pub fn get_animation_class(&self) -> &str {
        match self {
            InstallationState::WaitingSudo => "rocket-idle",
            InstallationState::InstallingK3s => "rocket-sparks",
            InstallationState::WaitingK3s => "rocket-glowing",
            InstallationState::DeployingTinkerbell => "rocket-smoke",
            InstallationState::DeployingDragonfly => "rocket-flicker",
            InstallationState::Ready => "rocket-fire rocket-shift",
            InstallationState::Failed(_) => "rocket-error", // CSS class for failed state
        }
    }
}

// Application state struct
#[derive(Clone)]
pub struct AppState {
    pub settings: Arc<Mutex<Settings>>,
    pub event_manager: Arc<EventManager>,
    pub setup_mode: bool,  // Explicit CLI setup mode
    pub first_run: bool,   // First run based on settings
    pub shutdown_tx: watch::Sender<()>,  // Channel to signal shutdown
    // Use the new enum for the environment
    pub template_env: TemplateEnv,
    // Add shared installation state, wrapped in Option
    pub install_state: Option<Arc<Mutex<InstallationState>>>,
}

// Clean up any existing processes
async fn cleanup_existing_processes() {
    // No complex process handling - removed
}

pub async fn run() -> anyhow::Result<()> {
    let is_install_mode = std::env::var("DRAGONFLY_QUIET").is_ok();
    
    let install_state = if is_install_mode {
        let state = Arc::new(Mutex::new(InstallationState::WaitingSudo));
        // Store the initial state in the global static
        if let Ok(mut global_ref) = INSTALL_STATE_REF.write() {
            *global_ref = Some(state.clone());
        }
        Some(state)
    } else {
        None
    };
    
    // Initialize the database 
    let db_pool = init_db().await?;
    
    // Initialize timing database tables
    db::init_timing_tables().await?;
    
    // Load historical timing data
    tinkerbell::load_historical_timings().await?;
    
    // --- Start OS Templates Initialization in Background ---
    info!("Starting OS templates initialization in background...");
    tokio::spawn(async move {
        match os_templates::init_os_templates().await {
            Ok(_) => info!("OS templates initialized successfully"),
            Err(e) => warn!("Failed to initialize OS templates: {}", e),
        }
    });
    
    // --- Graceful Shutdown Setup ---
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(());

    // Start the timing cleanup task with shutdown signal
    tinkerbell::start_timing_cleanup_task(shutdown_rx.clone()).await;
    
    // Create event manager
    let event_manager = Arc::new(EventManager::new());
    
    // Store the event manager in the global static
    if let Ok(mut global_ref) = EVENT_MANAGER_REF.write() {
        *global_ref = Some(event_manager.clone());
    }
    
    // Start the workflow polling task with shutdown signal
    tinkerbell::start_workflow_polling_task(event_manager.clone(), shutdown_rx.clone()).await;
    
    // Load or generate admin credentials
    let credentials = match load_credentials().await {
        Ok(creds) => {
            info!("Loaded existing admin credentials");
            creds
        },
        Err(_) => {
            info!("No admin credentials found, generating default credentials");
            match generate_default_credentials().await {
                Ok(creds) => creds,
                Err(e) => {
                    return Err(anyhow::anyhow!("Failed to generate default credentials: {}", e));
                }
            }
        }
    };
    
    // Load settings
    let settings = match load_settings().await {
        Ok(s) => s,
        Err(e) => {
            info!("Failed to load settings: {}, using defaults", e);
            Settings::default()
        }
    };
    
    // Check if setup mode is enabled via environment variable (set by CLI)
    let setup_mode = std::env::var("DRAGONFLY_SETUP_MODE").is_ok();
    
    // If setup mode is enabled, reset the setup flag
    if setup_mode {
        info!("Setup mode enabled, resetting setup completion status");
        let mut settings_copy = settings.clone();
        settings_copy.setup_completed = false;
        if let Err(e) = auth::save_settings(&settings_copy).await {
            warn!("Failed to reset setup status: {}", e);
        }
    }
    
    // Check if this is the first run by looking at the setup_completed flag
    let first_run = !settings.setup_completed || setup_mode;
    
    // --- MiniJinja Environment Setup ---
    // Determine template path
    let preferred_template_path = "/opt/dragonfly/templates";
    let fallback_template_path = "crates/dragonfly-server/templates";
    // Clone path for potential use in reloader closure
    let template_path = if std::path::Path::new(preferred_template_path).exists() {
        preferred_template_path
    } else {
        fallback_template_path
    }.to_string(); 

    let template_env = {
        // Enable debug auto-reloading if in development mode
        #[cfg(debug_assertions)]
        {
            info!("Setting up MiniJinja with auto-reload for development");
            
            // Flag to signal when templates are reloaded
            let templates_reloaded_flag = Arc::new(AtomicBool::new(false));
            let flag_clone_for_closure = templates_reloaded_flag.clone();
            
            let reloader = AutoReloader::new(move |notifier| {
                info!("MiniJinja environment is being (re)created...");
                let mut env = Environment::new();
                let path_for_closure = template_path.clone();
                env.set_loader(path_loader(&path_for_closure));
                // TODO: Add custom filters
                // Add filters::json_pretty to the environment
                // env.add_filter("json_pretty", filters::json_pretty_filter);
                
                // Signal that the environment was created/reloaded
                flag_clone_for_closure.store(true, Ordering::SeqCst);
                
                // Watch the template directory
                notifier.watch_path(path_for_closure.as_str(), true);
                
                Ok(env)
            });
            
            // Spawn the watcher loop
            let reloader_arc = Arc::new(reloader);
            let reloader_clone = reloader_arc.clone();
            let flag_clone_for_loop = templates_reloaded_flag.clone(); // Clone flag for the loop
            // Get a weak reference to the event manager for the watcher loop
            let event_manager_weak = Arc::downgrade(&event_manager);
            tokio::spawn(async move { 
                info!("Starting MiniJinja watcher loop...");
                loop {
                    // Acquire the environment guard. This checks for changes.
                    match reloader_clone.acquire_env() {
                        Ok(_) => {
                            // Check the flag set by the closure
                            if flag_clone_for_loop.swap(false, Ordering::SeqCst) {
                                info!("Templates reloaded - sending refresh event");
                                // Use the weak reference to the event manager
                                if let Some(event_manager) = event_manager_weak.upgrade() {
                                    if let Err(e) = event_manager.send("template_changed:refresh".to_string()) {
                                        warn!("Failed to send template refresh event: {}", e);
                                    } else {
                                        info!("Reload event sent successfully.");
                                    }
                                } else {
                                    warn!("EventManager dropped, cannot send reload event.");
                                }
                            }
                        },
                        Err(e) => {
                            error!("MiniJinja watcher refresh failed: {}", e);
                        }
                    }
                    
                    // Check more frequently in development mode
                    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
                }
            });
            TemplateEnv::Reloading(reloader_arc)
        }
        
        // Static environment for release builds
        #[cfg(not(debug_assertions))]
        {
            info!("Using static MiniJinja environment for release build");
            let mut env = Environment::new();
            env.set_loader(path_loader(&template_path));
            // TODO: Add custom filters here too
            TemplateEnv::Static(Arc::new(env))
        }
    };
    // --- End MiniJinja Setup ---
    
    // Create application state
    let app_state = AppState {
        settings: Arc::new(Mutex::new(settings)),
        event_manager: event_manager,
        setup_mode,
        first_run,
        shutdown_tx: shutdown_tx.clone(),
        // Add the environment enum to the state
        template_env,
        install_state: install_state.clone(), // Pass the install state
    };
    
    // Set up the persistent session store using the sqlx store
    let session_store = SqliteStore::new(db_pool.clone());
    session_store.migrate().await?; // Create the sessions table

    // Configure the session layer with the SqliteStore
    let session_layer = SessionManagerLayer::new(session_store)
        .with_secure(false);
    
    // Create session-based authentication
    let backend = AdminBackend::new(credentials);
    let auth_layer = AuthManagerLayerBuilder::new(backend, session_layer).build();
    
    // Build the app router with shared state
    let app = Router::new()
        .merge(auth_router())
        .merge(ui::ui_router())
        .route("/favicon.ico", get(handle_favicon))
        .route("/{mac}", get(api::ipxe_script))
        .route("/ipxe/{*path}", get(api::serve_ipxe_artifact))
        .nest("/api", api::api_router())
        .nest_service("/static", {
            let preferred_path = "/opt/dragonfly/static";
            let fallback_path = "crates/dragonfly-server/static";
            
            let static_path = if std::path::Path::new(preferred_path).exists() {
                preferred_path
            } else {
                fallback_path
            };
            
            ServeDir::new(static_path)
        })
        .layer(CookieManagerLayer::new())
        .layer(auth_layer)
        .layer(Extension(db_pool.clone())) // Pass the pool clone
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(trace::DefaultMakeSpan::new()
                    .level(tracing::level_filters::LevelFilter::current().into_level().unwrap_or(Level::INFO)))
                .on_response(trace::DefaultOnResponse::new()
                    .level(tracing::level_filters::LevelFilter::current().into_level().unwrap_or(Level::INFO))),
        )
        .with_state(app_state);
    
    // Check if we need to start the handoff listener
    let current_mode = mode::get_current_mode().await.unwrap_or(None);
    if let Some(mode) = current_mode {
        if mode == mode::DeploymentMode::Flight {
            info!("Running in Flight mode - starting handoff listener");
            tokio::spawn(async move {
                if let Err(e) = mode::start_handoff_listener(shutdown_rx.clone()).await {
                    error!("Handoff listener failed: {}", e);
                }
            });
        }
    }
    
    // --- Start Server with Socket Activation Support ---
    
    let server_port = 3000;
    let addr = SocketAddr::from(([0, 0, 0, 0], server_port));
    
    // Try to get listener socket from environment (systemfd/socket activation)
    let mut listenfd = ListenFd::from_env();
    
    // Look for LISTEN_FDS environment variable (set by systemd/launchd socket activation)
    let socket_activation = std::env::var("LISTEN_FDS").is_ok();
    
    if socket_activation {
        info!("Socket activation detected via LISTEN_FDS={}",
            std::env::var("LISTEN_FDS").unwrap_or_else(|_| "1".to_string()));
    }
    
    let listener = match listenfd.take_tcp_listener(0).context("Failed to take TCP listener from environment") {
        Ok(Some(listener)) => {
            // Convert the std::net TCP listener to a tokio one
            info!("Successfully acquired socket from socket activation");
            tokio::net::TcpListener::from_std(listener).context("Failed to convert TCP listener")?
        },
        Ok(None) => {
            // No socket from environment, create our own
            if socket_activation {
                warn!("Socket activation was detected (LISTEN_FDS), but no socket was found at fd 3");
            }
            info!("Binding to port {} directly", server_port);
            
            match tokio::net::TcpListener::bind(addr).await {
                Ok(listener) => listener,
                Err(e) => {
                    // Handle address in use error specifically
                    if e.kind() == std::io::ErrorKind::AddrInUse {
                        error!("Failed to start server: Port {} is already in use", server_port);
                        error!("Another instance of Dragonfly may be running. Try the following:");
                        error!("1. Check if Dragonfly is already running in the status bar");
                        error!("2. Run 'lsof -i:{}' to identify the process", server_port);
                        error!("3. Kill the process with 'kill -9 <PID>'");
                        return Err(anyhow::anyhow!("Port {} is already in use by another process. See above for resolution steps.", server_port));
                    }
                    
                    // Handle other kinds of socket binding errors
                    return Err(anyhow::anyhow!("Failed to bind to address: {}", e));
                }
            }
        },
        Err(e) => {
            warn!("Failed to check for socket activation: {}", e);
            // Fall back to normal binding
            match tokio::net::TcpListener::bind(addr).await {
                Ok(listener) => listener,
                Err(e) => {
                    return Err(anyhow::anyhow!("Failed to bind to address: {}", e));
                }
            }
        }
    };
    
    info!("Dragonfly server listening on http://{}", listener.local_addr().context("Failed to get local address")?);

    // Define the shutdown signal future - simplified
    let shutdown_signal = async move {
        let ctrl_c = async {
            tokio::signal::ctrl_c().await
                .expect("Failed to install Ctrl+C handler");
        };

        #[cfg(unix)]
        let terminate = async {
            signal(SignalKind::terminate())
                .expect("Failed to install signal handler")
                .recv()
                .await;
        };

        #[cfg(not(unix))] // Fallback for non-Unix systems
        let terminate = std::future::pending::<()>();

        tokio::select! {
            _ = ctrl_c => {
                 info!("Received SIGINT (Ctrl+C), exiting immediately");
                 std::process::exit(0);
            },
            _ = terminate => {
                 info!("Received SIGTERM, exiting immediately");
                 std::process::exit(0);
            },
            // Add a case for SIGUSR1 (handoff signal)
            _ = async {
                // Set up a signal handler for SIGUSR1
                if let Ok(mut sigusr1) = signal(SignalKind::user_defined1()) {
                    sigusr1.recv().await;
                    info!("Received SIGUSR1 signal for handoff");
                    true
                } else {
                    // If the signal handler can't be set up, this should never complete
                    std::future::pending::<bool>().await
                }
            } => {
                info!("Initiating handoff based on SIGUSR1 signal");
                std::process::exit(0);
            }
        }
    };

    // Start the server with graceful shutdown
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal)
        .await
        .context("Server error")?;

    // Remove cleanup at exit
    
    // Final cleanup message
    info!("Shutdown complete");
    
    Ok(())
} 

async fn handle_favicon() -> impl IntoResponse {
    // Serve the favicon from the static directory instead of returning 404
    let path = if std::path::Path::new("/opt/dragonfly/static/favicon/favicon.ico").exists() {
        "/opt/dragonfly/static/favicon/favicon.ico"
    } else {
        "crates/dragonfly-server/static/favicon/favicon.ico"
    };
    
    match tokio::fs::read(path).await {
        Ok(contents) => (
            StatusCode::OK,
            [(axum::http::header::CONTENT_TYPE, "image/x-icon")],
            contents
        ).into_response(),
        Err(_) => (StatusCode::NOT_FOUND, "Favicon not found").into_response()
    }
}