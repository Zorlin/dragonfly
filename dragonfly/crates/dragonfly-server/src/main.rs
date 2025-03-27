use dragonfly_server;
use tracing::error;

#[tokio::main]
async fn main() {
    // Initialize tracing
    tracing_subscriber::fmt::init();
    
    // Start the server
    if let Err(e) = dragonfly_server::start().await {
        error!("Server error: {}", e);
        std::process::exit(1);
    }
} 