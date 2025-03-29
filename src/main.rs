// Main binary that starts the server
use tracing::{info, error};

#[tokio::main]
async fn main() {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();
    
    info!("Starting Dragonfly server...");
    
    // For now, just defer to the server crate
    match dragonfly_server::start().await {
        Ok(_) => {
            info!("Server shut down gracefully");
        },
        Err(e) => {
            error!("Server error: {}", e);
            eprintln!("Error: {}", e);
        },
    }
} 