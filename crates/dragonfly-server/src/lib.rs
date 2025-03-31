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

mod auth;
mod api;
mod db;
mod filters;
mod ui;
mod tinkerbell;
mod event_manager;
mod os_templates;
pub mod mode;

// macOS-specific UI features (only compiled on macOS)
#[cfg(target_os = "macos")]
mod macos_ui;

// Global static for accessing event manager from other modules
use std::sync::RwLock;
use once_cell::sync::Lazy;
pub static EVENT_MANAGER_REF: Lazy<RwLock<Option<std::sync::Arc<EventManager>>>> = Lazy::new(|| {
    RwLock::new(None)
});

// Application state struct
#[derive(Clone)]
pub struct AppState {
    pub settings: Arc<Mutex<Settings>>,
    pub event_manager: Arc<EventManager>,
    pub setup_mode: bool,  // Explicit CLI setup mode
    pub first_run: bool,   // First run based on settings
    pub shutdown_tx: watch::Sender<()>,  // Channel to signal shutdown
}

// Clean up any existing processes
async fn cleanup_existing_processes() {
    info!("Checking for existing Dragonfly processes");
    
    // Check for processes using port 3000
    let lsof_output = Command::new("lsof")
        .args(["-i:3000", "-t"])
        .output();
    
    if let Ok(output) = lsof_output {
        if output.status.success() && !output.stdout.is_empty() {
            info!("Found existing process on port 3000, attempting to terminate");
            
            // Get the PID as a string
            let pid = String::from_utf8_lossy(&output.stdout).trim().to_string();
            
            // Try to terminate gracefully first
            let _ = Command::new("kill")
                .arg(&pid)
                .output();
                
            // Wait a moment for graceful shutdown
            tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
            
            // Check if it's still running
            let check_output = Command::new("lsof")
                .args(["-i:3000", "-t"])
                .output();
                
            if let Ok(check) = check_output {
                if check.status.success() && !check.stdout.is_empty() {
                    // Force kill if still running
                    info!("Process still running, forcing termination");
                    let _ = Command::new("kill")
                        .args(["-9", &pid])
                        .output();
                }
            }
        }
    }
    
    // Clean up any Swift UI processes
    #[cfg(target_os = "macos")]
    {
        let _ = Command::new("pkill")
            .args(["-f", "dragonfly_status_bar.swift"])
            .output();
    }
}

pub async fn run() -> anyhow::Result<()> {
    // Clean up any existing processes
    cleanup_existing_processes().await;

    // Initialize the database 
    let db_pool = init_db().await?;
    
    // Initialize timing database tables
    db::init_timing_tables().await?;
    
    // Load historical timing data
    tinkerbell::load_historical_timings().await?;
    
    // Remove automatic HookOS download at startup - we'll do this when the user selects Flight mode
    
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
    
    // Store the event manager in the global static for access from other modules
    if let Ok(mut global_ref) = EVENT_MANAGER_REF.write() {
        *global_ref = Some(event_manager.clone());
    } else {
        error!("Failed to store event manager reference");
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
    
    // Create application state
    let app_state = AppState {
        settings: Arc::new(Mutex::new(settings)),
        event_manager: event_manager,
        setup_mode,
        first_run,
        shutdown_tx: shutdown_tx.clone(),
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
                    .level(Level::INFO))
                .on_response(trace::DefaultOnResponse::new()
                    .level(Level::INFO)),
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
        
        // Initialize macOS status bar icon if running on macOS and a mode is already set
        #[cfg(target_os = "macos")]
        {
            // Get the deployment mode for the status bar icon
            let mode_str = mode.as_str();
            
            // Clone shutdown_tx for use with the macOS UI
            let ui_shutdown_tx = shutdown_tx.clone();
            
            // Initialize the macOS status bar icon
            tokio::spawn(async move {
                // Wait a moment for the server to fully start
                tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
                
                // Now try to initialize the status bar icon
                info!("Initializing macOS status bar icon for {} mode on startup", mode_str);
                match macos_ui::setup_status_bar(mode_str, ui_shutdown_tx).await {
                    Ok(_) => info!("macOS status bar icon initialized successfully on startup"),
                    Err(e) => error!("Could not initialize macOS status bar icon on startup: {}", e),
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

    // Define the shutdown signal future
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
                 info!("Received SIGINT (Ctrl+C), initiating shutdown...");
            },
            _ = terminate => {
                 info!("Received SIGTERM, initiating shutdown...");
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
            }
        }

        // Signal all subsystems to shut down
        let _ = shutdown_tx.send(());
        info!("Shutdown signal sent to all subsystems");
        
        // Short delay to ensure cleanup tasks can run
        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
    };

    // Start the server with graceful shutdown
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal)
        .await
        .context("Server error")?;

    // Clean up at exit
    cleanup_existing_processes().await;
    
    // Final cleanup message
    info!("Shutdown complete");
    
    Ok(())
} 

async fn handle_favicon() -> impl IntoResponse {
    (StatusCode::NOT_FOUND, "Favicon not found")
}