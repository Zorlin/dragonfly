use axum::{
    http::{Method, HeaderValue},
    middleware,
};
use tower_http::cors::{Any, CorsLayer};
use tokio::signal;
use std::path::PathBuf;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::Mutex;
use tower_sessions::{session_store::MemoryStore, Expiry, SessionManagerLayer};
use axum_login::{
    login_required, AuthLayer, RequireAuthorizationLayer,
};
use tracing::{error, info};

mod api;
mod db;
mod ui;
mod tinkerbell;
mod auth;

const CONFIG_DIR: &str = "config";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    // Create config directory if it doesn't exist
    std::fs::create_dir_all(CONFIG_DIR)?;

    // Initialize config DB - make sure this happens before loading credentials
    db::init_db().await?;

    // Load or generate admin credentials
    let credentials = match auth::load_credentials().await {
        Ok(creds) => {
            info!("Loaded existing admin credentials");
            creds
        },
        Err(_) => {
            info!("No admin credentials found, generating default ones (ONLY HAPPENS ON FIRST RUN)");
            // This will both generate a random password and save it to a file for the admin
            auth::generate_default_credentials().await
        }
    };

    // Load application settings
    let settings = auth::load_settings();
    let settings_state = Arc::new(Mutex::new(settings));

    // Set up authentication backend
    let backend = auth::AdminBackend::new(credentials);

    // Set up sessions with MemoryStore
    let session_store = MemoryStore::default();
    let session_layer = SessionManagerLayer::new(session_store)
        .with_secure(false)
        .with_expiry(Expiry::OnInactivity(time::Duration::days(1)));

    // Set up auth layer
    let auth_layer = AuthLayer::new(backend.clone());

    // Set up CORS
    let cors = CorsLayer::new()
        .allow_methods([Method::GET, Method::POST, Method::PUT, Method::DELETE])
        .allow_origin(Any);

    // Build our application with routes
    let app = axum::Router::new()
        .merge(ui::ui_router())
        .merge(api::api_router())
        .merge(auth::auth_router())
        .with_state(backend)
        .with_state(settings_state.clone())
        .layer(cors)
        .layer(auth_layer)
        .layer(session_layer)
        .layer(middleware::from_fn_with_state(
            settings_state,
            auth::auth_middleware,
        ));

    // Run the server
    let addr = SocketAddr::from(([0, 0, 0, 0], 3000));
    info!("Starting server at http://{}", addr);

    axum::Server::bind(&addr)
        .serve(app.into_make_service())
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    Ok(())
}

// Graceful shutdown handler
async fn shutdown_signal() {
    let ctrl_c = async {
        signal::ctrl_c()
            .await
            .expect("Failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("Failed to install signal handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }

    info!("Shutdown signal received, starting graceful shutdown");
} 