//! macOS-specific UI functionality for Dragonfly
//! This module is only compiled on macOS and provides a status bar icon

use anyhow::{Result, Context};
use image::io::Reader as ImageReader;
use std::io::Cursor;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::watch;
use tracing::info;
use tracing::warn;

// Use newer API version (0.20.0)
use tray_icon::Icon;

// Include the PNG icon as a static byte array
const ICON_DATA: &[u8] = include_bytes!("../static/icons/dragonfly_icon.png");

// Static flag to prevent multiple initialization
static INITIALIZED: AtomicBool = AtomicBool::new(false);

// Setup the status bar icon
pub async fn setup_status_bar(mode: &str, shutdown_tx: watch::Sender<()>) -> Result<()> {
    // Prevent multiple initializations
    if INITIALIZED.swap(true, Ordering::SeqCst) {
        info!("macOS status bar icon already initialized, skipping");
        return Ok(());
    }

    info!("Setting up macOS status bar icon");
    
    // Write a simple Swift program to create a status bar icon
    // This is much easier to maintain than AppleScript
    let swift_code = format!(
r#"import Cocoa

// Set the app to be an accessory app (no dock icon)
NSApplication.shared.setActivationPolicy(.accessory)

// Set a proper application name using process info
let appName = "Dragonfly Server"
ProcessInfo.processInfo.processName = appName

class StatusBarController {{
    private var statusBar: NSStatusBar
    private var statusItem: NSStatusItem
    private var menu: NSMenu
    
    init() {{
        statusBar = NSStatusBar.system
        statusItem = statusBar.statusItem(withLength: NSStatusItem.variableLength)
        menu = NSMenu()
        
        // Set the title emoji
        statusItem.button?.title = "🐉"
        
        // Create menu items
        let titleItem = NSMenuItem(title: "Running in {} Mode", action: nil, keyEquivalent: "")
        titleItem.isEnabled = false
        menu.addItem(titleItem)
        
        menu.addItem(NSMenuItem.separator())
        
        let openDashboardItem = NSMenuItem(title: "Open Dashboard", action: #selector(openDashboard), keyEquivalent: "")
        openDashboardItem.target = self
        menu.addItem(openDashboardItem)
        
        let viewLogsItem = NSMenuItem(title: "View Logs", action: #selector(viewLogs), keyEquivalent: "l")
        viewLogsItem.target = self
        menu.addItem(viewLogsItem)
        
        menu.addItem(NSMenuItem.separator())
        
        let quitItem = NSMenuItem(title: "Quit Dragonfly", action: #selector(quitApp), keyEquivalent: "q")
        quitItem.target = self
        menu.addItem(quitItem)
        
        // Attach the menu to the status item
        statusItem.menu = menu
    }}
    
    @objc func openDashboard() {{
        let url = URL(string: "http://localhost:3000")!
        NSWorkspace.shared.open(url)
    }}
    
    @objc func viewLogs() {{
        // Open system logs filtered to show Dragonfly logs
        // This works in both macOS 12+ (log show) and older versions (Console.app)
        let task = Process()
        task.launchPath = "/usr/bin/open"
        
        // Try to use the 'log' command first
        if FileManager.default.fileExists(atPath: "/usr/bin/log") {{
            // Create an AppleScript to open Terminal with log command
            let script = NSAppleScript(source: """
                tell application "Terminal"
                    do script "log stream --predicate 'processImagePath contains \"dragonfly\"' --style compact"
                    activate
                end tell
                """)
            script?.executeAndReturnError(nil)
        }} else {{
            // Fallback to Console.app
            task.arguments = ["-a", "Console"]
            try? task.run()
        }}
    }}
    
    @objc func quitApp() {{
        // Create a more robust approach to quit all Dragonfly processes
        print("Quitting Dragonfly...")
        
        let cleanupTask = {{
            // Run all termination logic here
            
            // First attempt to signal dragonfly processes to exit gracefully
            let mainTask = Process()
            mainTask.launchPath = "/usr/bin/pkill"
            mainTask.arguments = ["-f", "dragonfly"]
            try? mainTask.run()
            
            // Wait a moment for graceful shutdown
            Thread.sleep(forTimeInterval: 0.5)
            
            // Try to forcefully kill any remaining processes
            let forceTask = Process()
            forceTask.launchPath = "/usr/bin/pkill"
            forceTask.arguments = ["-9", "-f", "dragonfly"]
            try? forceTask.run()
            
            // Also kill any Swift UI processes we created
            let uiTask = Process()
            uiTask.launchPath = "/usr/bin/pkill"
            uiTask.arguments = ["-f", "dragonfly_status_bar.swift"]
            try? uiTask.run()
            
            // Finally try to kill any processes using port 3000
            let portTask = Process()
            portTask.launchPath = "/bin/sh"
            portTask.arguments = ["-c", "lsof -i:3000 -t | xargs kill -9 >/dev/null 2>&1 || true"]
            try? portTask.run()
        }}
        
        // Run the cleanup in a background thread
        DispatchQueue.global(qos: .userInitiated).async {{
            // Run cleanup
            cleanupTask()
            
            // Exit the app after a short delay
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.5) {{
                NSApplication.shared.terminate(nil)
            }}
        }}
    }}
}}

// Create the application
let app = NSApplication.shared
app.applicationIconImage = NSImage(named: NSImage.Name("NSApplicationIcon"))
let controller = StatusBarController()

// Run the app
app.run()
"#, mode);

    // Write the Swift code to a temporary file
    let swift_path = "/tmp/dragonfly_status_bar.swift";
    info!("Writing Swift status bar application to {}", swift_path);
    tokio::fs::write(swift_path, swift_code).await?;
    
    // Compile and run the Swift program
    info!("Launching Swift status bar application with: swift {}", swift_path);
    let spawn_result = Command::new("swift")
        .arg(swift_path)
        .spawn()
        .context("Failed to launch status bar app (is Swift installed?)");
        
    match spawn_result {
        Ok(output) => {
            info!("macOS status bar icon launched with PID: {:?}", output.id());

            // Check if the Swift process is actually running after a short delay
            tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
            
            let check_pid = Command::new("pgrep")
                .args(["-f", "dragonfly_status_bar.swift"])
                .output();
            
            match check_pid {
                Ok(output) if output.status.success() && !output.stdout.is_empty() => {
                    // Process is running, set up cleanup on shutdown
                    let mut shutdown_rx = shutdown_tx.subscribe();
                    tokio::spawn(async move {
                        // Wait for shutdown signal
                        let _ = shutdown_rx.changed().await;
                        
                        // Clean up by killing the Swift app
                        let _ = Command::new("pkill")
                            .args(["-f", "dragonfly_status_bar.swift"])
                            .output();
                            
                        // Remove the Swift file
                        let _ = tokio::fs::remove_file(swift_path).await;
                    });
                    
                    Ok(())
                },
                _ => {
                    // Reset the initialized flag so we can try again next time
                    INITIALIZED.store(false, Ordering::SeqCst);
                    warn!("macOS status bar icon process started but exited immediately");
                    Err(anyhow::anyhow!("Status bar process exited immediately after launch"))
                }
            }
        },
        Err(e) => {
            // Reset the initialized flag so we can try again next time
            INITIALIZED.store(false, Ordering::SeqCst);
            Err(e)
        }
    }
}

// Load the icon from the embedded data - keeping this for future use if needed
fn load_icon() -> Result<Icon> {
    let img = ImageReader::new(Cursor::new(ICON_DATA))
        .with_guessed_format()?
        .decode()?;
    
    // Get dimensions before converting to rgba8
    let width = img.width();
    let height = img.height();
    
    // Convert to rgba8 and create icon
    let rgba8 = img.into_rgba8();
    Ok(Icon::from_rgba(rgba8.into_raw(), width, height)?)
} 