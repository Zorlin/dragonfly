// Global allocator setup for heap profiling
#[cfg(feature = "dhat-heap")]
#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

// Main binary that starts the server
use clap::{Parser, Subcommand};
use color_eyre::eyre::Result;
use tracing::{error, info, Level};
// Updated imports: Add EnvFilter
use tracing_subscriber::{fmt, prelude::*, registry, EnvFilter};
use tokio::sync::watch; // For shutdown signal
use clap::CommandFactory; // Needed for print_help

// Reference the cmd module where subcommands live
mod cmd;
// Add the new status module
mod status;
// Reference the actual install args from its module
use cmd::install::InstallArgs;

// Import necessary file handling modules
use std::fs::OpenOptions;
use std::io::stderr; // For foreground logging

// Define the command-line arguments
#[derive(Parser, Debug)]
#[command(author, version, about = "Dragonfly Metal Management", long_about = None)]
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
    let log_level = if cli.verbose { Level::DEBUG } else { Level::INFO };

    // Create shutdown channel (used only by install command for now)
    let (shutdown_tx, shutdown_rx) = watch::channel(());

    // Setup Ctrl+C handler to send shutdown signal
    let shutdown_tx_clone = shutdown_tx.clone();
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.expect("Failed to install Ctrl+C handler");
        info!("Ctrl+C received, sending shutdown signal...");
        // Send shutdown signal. Ignore result if receiver already dropped.
        let _ = shutdown_tx_clone.send(());
    });

    // Initialize logging ONLY if we are the main installer process
    if let Some(Commands::Install(_)) = &cli.command {
        // Define EnvFilter directives for install mode:
        // - Default level based on verbosity for the installer itself (crate root)
        // - Silence server-related crates
        let directives = format!(
            "dragonfly={level},dragonfly_server=off,tower=warn,hyper=warn,sqlx=warn,kube=warn,rustls=warn,h2=warn,reqwest=warn,tokio_reactor=warn,mio=warn,want=warn",
            level = log_level
        );
        let filter = EnvFilter::new(directives);
        let fmt_layer = fmt::layer().with_writer(stderr).with_target(false);
        registry().with(filter).with(fmt_layer).init();
        info!("Installer process starting with logging enabled...");
    }
    // NOTE: No logging init here for other modes. Server/Setup/Demo rely on RUST_LOG.
    
    // Set RUST_LOG env var based on verbosity *only if not installing*.
    // This allows tracing's default EnvFilter to pick it up if server is run directly.
    if !matches!(cli.command, Some(Commands::Install(_))) {
        if cli.verbose && std::env::var("RUST_LOG").is_err() {
            std::env::set_var("RUST_LOG", "debug");
        }
    }

    // Process commands
    match cli.command {
        Some(Commands::Install(args)) => {
            info!("Running install command...");
            // Pass the shutdown receiver to the install function
            if let Err(e) = cmd::install::run_install(args, shutdown_rx).await {
                error!("Installation failed: {:#}", e);
                eprintln!("Error during installation: {}", e);
                // Ensure shutdown signal is sent on error too
                let _ = shutdown_tx.send(());
                std::process::exit(1);
            } else {
                 info!("Installation process finished successfully.");
                 // Signal successful completion if needed, or just let program exit
                 // let _ = shutdown_tx.send(()); // Optional: Signal server to stop
            }
        }
        Some(Commands::Setup(_)) | Some(Commands::Server(_)) | None => {
            // Scenario A: Handle default 'dragonfly' invocation
            // This scenario *only* prints information and help, then exits.
            // It does NOT run the server.
            let db_exists = dragonfly_server::database_exists().await;

            if !db_exists {
                // Case: Dragonfly Not Installed
                println!("💡 Dragonfly is not installed.");
                println!("🐉 To get started, run: dragonfly install");
                println!();
                Cli::command().print_help()?; // Print help text
            } else {
                // Case: Dragonfly Installed
                println!("✅ Dragonfly is installed 🐉");
                
                // Check Kubernetes connectivity
                match status::check_kubernetes_connectivity().await {
                    Ok(_) => {
                        println!("  🔗 Kubernetes API: Reachable");
                        
                        // Check Dragonfly StatefulSet status
                        match status::check_dragonfly_statefulset_status().await {
                            Ok(true) => {
                                println!("  ✅ Dragonfly is running");
                                // Attempt to get WebUI address ONLY if StatefulSet is ready
                                match status::get_webui_address().await {
                                    Ok(Some(url)) if url.starts_with("http") => {
                                        println!("  🌐 Web UI should be available at: {}", url);
                                    }
                                    Ok(Some(internal_addr)) => {
                                        // Likely ClusterIP, provide guidance
                                        println!("    🏠 Web UI internal address: {} (Use 'kubectl port-forward svc/dragonfly 3000:80 -n dragonfly' or similar)", internal_addr);
                                    }
                                    Ok(None) => {
                                        // Service found, but address not determined (e.g., LB pending, port missing)
                                        println!("    ⏳ Web UI address determination pending (Service found, but address not ready/determinable)");
                                    }
                                    Err(e) => {
                                        println!("    🔴 Error determining Web UI address: {}", e);
                                    }
                                }
                            }
                            Ok(false) => {
                                println!("  ⚠️  Dragonfly is not running");
                            }
                            Err(e) => {
                                println!("  🛑  Error checking StatefulSet 'dragonfly': {}", e);
                            }
                        }
                    }
                    Err(e) => {
                        println!("  🔴 Error connecting to Kubernetes API: {}", e);
                        println!("    (Is k3s running? Is KUBECONFIG set correctly?)");
                    }
                }

                // Remove the old hardcoded address print
                // println!("🌐 Web UI should be available at: http://localhost:3000"); // Assuming default port
                println!();
                Cli::command().print_help()?; // Print help text
            }
            // Exit successfully after printing info/help.
        }
    }

    Ok(())
} 