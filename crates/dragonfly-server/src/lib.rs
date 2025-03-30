use axum::{routing::{get, post}, extract::Extension, Router, response::{IntoResponse, Response}};
use axum_login::{AuthManagerLayerBuilder};
use tower_sessions::{SessionManagerLayer, MemoryStore};
use std::sync::{Arc};
use tokio::sync::Mutex;
use tower_http::trace;
use tower_http::trace::TraceLayer;
use tracing::{info, Level, error};
use std::net::SocketAddr;
use tower_cookies::CookieManagerLayer;
use tower_http::services::ServeDir;

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
}

pub async fn run() -> anyhow::Result<()> {
    // Initialize the database 
    let db_pool = init_db().await?;
    
    // Initialize timing database tables
    db::init_timing_tables().await?;
    
    // Load historical timing data
    tinkerbell::load_historical_timings().await?;
    
    // Start the timing cleanup task
    tinkerbell::start_timing_cleanup_task().await;
    
    // Create event manager
    let event_manager = Arc::new(EventManager::new());
    
    // Store the event manager in the global static for access from other modules
    if let Ok(mut global_ref) = EVENT_MANAGER_REF.write() {
        *global_ref = Some(event_manager.clone());
    } else {
        error!("Failed to store event manager reference");
    }
    
    // Start the workflow polling task
    tinkerbell::start_workflow_polling_task(event_manager.clone()).await;
    
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
    
    // Create application state
    let app_state = AppState {
        settings: Arc::new(Mutex::new(settings)),
        event_manager: event_manager,
    };
    
    // Set up a session store
    let session_store = MemoryStore::default();
    let session_layer = SessionManagerLayer::new(session_store)
        .with_secure(false); // Ensure session layer is configured (secure flag might depend on deployment)
    
    // Create session-based authentication
    let backend = AdminBackend::new(credentials);
    let auth_layer = AuthManagerLayerBuilder::new(backend, session_layer).build();
    
    // Build the app router with shared state
    let app = Router::new()
        .merge(auth_router())
        .merge(ui::ui_router())
        .route("/{mac}", get(api::ipxe_script))  // MAC route at root level for iPXE
        .nest("/api", api::api_router())
        .nest_service("/static", ServeDir::new("crates/dragonfly-server/static"))
        .layer(CookieManagerLayer::new())
        .layer(auth_layer)
        .layer(Extension(db_pool))
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(trace::DefaultMakeSpan::new()
                    .level(Level::INFO))
                .on_response(trace::DefaultOnResponse::new()
                    .level(Level::INFO)),
        )
        .with_state(app_state);
    
    // Start the server
    info!("Starting server on 0.0.0.0:3000");
    let addr = SocketAddr::from(([0, 0, 0, 0], 3000));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app.into_make_service()).await?;
    
    Ok(())
} 