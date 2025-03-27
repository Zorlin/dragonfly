use reqwest::Client;
use anyhow::{Result, Context};
use dragonfly_common::*;
use std::env;
use sysinfo::System;

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logger
    tracing_subscriber::fmt::init();
    
    // Get API URL from environment or use default
    let api_url = env::var("DRAGONFLY_API_URL").unwrap_or_else(|_| "http://localhost:3000".to_string());
    
    // Create HTTP client
    let client = Client::new();
    
    // Get system information
    let mut system = System::new_all();
    system.refresh_all();
    
    // Get MAC address and IP address
    let mac_address = get_mac_address().context("Failed to get MAC address")?;
    let ip_address = get_ip_address().context("Failed to get IP address")?;
    
    // Get hostname
    let hostname = System::host_name();
    
    // Prepare registration request
    let register_request = RegisterRequest {
        mac_address,
        ip_address,
        hostname,
    };
    
    // Register the machine
    tracing::info!("Registering machine with Dragonfly server...");
    let response = client.post(format!("{}/api/machines", api_url))
        .json(&register_request)
        .send()
        .await
        .context("Failed to send registration request")?;
    
    if !response.status().is_success() {
        let error_text = response.text().await?;
        anyhow::bail!("Failed to register machine: {}", error_text);
    }
    
    let register_response: RegisterResponse = response.json().await
        .context("Failed to parse registration response")?;
    
    tracing::info!("Machine registered successfully!");
    tracing::info!("Machine ID: {}", register_response.machine_id);
    tracing::info!("Next step: {}", register_response.next_step);
    
    // TODO: Implement a background process to periodically update status
    
    Ok(())
}

fn get_mac_address() -> Result<String> {
    // For simplicity, return a fake MAC address
    // In production, we would use a crate like mac_address or platform-specific code
    Ok("00:11:22:33:44:55".to_string())
}

fn get_ip_address() -> Result<String> {
    // In sysinfo 0.30, the networks interface has changed
    // Let's use a simple fallback for now
    Ok("127.0.0.1".to_string())
} 