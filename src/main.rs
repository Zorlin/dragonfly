// Main binary that starts the server
#[tokio::main]
async fn main() {
    println!("Starting Dragonfly server...");
    
    // For now, just defer to the server crate
    match dragonfly_server::start().await {
        Ok(_) => {},
        Err(e) => eprintln!("Error: {}", e),
    }
} 