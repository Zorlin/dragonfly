use axum::{
    middleware,
    Router,
};
use std::{net::SocketAddr, sync::Arc};
use tokio::sync::Mutex;
use tower_http::{cors::CorsLayer, services::ServeDir, trace::TraceLayer};
use tracing::info;
use sqlx::SqlitePool;
use axum_login::AuthManagerLayerBuilder;
use tower_sessions::{MemoryStore, SessionManagerLayer};
use rand::RngCore;

use crate::auth::{AdminBackend, auth_router, load_credentials, generate_default_credentials, load_settings, Settings};
use crate::db::init_db;

mod auth;
mod api;
mod ui;
mod db;
mod tinkerbell;
mod filters;

pub use api::api_router;

// Define a shared state structure
#[derive(Clone)]
pub struct AppState {
    pub auth_backend: AdminBackend,
    pub db_pool: SqlitePool,
    pub settings: Arc<Mutex<Settings>>,
}

/// Start the API server with a default address
pub async fn start() -> Result<(), Box<dyn std::error::Error>> {
    // Use default address or read from configuration
    let addr = std::net::SocketAddr::from(([0, 0, 0, 0], 3000));
    init(addr).await
}

/// Initialize the API server
pub async fn init(addr: SocketAddr) -> Result<(), Box<dyn std::error::Error>> {
    // Load or generate admin credentials
    let credentials = match load_credentials().await {
        Ok(creds) => {
            info!("Admin credentials loaded - username: {}", creds.username);
            creds
        },
        Err(_) => {
            info!("No admin credentials found, generating...");
            generate_default_credentials()
        }
    };

    // Initialize the database 
    let db_pool = init_db().await?;
    
    // Create admin backend
    let backend = AdminBackend::new(credentials);
    
    // Load settings or use defaults
    let settings = load_settings();
    let settings_state = Arc::new(Mutex::new(settings));
    
    // Create shared state
    let app_state = AppState {
        auth_backend: backend.clone(),
        db_pool,
        settings: settings_state.clone(),
    };
    
    // Generate a random secret key for the session
    let mut secret = [0u8; 64];
    rand::thread_rng().fill_bytes(&mut secret);
    
    // Create session store and session layer
    info!("Setting up session store and auth layer");
    let session_store = MemoryStore::default();
    let session_layer = SessionManagerLayer::new(session_store)
        .with_secure(false)
        .with_same_site(tower_sessions::cookie::SameSite::Lax);

    // Create auth manager layer with explicit credential identifier name
    info!("Creating auth manager layer");
    let auth_layer = AuthManagerLayerBuilder::new(backend, session_layer)
        .build();
    
    // Build the router
    info!("Building router with authentication layer");
    let app = Router::new()
        .merge(api_router())
        .merge(auth_router())
        .merge(ui::ui_router().with_state(app_state.clone()))
        .nest_service("/static", ServeDir::new("crates/dragonfly-server/static"))
        .nest_service("/js", ServeDir::new("crates/dragonfly-server/static/js"))
        .nest_service("/css", ServeDir::new("crates/dragonfly-server/static/css"))
        .nest_service("/images", ServeDir::new("crates/dragonfly-server/static/images"))
        .layer(auth_layer)
        .layer(axum::extract::Extension(app_state))
        .layer(TraceLayer::new_for_http())
        .layer(CorsLayer::permissive());
    
    // Start the server
    info!("Starting server at {}", addr);
    axum::serve(
        tokio::net::TcpListener::bind(addr).await?,
        app.into_make_service()
    )
    .await?;
    
    Ok(())
} 