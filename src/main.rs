// Global allocator setup for heap profiling
#[cfg(feature = "dhat-heap")]
#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

// Main binary that starts the server
use clap::{Parser, Subcommand};
use color_eyre::eyre::Result;
use tracing::{error, info, Level, warn};
use tracing_subscriber::{fmt, EnvFilter, prelude::*};

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
struct ServerArgs {}

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
    
    // --- Logging initialization is now deferred to command handlers ---

    match cli.command {
        Some(Commands::Install(args)) => {
            // Set env vars for install mode BEFORE initializing logger
            std::env::set_var("RUST_LOG", "error,dragonfly_server=error");
            std::env::set_var("DRAGONFLY_QUIET", "true");

            // Initialize QUIET logger for install command
            let env_filter = EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("error")); // Default to error if RUST_LOG not set

            fmt()
                .with_env_filter(env_filter)
                .with_writer(std::io::sink) // Discard all output
                .with_target(false)
                .init();
            
            // info!("Running Install command..."); // No logging in quiet mode
            
            // Call the actual installation function
            if let Err(e) = cmd::install::run_install(args).await {
                // Still print critical errors directly to stderr
                eprintln!("Error during installation: {:#}", e);
                std::process::exit(1);
            }
            // info!("Installation process finished."); // No logging
        }
        Some(Commands::Setup(_)) => {
            // Initialize NORMAL logger for setup command
            let log_level = if cli.verbose { Level::DEBUG } else { Level::INFO };
             let default_log_level_directive = if cli.verbose { 
                "debug".parse().unwrap() 
            } else { 
                "info".parse().unwrap() 
            };
            let env_filter = EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("").add_directive(default_log_level_directive));

            fmt()
                .with_env_filter(env_filter)
                .with_writer(stderr)
                .with_target(false)
                .init();
            info!("Logging initialized for Setup Wizard (stderr).");
            info!("Running Dragonfly setup wizard...");
            
            std::env::set_var("DRAGONFLY_SETUP_MODE", "true");
            
            if let Err(e) = dragonfly_server::run().await {
                error!("Setup wizard failed: {:#}", e);
                eprintln!("Error running setup wizard: {}", e);
                std::process::exit(1);
            }
        }
        Some(Commands::Server(_)) | None => {
            // Initialize NORMAL logger for server command (or default)
             let default_log_level_directive = if cli.verbose { 
                "debug".parse().unwrap() 
            } else { 
                "info".parse().unwrap() 
            };
            let env_filter = EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("").add_directive(default_log_level_directive));
            
            let run_as_service = std::env::var("DRAGONFLY_SERVICE").is_ok();
            
            let make_writer = move || -> Box<dyn std::io::Write + Send + Sync> {
                 if run_as_service {
                    let log_dir_result = dragonfly_server::mode::ensure_log_directory();
                    let log_path = match log_dir_result {
                        Ok(dir) => format!("{}/dragonfly.log", dir),
                        Err(e) => {
                            eprintln!("Error ensuring log directory: {}, falling back to /tmp/dragonfly.log", e);
                            "/tmp/dragonfly.log".to_string()
                        }
                    };
                    match OpenOptions::new().create(true).append(true).open(&log_path) {
                        Ok(log_file) => Box::new(log_file),
                        Err(e) => {
                            eprintln!("CRITICAL: Failed to open log file '{}': {}. Logging to stderr.", log_path, e);
                            Box::new(stderr()) // Fallback to stderr
                        }
                    }
                } else {
                    Box::new(stderr()) // Default to stderr for foreground
                }
            };
            
            fmt()
                .with_env_filter(env_filter)
                .with_writer(make_writer)
                .with_target(false)
                .init();
                
             if run_as_service {
                 let log_dir_result = dragonfly_server::mode::ensure_log_directory();
                 if log_dir_result.is_ok() {
                     let log_path = format!("{}/dragonfly.log", log_dir_result.unwrap());
                     if OpenOptions::new().append(true).open(&log_path).is_ok() {
                          info!("Logging initialized to file: {}", log_path);
                     } else {
                         warn!("Falling back to stderr for logging due to file open failure.");
                     }
                 } else {
                      warn!("Falling back to stderr for logging due to log directory failure.");
                 }
            } else {
                info!("Logging initialized to stderr (foreground mode).");
            }

            // --- Server specific logic (checking mode, starting service, etc.) ---
            if run_as_service {
                info!("Starting Dragonfly server in service mode with PID {}...", std::process::id());
            } else {
                info!("Starting Dragonfly server...");
                match dragonfly_server::mode::get_current_mode().await {
                    Ok(Some(mode)) => {
                        info!("Deployment mode {} detected, starting service...", mode.as_str());
                        if cfg!(unix) {
                            if let Err(e) = dragonfly_server::mode::start_service() {
                                error!("Failed to start service: {:#}", e);
                                eprintln!("Error starting service: {}", e);
                                std::process::exit(1);
                            }
                            warn!("Continuing as fallback");
                        } else {
                            info!("Service management is not supported on this platform");
                        }
                    },
                    Ok(None) => {
                        info!("No deployment mode configured");
                    },
                    Err(e) => {
                        warn!("Failed to check deployment mode: {}", e);
                    }
                }
            }

            // Run the actual server
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