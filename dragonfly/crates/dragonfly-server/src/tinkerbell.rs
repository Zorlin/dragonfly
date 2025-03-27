use anyhow::{anyhow, Result};
use kube::{
    api::{Api, PostParams, PatchParams, Patch},
    Client, Error as KubeError, core::DynamicObject,
};
use serde::{Deserialize, Serialize};
use tokio::sync::OnceCell;
use tracing::{error, info, warn};
use dragonfly_common::models::Machine;

// Define a static Kubernetes client
static KUBE_CLIENT: OnceCell<Client> = OnceCell::const_new();

// Initialize the Kubernetes client using KUBECONFIG
pub async fn init() -> Result<()> {
    // Expand the tilde in KUBECONFIG if present
    if let Ok(kubeconfig) = std::env::var("KUBECONFIG") {
        if kubeconfig.starts_with('~') {
            // Replace tilde with home directory
            if let Ok(home) = std::env::var("HOME") {
                let expanded_path = kubeconfig.replacen('~', &home, 1);
                std::env::set_var("KUBECONFIG", &expanded_path);
                info!("Expanded KUBECONFIG path: {}", expanded_path);
            }
        }
    }
    
    // Create a new client using the current environment (KUBECONFIG)
    let client = Client::try_default().await
        .map_err(|e| anyhow!("Failed to create Kubernetes client: {}", e))?;
    
    // Test the client to ensure it can connect to the cluster
    client
        .apiserver_version()
        .await
        .map_err(|e| anyhow!("Failed to connect to Kubernetes API server: {}", e))?;
    
    // Set the global client
    if let Err(_) = KUBE_CLIENT.set(client) {
        return Err(anyhow!("Failed to set global Kubernetes client"));
    }
    
    info!("Kubernetes client initialized successfully");
    Ok(())
}

// Get the Kubernetes client
async fn get_client() -> Result<&'static Client> {
    KUBE_CLIENT.get().ok_or_else(|| anyhow!("Kubernetes client not initialized"))
}

// Define the Hardware Custom Resource using serde
#[derive(Debug, Serialize, Deserialize, Clone)]
struct Hardware {
    #[serde(rename = "apiVersion")]
    api_version: String,
    kind: String,
    metadata: Metadata,
    spec: HardwareSpec,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct Metadata {
    name: String,
    namespace: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    labels: Option<std::collections::HashMap<String, String>>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct HardwareSpec {
    #[serde(skip_serializing_if = "Option::is_none")]
    disks: Option<Vec<DiskSpec>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    interfaces: Option<Vec<InterfaceSpec>>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct DiskSpec {
    device: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct InterfaceSpec {
    #[serde(skip_serializing_if = "Option::is_none")]
    dhcp: Option<DHCPSpec>,
    #[serde(skip_serializing_if = "Option::is_none")]
    netboot: Option<NetbootSpec>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct DHCPSpec {
    #[serde(skip_serializing_if = "Option::is_none")]
    arch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    hostname: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ip: Option<IPSpec>,
    #[serde(skip_serializing_if = "Option::is_none")]
    lease_time: Option<u32>,
    mac: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    name_servers: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    uefi: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct IPSpec {
    address: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    gateway: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    netmask: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct NetbootSpec {
    #[serde(rename = "allowPXE")]
    #[serde(skip_serializing_if = "Option::is_none")]
    allow_pxe: Option<bool>,
    #[serde(rename = "allowWorkflow")]
    #[serde(skip_serializing_if = "Option::is_none")]
    allow_workflow: Option<bool>,
}

// Register a machine with Tinkerbell
pub async fn register_machine(machine: &Machine) -> Result<()> {
    // Get the Kubernetes client
    let client = match get_client().await {
        Ok(c) => c,
        Err(e) => {
            warn!("Skipping Tinkerbell registration: {}", e);
            return Ok(());
        }
    };
    
    // Create a unique name for the hardware resource based on MAC address
    let resource_name = format!("machine-{}", machine.mac_address.replace(":", "-"));
    
    // Extract hostname from machine
    let hostname = machine.hostname.clone().unwrap_or_else(|| resource_name.clone());
    
    info!("Registering machine {} with Tinkerbell", resource_name);
    
    // Create the Hardware resource, focusing only on the specific fields we need to set
    // to reduce conflicts with other field managers
    let hardware = Hardware {
        api_version: "tinkerbell.org/v1alpha1".to_string(),
        kind: "Hardware".to_string(),
        metadata: Metadata {
            name: resource_name.clone(),
            namespace: "tink".to_string(),
            labels: None,
        },
        spec: HardwareSpec {
            disks: Some(machine.disks.iter().map(|disk| DiskSpec {
                device: disk.device.clone(),
            }).collect()),
            interfaces: Some(vec![InterfaceSpec {
                dhcp: Some(DHCPSpec {
                    arch: Some("x86_64".to_string()),
                    hostname: Some(hostname),
                    ip: Some(IPSpec {
                        address: machine.ip_address.clone(),
                        gateway: None,
                        netmask: None,
                    }),
                    lease_time: Some(86400),
                    mac: machine.mac_address.clone(),
                    name_servers: Some(machine.nameservers.clone()),
                    uefi: Some(true),
                }),
                netboot: Some(NetbootSpec {
                    allow_pxe: Some(true),
                    allow_workflow: Some(true),
                }),
            }]),
        },
    };
    
    // Convert the Hardware resource to JSON
    let hardware_json = serde_json::to_value(&hardware)?;
    
    // Create the ApiResource for the Hardware CRD
    let api_resource = kube::core::ApiResource {
        group: "tinkerbell.org".to_string(),
        version: "v1alpha1".to_string(),
        kind: "Hardware".to_string(),
        api_version: "tinkerbell.org/v1alpha1".to_string(),
        plural: "hardware".to_string(),
    };
    
    info!("Using Kubernetes API Resource: group={}, version={}, kind={}, plural={}", 
          api_resource.group, api_resource.version, api_resource.kind, api_resource.plural);
    
    // Create a dynamic API to interact with the Hardware custom resource
    let api: Api<DynamicObject> = Api::namespaced_with(client.clone(), "tink", &api_resource);
    
    // Create a DynamicObject from our hardware_json
    let mut dynamic_obj = DynamicObject {
        metadata: kube::core::ObjectMeta {
            name: Some(resource_name.clone()),
            namespace: Some("tink".to_string()),
            ..Default::default()
        },
        types: Some(kube::core::TypeMeta {
            api_version: "tinkerbell.org/v1alpha1".to_string(),
            kind: "Hardware".to_string(),
        }),
        data: hardware_json,
    };
    
    // Check if the hardware resource already exists
    match api.get(&resource_name).await {
        Ok(_existing) => {
            info!("Found existing Hardware resource in Tinkerbell: {}", resource_name);
            
            // Use JSON merge patch instead of server-side apply
            let patch_params = PatchParams::default();
            
            info!("Applying update via JSON merge patch");
            
            // Use JSON merge patch to update the resource
            match api.patch(&resource_name, &patch_params, &Patch::Merge(dynamic_obj)).await {
                Ok(patched) => {
                    info!(
                        "Updated Hardware resource in Tinkerbell: {} (resourceVersion: {:?})",
                        resource_name,
                        patched.metadata.resource_version
                    );
                    Ok(())
                },
                Err(e) => {
                    error!("Failed to update Hardware resource in Tinkerbell: {}", e);
                    Err(anyhow!("Failed to update Hardware resource: {}", e))
                }
            }
        },
        Err(KubeError::Api(ae)) if ae.code == 404 => {
            info!("No existing Hardware resource found, creating new one: {}", resource_name);
            
            // For creation, ensure we have a clean metadata without resourceVersion
            dynamic_obj.metadata = kube::core::ObjectMeta {
                name: Some(resource_name.clone()),
                namespace: Some("tink".to_string()),
                ..Default::default()
            };
            
            // Create a new hardware resource
            match api.create(&PostParams::default(), &dynamic_obj).await {
                Ok(created) => {
                    info!(
                        "Created new Hardware resource in Tinkerbell: {} (initial resourceVersion: {:?})",
                        resource_name,
                        created.metadata.resource_version
                    );
                    Ok(())
                },
                Err(e) => {
                    error!("Failed to create Hardware resource in Tinkerbell: {}", e);
                    Err(anyhow!("Failed to create Hardware resource: {}", e))
                }
            }
        },
        Err(e) => {
            error!("Error checking Hardware resource in Tinkerbell: {}", e);
            Err(anyhow!("Error checking Hardware resource: {}", e))
        }
    }
} 