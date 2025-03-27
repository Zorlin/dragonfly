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

/// Run the dragonfly server
pub async fn run_server() -> anyhow::Result<()> {
    // Initialize the database
    db::init_db().await?;
    
    // Initialize Tinkerbell integration if KUBECONFIG is available
    if let Ok(_) = tinkerbell::init().await {
        tracing::info!("Tinkerbell integration initialized successfully");
    } else {
        tracing::warn!("Tinkerbell integration not initialized. KUBECONFIG environment variable may not be set.");
    }
    
    // Create the router
    let app = Router::new()
        .merge(api::api_router())
        .merge(ui::ui_router())
        // Serve static files
        .nest_service("/js", ServeDir::new("crates/dragonfly-server/static/js"))
        .nest_service("/css", ServeDir::new("crates/dragonfly-server/static/css"))
        .nest_service("/images", ServeDir::new("crates/dragonfly-server/static/images"))
        .layer(TraceLayer::new_for_http());

    // Create the address to bind to
    let addr = SocketAddr::from(([127, 0, 0, 1], 3000));
    tracing::info!("Listening on {}", addr);

    // Start the server
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;
    
    Ok(())
}

/// Run the dragonfly server, initializing tracing if needed
pub fn run() -> anyhow::Result<()> {
    // Initialize tracing if not already initialized
    tracing_subscriber::fmt::try_init().ok();
    
    // Start the runtime and run the server
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(run_server())
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