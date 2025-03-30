use anyhow::{anyhow, Result};
use kube::{
    api::{Api, PostParams},
    Client, Error as KubeError, core::DynamicObject,
};
use serde_yaml;
use tracing::{info, error, warn};
use std::path::Path;
use tokio::fs;
use std::env;
use url::Url;
use std::collections::HashMap;

/// Initialize the OS templates in Kubernetes
pub async fn init_os_templates() -> Result<()> {
    info!("Initializing OS templates...");
    
    // Get Tinkerbell client
    let client = match crate::tinkerbell::get_client().await {
        Ok(c) => c,
        Err(e) => {
            warn!("Skipping OS template initialization: {}", e);
            return Err(anyhow!("Failed to get Kubernetes client: {}", e));
        }
    };
    
    // Get the bare base URL (without port) for template substitution
    let base_url_bare = get_base_url_without_port()?;
    
    // Check and install ubuntu-2204 template
    if let Err(e) = install_template(client, "ubuntu-2204", &base_url_bare).await {
        error!("Failed to install ubuntu-2204 template: {}", e);
        return Err(anyhow!("Failed to install ubuntu-2204 template: {}", e));
    }
    
    info!("OS templates initialization complete");
    Ok(())
}

/// Extract base URL without port from DRAGONFLY_BASE_URL environment variable
fn get_base_url_without_port() -> Result<String> {
    // Read required base URL from environment variable
    let base_url = match env::var("DRAGONFLY_BASE_URL") {
        Ok(url) => url,
        Err(_) => {
            // If not set, default to localhost for development
            warn!("DRAGONFLY_BASE_URL not set, using localhost as base URL for templates");
            "localhost".to_string()
        }
    };
    
    // Parse the URL to extract just the hostname without port
    let base_url_bare = if base_url.contains("://") {
        // Full URL with scheme
        match Url::parse(&base_url) {
            Ok(parsed_url) => {
                parsed_url.host_str().unwrap_or("localhost").to_string()
            },
            Err(_) => {
                // Fall back to simple splitting if URL parsing fails
                base_url.split(':').next().unwrap_or("localhost").to_string()
            }
        }
    } else if base_url.contains(':') {
        // Just hostname:port without scheme
        base_url.split(':').next().unwrap_or("localhost").to_string()
    } else {
        // Just hostname without port
        base_url
    };
    
    info!("Using base URL without port for templates: {}", base_url_bare);
    Ok(base_url_bare)
}

/// Check if a template exists in Kubernetes, and install it if it doesn't
async fn install_template(client: &Client, template_name: &str, base_url_bare: &str) -> Result<()> {
    // Create the API resource for Template CRD
    let template_api_resource = kube::core::ApiResource {
        group: "tinkerbell.org".to_string(),
        version: "v1alpha1".to_string(),
        kind: "Template".to_string(),
        api_version: "tinkerbell.org/v1alpha1".to_string(),
        plural: "templates".to_string(),
    };
    
    let template_api: Api<DynamicObject> = Api::namespaced_with(client.clone(), "tink", &template_api_resource);
    
    // Check if template already exists
    match template_api.get(template_name).await {
        Ok(_) => {
            info!("Template '{}' already exists in Tinkerbell, skipping installation", template_name);
            Ok(())
        },
        Err(KubeError::Api(ae)) if ae.code == 404 => {
            info!("Template '{}' not found in Tinkerbell, installing...", template_name);
            install_template_from_file(client, template_name, base_url_bare).await
        },
        Err(e) => {
            error!("Error checking for template '{}': {}", template_name, e);
            Err(anyhow!("Error checking for template: {}", e))
        }
    }
}

/// Install a template from a YAML file
async fn install_template_from_file(client: &Client, template_name: &str, base_url_bare: &str) -> Result<()> {
    // Determine file paths
    let os_templates_dir = Path::new("/opt/dragonfly/os-templates");
    let fallback_dir = Path::new("os-templates");
    
    let template_path = if os_templates_dir.exists() {
        os_templates_dir.join(format!("{}.yml", template_name))
    } else {
        fallback_dir.join(format!("{}.yml", template_name))
    };
    
    info!("Loading template from: {:?}", template_path);
    
    // Read the template file
    let template_yaml = match fs::read_to_string(&template_path).await {
        Ok(content) => content,
        Err(e) => {
            error!("Failed to read template file {:?}: {}", template_path, e);
            return Err(anyhow!("Failed to read template file: {}", e));
        }
    };
    
    // Fix metadata_urls to work with the correct port
    let template_yaml = fix_metadata_urls(&template_yaml, base_url_bare);
    
    // Parse YAML to get the DynamicObject
    let dynamic_obj: DynamicObject = match serde_yaml::from_str(&template_yaml) {
        Ok(obj) => obj,
        Err(e) => {
            error!("Failed to parse template YAML: {}", e);
            return Err(anyhow!("Failed to parse template YAML: {}", e));
        }
    };
    
    // Create the API resource for Template CRD
    let template_api_resource = kube::core::ApiResource {
        group: "tinkerbell.org".to_string(),
        version: "v1alpha1".to_string(),
        kind: "Template".to_string(),
        api_version: "tinkerbell.org/v1alpha1".to_string(),
        plural: "templates".to_string(),
    };
    
    let template_api: Api<DynamicObject> = Api::namespaced_with(client.clone(), "tink", &template_api_resource);
    
    // Create the template
    match template_api.create(&PostParams::default(), &dynamic_obj).await {
        Ok(_) => {
            info!("Successfully created template '{}'", template_name);
            Ok(())
        },
        Err(e) => {
            error!("Failed to create template '{}': {}", template_name, e);
            Err(anyhow!("Failed to create template: {}", e))
        }
    }
}

/// Fix the metadata_urls in the template YAML to work with the correct port
fn fix_metadata_urls(yaml: &str, base_url_bare: &str) -> String {
    // Replace {{ base_url }} with {{ base_url_bare }} in the metadata_urls line
    // to ensure the port will be correctly appended
    let replacement_vars = HashMap::from([
        ("base_url".to_string(), base_url_bare.to_string()),
    ]);
    
    // In a more complex case, we might need to parse and modify the YAML structure,
    // but for now a simple replacement should work since we just have a bare URL.
    let mut result = yaml.to_string();
    for (key, value) in replacement_vars {
        result = result.replace(&format!("{{ {} }}", key), &value);
    }
    
    result
}

/// Helper function for unit tests to parse a URL without accessing environment variables
fn parse_url_to_bare(url: &str) -> String {
    if url.contains("://") {
        // Full URL with scheme
        match Url::parse(url) {
            Ok(parsed_url) => {
                parsed_url.host_str().unwrap_or("localhost").to_string()
            },
            Err(_) => {
                // Fall back to simple splitting if URL parsing fails
                url.split(':').next().unwrap_or("localhost").to_string()
            }
        }
    } else if url.contains(':') {
        // Just hostname:port without scheme
        url.split(':').next().unwrap_or("localhost").to_string()
    } else {
        // Just hostname without port
        url.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_url_parsing() {
        // Test cases for different URL formats
        let test_cases = vec![
            // Full URLs with scheme
            ("http://example.com:3000", "example.com"),
            ("https://server.domain.com:8443", "server.domain.com"),
            ("http://192.168.1.1:8080", "192.168.1.1"),
            
            // Hostname:port format
            ("example.com:3000", "example.com"),
            ("192.168.1.1:8080", "192.168.1.1"),
            
            // Just hostname
            ("example.com", "example.com"),
            ("192.168.1.1", "192.168.1.1"),
            
            // Edge cases
            ("localhost", "localhost"),
            ("localhost:3000", "localhost"),
        ];
        
        for (input, expected) in test_cases {
            let result = parse_url_to_bare(input);
            assert_eq!(result, expected, "Failed parsing URL: {}", input);
        }
    }
} 