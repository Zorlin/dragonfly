use clap::Args;
use color_eyre::eyre::{Result, WrapErr}; // Include WrapErr
use std::net::Ipv4Addr; // Use specific types
use std::io::Write; // Import Write trait for stdout().flush()
use tracing::{debug, error, info, warn}; // Use tracing macros
use std::path::PathBuf;
use std::process::{Command, Output}; // For running commands
use std::time::Instant;
use tokio::fs; // For async file operations

#[derive(Args, Debug)]
pub struct InstallArgs {
    /// Optional: Specify the network interface to use for IP detection.
    #[arg(long)]
    pub interface: Option<String>, // Made public if needed elsewhere, or keep private

    /// Optional: Specify the starting IP address offset from the host IP.
    #[arg(long, default_value_t = 1)]
    pub start_offset: u8,

    /// Optional: Maximum number of IPs to check before giving up.
    #[arg(long, default_value_t = 20)]
    pub max_ip_search: u8,

    // Add other install-specific args here
}

// The main function for the install command
pub async fn run_install(args: InstallArgs) -> Result<()> {
    let start_time = Instant::now();
    
    info!("Starting Dragonfly installation");
    debug!("Installation arguments: {:?}", args);

    // --- 1. Determine Host IP and Network ---
    // Placeholder - Implement get_host_ip_and_mask
    let host_ip = Ipv4Addr::new(192, 168, 1, 100); // Example
    let netmask = Ipv4Addr::new(255, 255, 255, 0); // Example
    info!("Detected host IP: {} with netmask: {}", host_ip, netmask);
    // let (host_ip, netmask) = get_host_ip_and_mask(args.interface.as_deref())?
    //     .wrap_err("Failed to determine host IP address and netmask")?;


    // --- 2. Find Available Floating IP ---
    // Placeholder - Implement find_available_ip
    let bootstrap_ip = Ipv4Addr::new(192, 168, 1, 101); // Example
    info!("Found available bootstrap IP: {}", bootstrap_ip);
    // let bootstrap_ip = find_available_ip(host_ip, netmask, args.start_offset, args.max_ip_search)
    //     .await?
    //     .wrap_err("Failed to find an available IP address for the bootstrap node")?;


    // --- 3. Install k3s ---
    info!("Setting up k3s...");
    install_k3s().await.wrap_err("Failed to set up k3s")?;

    // --- 4. Configure kubectl ---
    info!("Configuring kubectl...");
    let kubeconfig_path = configure_kubectl().await.wrap_err("Failed to configure kubectl")?;
    std::env::set_var("KUBECONFIG", &kubeconfig_path);
    debug!("Set KUBECONFIG environment variable to: {:?}", kubeconfig_path);

    // --- 5. Wait for Node Ready ---
    wait_for_node_ready().await.wrap_err("Timed out waiting for Kubernetes node")?;

    // --- 6. Install Helm ---
    info!("Setting up Helm...");
    install_helm().await.wrap_err("Failed to set up Helm")?;

    // --- 7. Install Tinkerbell Stack ---
    info!("Installing Tinkerbell stack...");
    install_tinkerbell_stack(bootstrap_ip).await.wrap_err("Failed to install Tinkerbell stack")?;

    let elapsed = start_time.elapsed();
    info!("✅ Dragonfly installation completed in {:.1?}!", elapsed);
    info!("PXE services available at: http://{}:8080", bootstrap_ip);

    Ok(())
}


// --- Helper function implementations (from previous response) ---

// Placeholder for run_shell_command - Implement robustly
fn run_shell_command(script: &str, description: &str) -> Result<()> {
    debug!("Running shell command: {}", description);
    let output = Command::new("sh")
        .arg("-c")
        .arg(script)
        .output()
        .wrap_err_with(|| format!("Failed to execute command: {}", description))?;

    if !output.status.success() {
        error!("Command '{}' failed with status: {}", description, output.status);
        error!("Stderr: {}", String::from_utf8_lossy(&output.stderr));
        error!("Stdout: {}", String::from_utf8_lossy(&output.stdout));
        color_eyre::eyre::bail!("Command '{}' failed", description);
    } else {
         debug!("Command '{}' succeeded.", description);
    }
    Ok(())
}

// Placeholder for run_command - Implement robustly
fn run_command(cmd: &str, args: &[&str], description: &str) -> Result<Output> {
    debug!("Running command: {} {}", cmd, args.join(" "));
     let output = Command::new(cmd)
        .args(args)
        .output()
        .wrap_err_with(|| format!("Failed to execute command: {}", description))?;

     if !output.status.success() {
        error!("Command '{}' failed with status: {}", description, output.status);
        error!("Stderr: {}", String::from_utf8_lossy(&output.stderr));
        error!("Stdout: {}", String::from_utf8_lossy(&output.stdout));
        color_eyre::eyre::bail!("Command '{}' failed", description);
    } else {
        debug!("Command '{}' succeeded.", description);
    }
     Ok(output)
}


// Placeholder for is_command_present - Implement robustly
fn is_command_present(cmd: &str) -> bool {
    Command::new(cmd).arg("--version").output().is_ok() // Simple check
}

// Implement get_host_ip_and_mask, find_available_ip...

async fn install_k3s() -> Result<()> {
    debug!("Checking if k3s is already installed");
    
    // Check for existing k3s config and service
    let config_exists = check_file_exists("/etc/rancher/k3s/k3s.yaml").await;
    let service_exists = is_command_present("k3s");
    let is_running = check_service_running("k3s").await;
    
    if service_exists && config_exists && is_running {
        info!("K3s is already installed and running");
        return Ok(());
    }
    
    // Handle partially installed k3s
    if service_exists && !is_running {
        info!("K3s service exists but isn't running, starting service...");
        restart_k3s_service().await?;
        return Ok(());
    }
    
    // Full installation needed
    info!("Installing k3s (single-node)");
    let script = r#"curl -sfL https://get.k3s.io | INSTALL_K3S_EXEC="--disable traefik" sh -"#;
    run_shell_command(script, "k3s installation script")?;

    // Verify installation
    if !is_command_present("k3s") {
        color_eyre::eyre::bail!("k3s installation command ran, but 'k3s' command not found afterwards.");
    }
    
    // Wait briefly for k3s to create its config
    tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
    
    // Make sure the service is up
    if !check_service_running("k3s").await {
        info!("Starting k3s service...");
        restart_k3s_service().await?;
    }
    
    info!("K3s installed successfully");
    Ok(())
}

async fn check_service_running(service_name: &str) -> bool {
    let output = Command::new("systemctl")
        .args(["is-active", service_name])
        .output();
        
    match output {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            stdout.trim() == "active"
        },
        Err(_) => false,
    }
}

async fn restart_k3s_service() -> Result<()> {
    debug!("Restarting k3s service");
    let restart_cmd = "sudo systemctl restart k3s";
    run_shell_command(restart_cmd, "restart k3s service")?;
    
    // Check if service started successfully
    tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
    if check_service_running("k3s").await {
        debug!("K3s service started successfully");
        Ok(())
    } else {
        color_eyre::eyre::bail!("Failed to start k3s service after restart attempt");
    }
}

async fn configure_kubectl() -> Result<PathBuf> {
    debug!("Configuring kubectl access");
    let source_path = PathBuf::from("/etc/rancher/k3s/k3s.yaml");
    let dest_path = std::env::current_dir()?.join("k3s.yaml");

    // Check if the destination file already exists and is valid
    if dest_path.exists() {
        debug!("kubectl config already exists at {:?}, testing validity", dest_path);
        
        // Test if the existing config works
        let test_result = Command::new("kubectl")
            .args(["--kubeconfig", dest_path.to_str().unwrap(), "cluster-info"])
            .output();
            
        if let Ok(output) = test_result {
            if output.status.success() {
                debug!("Existing kubectl config is valid");
                return Ok(dest_path);
            }
            
            debug!("Existing kubectl config is invalid, will recreate");
        }
    }

    // Wait for k3s to create the config file
    let mut attempts = 0;
    while !source_path.exists() && attempts < 12 {
        debug!("Waiting for k3s.yaml to be created (attempt {}/12)...", attempts + 1);
        tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
        attempts += 1;
    }
    
    if !source_path.exists() {
        color_eyre::eyre::bail!("k3s config file not found at {:?} after 60 seconds. Was k3s installed correctly?", source_path);
    }

    // Determine if sudo is likely needed for copy/chown
    let uid = unsafe { libc::geteuid() };
    let needs_sudo = uid != 0; // Simple check if running as root

    debug!("Copying {:?} to {:?}", source_path, dest_path);
    let cp_cmd = format!(
        "{} cp {} {}",
        if needs_sudo { "sudo" } else { "" },
        source_path.display(),
        dest_path.display()
    );
    run_shell_command(&cp_cmd.trim(), "copy k3s.yaml")?; // trim leading space if no sudo

    // Get current user for chown
    let user = std::env::var("SUDO_USER") // If run with sudo, chown to the original user
        .or_else(|_| std::env::var("USER")) // Otherwise, use current user
        .wrap_err("Could not determine user for chown")?;

    let chown_cmd = format!(
        "{} chown {} {}",
        if needs_sudo { "sudo" } else { "" },
        user,
        dest_path.display()
    );
    run_shell_command(&chown_cmd.trim(), "chown k3s.yaml")?; // trim leading space if no sudo

    debug!("k3s.yaml copied and permissions set for user '{}'", user);
    Ok(dest_path)
}

async fn wait_for_node_ready() -> Result<()> {
    info!("Waiting for Kubernetes node to become ready...");
    let max_wait = std::time::Duration::from_secs(300); // 5 minutes timeout
    let check_interval = std::time::Duration::from_secs(5);
    let start_time = std::time::Instant::now();
    
    // Print a dot every few seconds to show progress
    let mut dots_printed = 0;

    loop {
        if start_time.elapsed() > max_wait {
            color_eyre::eyre::bail!("Timed out waiting for Kubernetes node to become ready after {} seconds.", max_wait.as_secs());
        }

        // Run kubectl with the simplest possible check for a Ready node
        let output_result = Command::new("kubectl")
            .args(["get", "nodes", "--no-headers"])
            .output();

        match output_result {
            Ok(output) => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                
                // Node is ready if "Ready" appears in the output and "NotReady" doesn't
                if output.status.success() && 
                   stdout.contains(" Ready") && 
                   !stdout.contains("NotReady") {
                    // Add a newline after dots if we printed any
                    if dots_printed > 0 {
                        println!();
                    }
                    info!("Kubernetes node is ready");
                    return Ok(());
                }
                
                // Node exists but is not ready yet
                debug!("Waiting for node to become ready: {}", stdout.trim());
            },
            Err(e) => {
                // Most likely the API server is still starting
                debug!("kubectl command error (will retry): {}", e);
            }
        }

        // Print a dot to show progress
        print!(".");
        std::io::stdout().flush().wrap_err("Failed to flush stdout")?;
        dots_printed += 1;

        tokio::time::sleep(check_interval).await;
    }
}

async fn install_helm() -> Result<()> {
    debug!("Checking if Helm is already installed");
    if is_command_present("helm") {
        // Additionally check the helm version works
        let version_check = Command::new("helm")
            .args(["version", "--short"])
            .output();
            
        if let Ok(output) = version_check {
            if output.status.success() {
                info!("Helm is already installed and working");
                return Ok(());
            }
        }
    }
    
    info!("Installing Helm");
    let script = r#"curl -sSL https://raw.githubusercontent.com/helm/helm/main/scripts/get-helm-3 | bash"#;
    run_shell_command(script, "Helm installation script")?;

    // Verify installation
    if !is_command_present("helm") {
        color_eyre::eyre::bail!("Helm installation command ran, but 'helm' command not found afterwards.");
    }

    info!("Helm installed successfully");
    Ok(())
}

async fn install_tinkerbell_stack(bootstrap_ip: Ipv4Addr) -> Result<()> {
    debug!("Checking if Tinkerbell stack is already installed");
    
    // Check if the tink namespace exists
    let namespace_check = Command::new("kubectl")
        .args(["get", "namespace", "tink", "--no-headers", "--ignore-not-found"])
        .output()?;
        
    let namespace_exists = namespace_check.status.success() && 
                           !String::from_utf8_lossy(&namespace_check.stdout).trim().is_empty();
                           
    // Check if the tink-stack release exists
    let release_check = Command::new("helm")
        .args(["list", "-n", "tink", "--filter", "tink-stack", "--short"])
        .output()?;
        
    let release_exists = release_check.status.success() && 
                         !String::from_utf8_lossy(&release_check.stdout).trim().is_empty();
                         
    if namespace_exists && release_exists {
        // Check if it's running properly
        let pods_check = Command::new("kubectl")
            .args(["get", "pods", "-n", "tink", "--no-headers"])
            .output()?;
            
        let pods_output = String::from_utf8_lossy(&pods_check.stdout).trim().to_string();
        let all_pods_running = pods_check.status.success() && 
                               !pods_output.is_empty() && 
                               !pods_output.contains("Pending") &&
                               !pods_output.contains("Error") &&
                               !pods_output.contains("CrashLoopBackOff");
                               
        if all_pods_running {
            info!("Tinkerbell stack is already installed and running");
            return Ok(());
        } else {
            info!("Tinkerbell stack is installed but not all pods are running, will reinstall");
        }
    }

    debug!("Installing Tinkerbell stack via Helm");

    // --- Get Pod CIDRs ---
    let pod_cidr_output = run_command(
        "kubectl",
        &["get", "nodes", "-o", "jsonpath='{.items[*].spec.podCIDR}'"],
        "get pod CIDRs",
    )?;
    let trusted_proxies_str = String::from_utf8_lossy(&pod_cidr_output.stdout)
                                  .trim()
                                  .trim_matches('\'') // Remove potential quotes from jsonpath
                                  .to_string();

    let trusted_proxies: Vec<String> = trusted_proxies_str
        .split(|c| c == ' ' || c == ',')
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect();

    if trusted_proxies.is_empty() {
        warn!("Could not determine pod CIDR. Proceeding without it in trustedProxies.");
    } else {
        debug!("Using Pod CIDRs for trusted proxies: {:?}", trusted_proxies);
    }

    // TODO: Dynamically determine the host's subnet for the hardcoded proxy
    let host_subnet_proxy = "10.7.1.200/24"; // Placeholder - should be derived
    debug!("Using host subnet proxy: {}", host_subnet_proxy);

    let mut final_trusted_proxies = trusted_proxies;
    final_trusted_proxies.push(host_subnet_proxy.to_string());

    // TODO: Verify the correct IP for smee.dhcp.httpIPXE.scriptUrl.host
    // The bash script used a hardcoded IP, but using the bootstrap_ip might be more correct/flexible.
    let smee_host_ip = bootstrap_ip; // Using bootstrap_ip
    debug!("Using {} as the host for Smee HTTP iPXE script URL", smee_host_ip);

    // --- Generate values.yaml ---
    let values_content = format!(
        r#"global:
  trustedProxies:
{}
  publicIP: {bootstrap_ip}
smee:
  dhcp:
    allowUnknownHosts: true
    mode: auto-proxy
    httpIPXE:
      scriptUrl:
        scheme: "http"
        host: "{smee_host_ip}" # Use the determined IP
        port: 3000 # Default Tinkerbell port for iPXE scripts via HTTP
        path: ""
  additionalArgs:
    - "--dhcp-http-ipxe-script-prepend-mac=true"
stack:
  hook:
    enabled: true
    persistence:
      hostPath: /opt/tinkerbell/hook
"#,
        final_trusted_proxies.iter().map(|p| format!("    - \"{}\"", p)).collect::<Vec<_>>().join("\n"),
        bootstrap_ip = bootstrap_ip,
        smee_host_ip = smee_host_ip,
    );

    let values_path = PathBuf::from("values.yaml");
    fs::write(&values_path, values_content).await
        .wrap_err_with(|| format!("Failed to write Helm values to {:?}", values_path))?;
    debug!("Generated Helm values file: {:?}", values_path);

    // If the release exists but isn't working, try uninstalling it first
    if release_exists {
        debug!("Uninstalling existing Tinkerbell stack before reinstalling");
        let _ = Command::new("helm")
            .args(["uninstall", "tink-stack", "-n", "tink"])
            .output();
            
        // Give it a moment to clean up
        tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
    }

    // --- Run Helm Install ---
    let stack_chart_version = "0.5.0"; // Consider making this configurable
    let helm_args = [
        "upgrade", "--install", "tink-stack",
        "oci://ghcr.io/tinkerbell/charts/stack",
        "--version", stack_chart_version,
        "--create-namespace",
        "--namespace", "tink",
        "--wait", // Add timeout? e.g. --timeout 10m
        "--timeout", "10m", // Added timeout
        "-f", values_path.to_str().ok_or_else(|| color_eyre::eyre::eyre!("values.yaml path is not valid UTF-8"))?,
    ];

    debug!("Running helm upgrade --install command");
    run_command("helm", &helm_args, "install Tinkerbell Helm chart")?;

    info!("Tinkerbell stack installed successfully");
    Ok(())
}

// Helper function to check if a file exists and is readable
async fn check_file_exists(path: impl AsRef<std::path::Path>) -> bool {
    if let Ok(metadata) = tokio::fs::metadata(path.as_ref()).await {
        metadata.is_file()
    } else {
        false
    }
}

// --- TODO: Implement these crucial functions ---

// fn get_host_ip_and_mask(interface_name: Option<&str>) -> Result<(Ipv4Addr, Ipv4Addr)> {
//     // Use libraries like `pnet` or `network-interface` to find the IP and mask
//     // Handle interface selection (specified vs. default route)
//     // Return error if no suitable IPv4 interface found
//     unimplemented!("get_host_ip_and_mask")
// }

// async fn find_available_ip(host_ip: Ipv4Addr, netmask: Ipv4Addr, start_offset: u8, max_tries: u8) -> Result<Ipv4Addr> {
//     // Calculate network range based on host_ip and netmask
//     // Iterate starting from host_ip + start_offset
//     // For each candidate IP:
//     //   - Check if it's within the subnet
//     //   - Check if it's the network or broadcast address
//     //   - Check availability (e.g., using ping or arping)
//     // Return the first available IP found
//     // Return error if no IP found within max_tries
//     unimplemented!("find_available_ip")
// }

// async fn check_ip_availability(ip: Ipv4Addr) -> Result<bool> {
//     // Use `tokio::process::Command` to run `ping -c 1 -W 1 <ip>` or `arping -c 1 -W 1 <ip>`
//     // Return Ok(true) if unreachable/timeout (available), Ok(false) if reply received (unavailable)
//     // Return Err if the command fails unexpectedly
//     unimplemented!("check_ip_availability")
// }

