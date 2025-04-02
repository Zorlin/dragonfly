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
// Reference the actual install args from its module
use cmd::install::InstallArgs;

// Import necessary file handling modules
use std::fs::OpenOptions;
use std::io::stderr; // For foreground logging

// Import status module from server crate
use dragonfly_server::status;

// --- Structs and Enums for Default Invocation Logic --- 

/// Represents the status determined by external checks.
#[derive(Debug, Clone, PartialEq)]
pub struct DefaultInvocationStatus {
    pub db_exists: bool,
    pub k8s_connectivity: Result<(), String>, // Store Ok or Err(message)
    pub statefulset_ready: Result<bool, String>, // Store Ok(is_ready) or Err(message)
    pub web_ui_address: Result<Option<String>, String>, // Store Ok(Some(url)/None) or Err(message)
}

/// Represents the detailed status of Kubernetes components.
#[derive(Debug, Clone, PartialEq)]
pub enum K8sStatus {
    ApiError(String),
    Connected {
        statefulset_status: StatefulSetStatus,
    },
}

/// Represents the status of the Dragonfly StatefulSet.
#[derive(Debug, Clone, PartialEq)]
pub enum StatefulSetStatus {
    Ready,
    NotReady,
    CheckError(String),
}

/// Represents the determined status or address of the Web UI service.
#[derive(Debug, Clone, PartialEq)]
pub enum WebUiStatus {
    Ready(String), // URL string
    Internal(String), // ClusterIP address/port string
    Pending, // Service found, but address not ready/determinable
    CheckError(String),
}

/// Represents the output/outcome of the default invocation logic.
#[derive(Debug, Clone, PartialEq)]
pub enum DefaultInvocationOutput {
    NotInstalled,
    Installed {
        k8s_status: K8sStatus,
        web_ui_status: Option<WebUiStatus>, // Only relevant if k8s connected and STS ready
    },
}

// --- End Structs and Enums --- 

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
            // Gather status first
            let db_exists = dragonfly_server::database_exists().await;
            
            // Perform k8s checks only if DB exists
            let mut k8s_conn_status = Err("Skipped (DB does not exist)".to_string());
            let mut sts_ready_status = Err("Skipped (DB does not exist)".to_string());
            let mut web_ui_status_res = Err("Skipped (DB does not exist)".to_string());
            
            if db_exists {
                k8s_conn_status = status::check_kubernetes_connectivity().await.map_err(|e| e.to_string());
                if k8s_conn_status.is_ok() {
                    sts_ready_status = status::check_dragonfly_statefulset_status().await.map_err(|e| e.to_string());
                    if matches!(sts_ready_status, Ok(true)) {
                        web_ui_status_res = status::get_webui_address().await.map_err(|e| e.to_string());
                    } else if sts_ready_status.is_ok() { // STS check succeeded but returned false (NotReady)
                         web_ui_status_res = Err("Skipped (StatefulSet not ready)".to_string());
                    } else { // STS check failed
                         web_ui_status_res = Err("Skipped (StatefulSet check failed)".to_string());
                    }
                } else {
                     sts_ready_status = Err("Skipped (K8s connection failed)".to_string());
                     web_ui_status_res = Err("Skipped (K8s connection failed)".to_string());
                }
            }
            
            // Populate the status struct
            let status_data = DefaultInvocationStatus {
                db_exists,
                k8s_connectivity: k8s_conn_status,
                statefulset_ready: sts_ready_status,
                web_ui_address: web_ui_status_res,
            };

            // Call the synchronous logic function
            let output = handle_default_invocation(status_data); // No .await here

            // Call the async printing function
            if let Err(e) = print_default_invocation_output(output).await {
                // Handle potential errors during printing (e.g., print_help failure)
                error!("Error printing default invocation output: {:#}", e);
                eprintln!("Error producing command output: {}", e);
                let _ = shutdown_tx.send(());
                std::process::exit(1);
            }
        }
    }

    Ok(())
}

/// Handles the logic for Scenario A (default invocation)
// Takes results as input, returns structured output, performs no I/O or printing.
fn handle_default_invocation(status: DefaultInvocationStatus) -> DefaultInvocationOutput {
    if !status.db_exists {
        return DefaultInvocationOutput::NotInstalled;
    }

    // Database exists, proceed with installed logic
    let k8s_status = match status.k8s_connectivity {
        Err(conn_err) => K8sStatus::ApiError(conn_err),
        Ok(_) => {
            // K8s connected, check StatefulSet
            let statefulset_status = match status.statefulset_ready {
                Ok(true) => StatefulSetStatus::Ready,
                Ok(false) => StatefulSetStatus::NotReady,
                Err(sts_err) => StatefulSetStatus::CheckError(sts_err),
            };
            K8sStatus::Connected { statefulset_status }
        }
    };

    // Determine WebUI status only if K8s connected and STS ready
    let web_ui_status = match &k8s_status {
        K8sStatus::Connected { statefulset_status: StatefulSetStatus::Ready } => {
            match status.web_ui_address {
                Ok(Some(url)) if url.starts_with("http") => Some(WebUiStatus::Ready(url)),
                Ok(Some(internal_addr)) => Some(WebUiStatus::Internal(internal_addr)),
                Ok(None) => Some(WebUiStatus::Pending),
                Err(ui_err) => Some(WebUiStatus::CheckError(ui_err)),
            }
        }
        _ => None, // Not relevant if K8s down or STS not ready
    };

    DefaultInvocationOutput::Installed {
        k8s_status,
        web_ui_status,
    }
}

/// Prints the output based on the structured DefaultInvocationOutput.
async fn print_default_invocation_output(output: DefaultInvocationOutput) -> Result<()> {
    match output {
        DefaultInvocationOutput::NotInstalled => {
            println!("💡 Dragonfly is not installed.");
            println!("🐉 To get started, run: dragonfly install");
        }
        DefaultInvocationOutput::Installed { k8s_status, web_ui_status } => {
            println!("✅ Dragonfly is installed 🐉");
            match k8s_status {
                K8sStatus::ApiError(conn_err) => {
                    println!("  🔴  Error connecting to Kubernetes API: {}", conn_err);
                    println!("      (Is k8s running? Is KUBECONFIG set correctly?)");
                }
                K8sStatus::Connected { statefulset_status } => {
                    println!("  🔗 Kubernetes API: Reachable");
                    match statefulset_status {
                        StatefulSetStatus::Ready => {
                            println!("  ✅ Dragonfly is running");
                            // Print WebUI status if available
                            match web_ui_status {
                                Some(WebUiStatus::Ready(url)) => {
                                    println!("  🌐 Web UI should be available at: {}", url);
                                }
                                Some(WebUiStatus::Internal(internal_addr)) => {
                                    println!("  🏠 Web UI internal address: {} (Use 'kubectl port-forward svc/tink-stack 3000:3000 -n tink' or similar)", internal_addr);
                                }
                                Some(WebUiStatus::Pending) => {
                                    println!("  ⏳ Web UI address determination pending (Service found, but address not ready/determinable)");
                                }
                                Some(WebUiStatus::CheckError(ui_err)) => {
                                    println!("  🔴 Error determining Web UI address: {}", ui_err);
                                }
                                None => { // Should not happen if STS is Ready, but handle defensively
                                     println!("  ❓ Web UI status check was skipped.");
                                }
                            }
                        }
                        StatefulSetStatus::NotReady => {
                             println!("    🟡 StatefulSet 'dragonfly': Not Ready (may be starting up or have issues)");
                             println!("    ⏳ Web UI address cannot be determined until StatefulSet is Ready.")
                        }
                         StatefulSetStatus::CheckError(sts_err) => {
                             println!("    🔴 Error checking StatefulSet 'dragonfly': {}", sts_err);
                         }
                    }
                }
            }
        }
    }

    // Print help text in all Scenario A cases
    println!();
    Cli::command().print_help()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*; // Import items from outer module

    #[test]
    fn test_handle_default_invocation_not_installed() {
        let status = DefaultInvocationStatus {
            db_exists: false,
            k8s_connectivity: Err("Skipped".to_string()), // Should be ignored
            statefulset_ready: Err("Skipped".to_string()), // Should be ignored
            web_ui_address: Err("Skipped".to_string()), // Should be ignored
        };
        let expected = DefaultInvocationOutput::NotInstalled;
        assert_eq!(handle_default_invocation(status), expected);
    }

    #[test]
    fn test_handle_default_invocation_installed_k8s_error() {
        let status = DefaultInvocationStatus {
            db_exists: true,
            k8s_connectivity: Err("Connection refused".to_string()),
            statefulset_ready: Err("Skipped".to_string()), // Should be skipped
            web_ui_address: Err("Skipped".to_string()), // Should be skipped
        };
        let expected = DefaultInvocationOutput::Installed {
            k8s_status: K8sStatus::ApiError("Connection refused".to_string()),
            web_ui_status: None,
        };
        assert_eq!(handle_default_invocation(status), expected);
    }

    #[test]
    fn test_handle_default_invocation_installed_sts_error() {
        let status = DefaultInvocationStatus {
            db_exists: true,
            k8s_connectivity: Ok(()),
            statefulset_ready: Err("Timeout getting STS".to_string()),
            web_ui_address: Err("Skipped".to_string()), // Should be skipped
        };
        let expected = DefaultInvocationOutput::Installed {
            k8s_status: K8sStatus::Connected {
                statefulset_status: StatefulSetStatus::CheckError("Timeout getting STS".to_string()),
            },
            web_ui_status: None,
        };
        assert_eq!(handle_default_invocation(status), expected);
    }

    #[test]
    fn test_handle_default_invocation_installed_sts_not_ready() {
        let status = DefaultInvocationStatus {
            db_exists: true,
            k8s_connectivity: Ok(()),
            statefulset_ready: Ok(false), // Explicitly not ready
            web_ui_address: Err("Skipped".to_string()), // Should be skipped
        };
        let expected = DefaultInvocationOutput::Installed {
            k8s_status: K8sStatus::Connected {
                statefulset_status: StatefulSetStatus::NotReady,
            },
            web_ui_status: None,
        };
        assert_eq!(handle_default_invocation(status), expected);
    }

    #[test]
    fn test_handle_default_invocation_installed_sts_ready_webui_ready() {
        let status = DefaultInvocationStatus {
            db_exists: true,
            k8s_connectivity: Ok(()),
            statefulset_ready: Ok(true),
            web_ui_address: Ok(Some("http://10.0.0.1:3000".to_string())),
        };
        let expected = DefaultInvocationOutput::Installed {
            k8s_status: K8sStatus::Connected {
                statefulset_status: StatefulSetStatus::Ready,
            },
            web_ui_status: Some(WebUiStatus::Ready("http://10.0.0.1:3000".to_string())),
        };
        assert_eq!(handle_default_invocation(status), expected);
    }
    
    #[test]
    fn test_handle_default_invocation_installed_sts_ready_webui_internal() {
        let status = DefaultInvocationStatus {
            db_exists: true,
            k8s_connectivity: Ok(()),
            statefulset_ready: Ok(true),
            web_ui_address: Ok(Some("10.43.1.5:3000".to_string())), // ClusterIP
        };
        let expected = DefaultInvocationOutput::Installed {
            k8s_status: K8sStatus::Connected {
                statefulset_status: StatefulSetStatus::Ready,
            },
            web_ui_status: Some(WebUiStatus::Internal("10.43.1.5:3000".to_string())),
        };
        assert_eq!(handle_default_invocation(status), expected);
    }
    
    #[test]
    fn test_handle_default_invocation_installed_sts_ready_webui_pending() {
        let status = DefaultInvocationStatus {
            db_exists: true,
            k8s_connectivity: Ok(()),
            statefulset_ready: Ok(true),
            web_ui_address: Ok(None), // LB IP pending
        };
        let expected = DefaultInvocationOutput::Installed {
            k8s_status: K8sStatus::Connected {
                statefulset_status: StatefulSetStatus::Ready,
            },
            web_ui_status: Some(WebUiStatus::Pending),
        };
        assert_eq!(handle_default_invocation(status), expected);
    }
    
    #[test]
    fn test_handle_default_invocation_installed_sts_ready_webui_error() {
        let status = DefaultInvocationStatus {
            db_exists: true,
            k8s_connectivity: Ok(()),
            statefulset_ready: Ok(true),
            web_ui_address: Err("Service not found".to_string()),
        };
        let expected = DefaultInvocationOutput::Installed {
            k8s_status: K8sStatus::Connected {
                statefulset_status: StatefulSetStatus::Ready,
            },
            web_ui_status: Some(WebUiStatus::CheckError("Service not found".to_string())),
        };
        assert_eq!(handle_default_invocation(status), expected);
    }
} 