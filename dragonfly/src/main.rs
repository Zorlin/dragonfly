// Main binary that starts the server
fn main() {
    println!("Starting Dragonfly server...");
    
    // For now, just defer to the server crate
    match dragonfly_server::run() {
        Ok(_) => {},
        Err(e) => eprintln!("Error: {}", e),
    }
} 