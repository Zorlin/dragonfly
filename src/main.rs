// Main binary that starts the server
use clap::{Parser, Subcommand};
use color_eyre::eyre::Result;
use tracing::{error, info, Level};
use tracing_subscriber::fmt;

// Reference the cmd module where subcommands live
mod cmd;
// Reference the actual install args from its module
use cmd::install::InstallArgs;

// Define the command-line arguments
#[derive(Parser, Debug)]
#[command(author, version, about = "Dragonfly Server and Installation Tool", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>, // Make the command optional
    
    /// Verbose output - shows more detailed logs
    #[arg(short, long)]
    verbose: bool,
}

// Define the subcommands
#[derive(Subcommand, Debug)]
enum Commands {
    /// Runs the main Dragonfly server (default action).
    Server(ServerArgs), // Add arguments struct if needed later
    /// Installs and configures k3s and the Tinkerbell stack.
    Install(InstallArgs), // Use the actual InstallArgs from cmd::install
    // Add Agent command later if needed
    // Agent(AgentArgs),
}

// Placeholder arguments for Server (can be empty if no args needed yet)
// This could eventually move to `src/cmd/server.rs` if server logic is extracted
#[derive(Parser, Debug)]
struct ServerArgs {}

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install()?; // Install better error handling

    let cli = Cli::parse();
    
    // Initialize tracing with a cleaner format (no timestamps)
    let log_level = if cli.verbose { Level::DEBUG } else { Level::INFO };
    
    fmt()
        .with_max_level(log_level)
        .without_time() // Remove timestamps
        .with_target(false) // Hide target module path
        .with_writer(std::io::stdout) // Write to stdout
        .init();

    // Decide which command to run
    match cli.command {
        // If `install` subcommand is given
        Some(Commands::Install(args)) => {
            info!("Running Install command...");
            // Call the actual installation function from the install module
            if let Err(e) = cmd::install::run_install(args).await {
                error!("Installation failed: {:#}", e);
                eprintln!("Error during installation: {}", e);
                std::process::exit(1);
            }
            info!("Installation process finished.");
        }
        // If `server` subcommand is given OR no subcommand is given (default)
        Some(Commands::Server(_)) | None => {
            info!("Starting Dragonfly server...");

            // Call your original server logic.
            // Consider moving this to `cmd/server.rs` and calling `cmd::server::run_server(...)`
            // For now, assuming `dragonfly_server::run()` exists in the lib or a module accessible here.
            match dragonfly_server::run().await { // Ensure dragonfly_server::run is accessible
                Ok(_) => {
                    info!("Server shut down gracefully");
                }
                Err(e) => {
                    error!("Server error: {:#}", e);
                    eprintln!("Error running server: {}", e);
                    std::process::exit(1);
                }
            }
        }
    }

    Ok(())
} 