// Global allocator setup for heap profiling
#[cfg(feature = "dhat-heap")]
#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

// Main binary that starts the server
use clap::{Parser, Subcommand};
use color_eyre::eyre::Result;
use tracing::{error, info, Level, warn};
use tracing_subscriber::fmt;

// Reference the cmd module where subcommands live
mod cmd;
// Reference the actual install args from its module
use cmd::install::InstallArgs;

// Import necessary file handling modules
use std::fs::OpenOptions;
use std::io::stderr; // For foreground logging

// Define the command-line arguments
#[derive(Parser, Debug)]
#[command(author, version, about = "Dragonfly Server and Installation Tool", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>, // Make the command optional
    
    /// Verbose output - shows more detailed logs
    #[arg(short, long, default_value_t = false)]
    verbose: bool,
    
    /// Run in foreground instead of daemonizing
    #[arg(long, default_value_t = false)]
    foreground: bool,
}

// Define the subcommands
#[derive(Subcommand, Debug)]
enum Commands {
    /// Runs the main Dragonfly server (default action).
    Server(ServerArgs), // Add arguments struct if needed later
    /// Installs and configures k3s and the Tinkerbell stack.
    Install(InstallArgs), // Use the actual InstallArgs from cmd::install
    /// Runs the setup wizard for Dragonfly.
    Setup(SetupArgs),
    // Add Agent command later if needed
    // Agent(AgentArgs),
}

// Placeholder arguments for Server (can be empty if no args needed yet)
// This could eventually move to `src/cmd/server.rs` if server logic is extracted
#[derive(Parser, Debug)]
struct ServerArgs {
    /// Run in foreground instead of daemonizing
    #[arg(short, long)]
    foreground: bool,
}

// Setup command arguments (empty for now)
#[derive(Parser, Debug)]
struct SetupArgs {}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize dhat heap profiler if feature is enabled
    #[cfg(feature = "dhat-heap")]
    let _profiler = dhat::Profiler::new_heap();

    color_eyre::install()?; // Install better error handling

    let cli = Cli::parse();
    
    // Determine log level early
    let log_level = if cli.verbose { Level::DEBUG } else { Level::INFO };

    // Check if we're running in service mode via environment variable
    let run_as_service = std::env::var("DRAGONFLY_SERVICE").is_ok();

    // Initialize logging based on service mode
    if run_as_service {
        // When running as a service, log to a file
        let log_dir_result = dragonfly_server::mode::ensure_log_directory();
        let log_path = match log_dir_result {
            Ok(dir) => format!("{}/dragonfly.log", dir),
            Err(e) => {
                // Log initial error to stderr since tracing isn't fully up yet
                eprintln!("Error ensuring log directory: {}, falling back to /tmp/dragonfly.log", e);
                "/tmp/dragonfly.log".to_string()
            }
        };

        // Try to open the log file for appending
        match OpenOptions::new().create(true).append(true).open(&log_path) {
            Ok(log_file) => {
                fmt()
                    .with_max_level(log_level)
                    .with_writer(log_file) // Write to the log file
                    .with_target(false)
                    .init();
                info!("Logging initialized to file: {}", log_path); 
            },
            Err(e) => {
                // Critical error: cannot open log file, fall back to stderr
                eprintln!("CRITICAL: Failed to open log file '{}': {}. Logging to stderr.", log_path, e);
                fmt()
                    .with_max_level(log_level)
                    .with_writer(stderr) // Fallback to stderr
                    .with_target(false)
                    .init();
                warn!("Falling back to stderr for logging due to file open failure.");
            }
        }
    } else {
        // Regular foreground mode, initialize tracing to stderr
        fmt()
            .with_max_level(log_level)
            .with_writer(stderr) // Write to stderr (usually displayed in terminal)
            .with_target(false)
            .init();
        info!("Logging initialized to stderr (foreground mode).");
    }

    // Process commands
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
        // If `setup` subcommand is given
        Some(Commands::Setup(_)) => {
            info!("Running Dragonfly setup wizard...");
            
            // Set a special env var that the server will detect
            std::env::set_var("DRAGONFLY_SETUP_MODE", "true");
            
            // Call the server with setup mode enabled
            if let Err(e) = dragonfly_server::run().await {
                error!("Setup wizard failed: {:#}", e);
                eprintln!("Error running setup wizard: {}", e);
                std::process::exit(1);
            }
        }
        // If `server` subcommand is given OR no subcommand is given (default)
        Some(Commands::Server(_)) | None => {
            // Check if we're running in service mode from the environment variable
            let service_mode = std::env::var("DRAGONFLY_SERVICE").is_ok();
            
            let foreground = if let Some(Commands::Server(server_args)) = &cli.command {
                server_args.foreground || cli.foreground
            } else {
                cli.foreground
            };
            
            // If in service mode, we were started by systemd/launchd, no need to check for mode
            if service_mode {
                info!("Starting Dragonfly server in service mode with PID {}...", std::process::id());
            } else {
                info!("Starting Dragonfly server in foreground mode...");
                
                // Check if a mode is already set - if so, start the service instead
                if !foreground {
                    match dragonfly_server::mode::get_current_mode().await {
                        Ok(Some(mode)) => {
                            // A mode is already configured, we should start the service
                            info!("Deployment mode {} detected, starting service...", mode.as_str());
                            
                            if cfg!(unix) {
                                // Start the service through the service manager
                                if let Err(e) = dragonfly_server::mode::start_service() {
                                    error!("Failed to start service: {:#}", e);
                                    eprintln!("Error starting service: {}", e);
                                    std::process::exit(1);
                                }
                                // If the start_service function returns, that means it failed
                                // and we should continue running in foreground as fallback
                                warn!("Continuing in foreground mode as fallback");
                            } else {
                                info!("Service management is not supported on this platform. Running in foreground...");
                            }
                        },
                        Ok(None) => {
                            info!("No deployment mode configured, running in foreground");
                        },
                        Err(e) => {
                            warn!("Failed to check deployment mode: {}, running in foreground", e);
                        }
                    }
                }
            }

            // Run the server
            match dragonfly_server::run().await {
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