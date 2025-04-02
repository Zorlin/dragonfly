use assert_cmd::prelude::*;
use std::process::{Command, Output};
// Import necessary items for performing checks within the test
use dragonfly_server::{status, database_exists}; // Correct import path
use color_eyre::Result;

#[tokio::test] // Mark test as async
async fn test_scenario_a_dynamically_checks_output() -> Result<()> {
    // This test verifies that running `dragonfly` with no arguments 
    // performs the correct checks and prints output appropriate 
    // for the environment the test runs in.
    
    println!("Performing pre-flight checks within test...");
    // 1. Perform checks to determine expected state
    let db_exists = database_exists().await;
    println!("Test Check: Database Exists? {}", db_exists);
    
    let k8s_ok = status::check_kubernetes_connectivity().await.is_ok();
    println!("Test Check: Kubernetes Connection OK? {}", k8s_ok);
    
    let sts_ok = if k8s_ok {
        status::check_dragonfly_statefulset_status().await 
            .map(|ready| ready)
            .unwrap_or(false)
    } else {
        false
    };
    println!("Test Check: StatefulSet Status OK? {}", sts_ok);

    let webui_ok = if k8s_ok {
        status::get_webui_address().await.is_ok() 
    } else {
        false
    };
    println!("Test Check: WebUI Address OK? {}", webui_ok);
    println!("Checks complete. Running dragonfly command...");

    // 2. Run the command and capture output
    let mut cmd = Command::cargo_bin("dragonfly")?;
    let output = cmd.output().expect("Failed to execute dragonfly command");

    // 3. Assert command success
    assert!(output.status.success(), "Dragonfly command failed. Stderr: {}", String::from_utf8_lossy(&output.stderr));

    // 4. Get stdout as string for checks
    let stdout_str = String::from_utf8_lossy(&output.stdout);
    println!("-- Dragonfly stdout --\n{}\n-- End Dragonfly stdout --", stdout_str);

    // 5. Perform individual assertions on stdout

    // Check for install status message based on DB existence
    if db_exists {
        assert!(stdout_str.contains("Dragonfly is installed."), "Missing 'installed' message");

        // If installed, check K8s related outputs based on checks
        if k8s_ok {
            assert!(stdout_str.contains("Kubernetes is reachable."), "Missing 'K8s reachable' message");
            if sts_ok {
                assert!(stdout_str.contains("Dragonfly StatefulSet is running."), "Missing 'STS running' message");
            } else {
                // Use the actual error format observed in logs if STS check fails
                assert!(stdout_str.contains("Dragonfly StatefulSet is not running or not found."), "Missing 'STS not running' message"); 
            }
            if webui_ok {
                assert!(stdout_str.contains("Web UI is likely available at:"), "Missing 'WebUI available' message");
            } else {
                // Check for the specific messages for WebUI not found/error
                let webui_service_not_found = stdout_str.contains("Web UI Service not found or has no LoadBalancer ingress/NodePort.");
                let webui_error = stdout_str.contains("Could not determine Web UI address");
                assert!(webui_service_not_found || webui_error, "Missing 'WebUI not found or error' message");
            }
        } else {
            // Use the actual error format observed in logs if K8s check fails
            assert!(stdout_str.contains("Could not connect to Kubernetes"), "Missing 'K8s connect error' message"); 
        }
    } else {
        assert!(stdout_str.contains("Dragonfly is not installed."), "Missing 'not installed' message");
        assert!(stdout_str.contains("To get started, run: dragonfly install"), "Missing 'run install' hint");
    }

    // NOW check for help text AFTER status messages
    assert!(stdout_str.contains("Usage: dragonfly [OPTIONS] [COMMAND]"), "Missing usage text"); 
    
    println!("Dragonfly command finished. Individual assertions checked.");
    Ok(())
}

// Note: The specific output based on installation state (DB exists, K8s status, etc.)
// is tested via unit tests for `handle_default_invocation` in src/main.rs.
// This integration test focuses only on the environment-independent behavior.

// TODO: Add test for the 'installed' case. This is difficult because it requires:
// 1. A valid Dragonfly database to exist where the test runs.
// 2. A running Kubernetes cluster accessible via KUBECONFIG.
// 3. The Dragonfly statefulset and tink-stack service deployed in the cluster.
// Mocking these dependencies for an integration test running the binary is complex.
// Consider unit testing the `handle_default_invocation` function by injecting dependencies/results. 