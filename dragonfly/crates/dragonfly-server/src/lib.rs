pub mod api;
pub mod ui;
pub mod db;

use axum::Router;
use std::net::SocketAddr;
use tower_http::trace::TraceLayer;

/// Run the dragonfly server
pub async fn run_server() -> anyhow::Result<()> {
    // Initialize the database
    db::init_db().await?;
    
    // Create the router
    let app = Router::new()
        .merge(api::api_router())
        .merge(ui::ui_router())
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