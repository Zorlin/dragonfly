pub mod api;
pub mod ui;
pub mod db;
pub mod tinkerbell;

use axum::Router;
use std::net::SocketAddr;
use tower_http::trace::TraceLayer;
use tower_http::services::ServeDir;
use anyhow::Result;
use tracing::{info, warn};

/// Run the dragonfly server, initializing tracing if needed
pub fn run() -> Result<()> {
    // Initialize tracing if not already initialized
    tracing_subscriber::fmt::try_init().ok();
    
    // Start the runtime and run the server
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(start())
}

/// Run the dragonfly server with extended configuration
pub async fn start() -> Result<()> {
    // Set the KUBECONFIG environment variable if not already set
    if std::env::var("KUBECONFIG").is_err() {
        std::env::set_var("KUBECONFIG", "/home/wings/projects/sparx/k3s.yaml");
    }
    
    // Initialize database
    db::init_db().await?;
    
    // Initialize Tinkerbell client
    if let Err(e) = tinkerbell::init().await {
        warn!("Failed to initialize Tinkerbell client: {}", e);
    } else {
        info!("Tinkerbell client initialized successfully");
    }
    
    // Build our application with a route
    let app = Router::new()
        .merge(ui::ui_router())
        .merge(api::api_router())
        .nest_service("/js", ServeDir::new("crates/dragonfly-server/static/js"))
        .nest_service("/css", ServeDir::new("crates/dragonfly-server/static/css"))
        .nest_service("/images", ServeDir::new("crates/dragonfly-server/static/images"))
        .layer(TraceLayer::new_for_http());
    
    // Run it
    let addr = SocketAddr::from(([0, 0, 0, 0], 3000));
    info!("Starting server at http://{}", addr);
    
    // Create listener and serve
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;
    
    Ok(())
} 