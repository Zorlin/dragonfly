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
    if KUBE_CLIENT.get().is_none() {
        info!("Kubernetes client not initialized, initializing now");
        
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
        let client = match Client::try_default().await {
            Ok(client) => client,
            Err(e) => {
                return Err(anyhow!("Failed to create Kubernetes client: {}", e));
            }
        };
        
        // Test the client to ensure it can connect to the cluster
        if let Err(e) = client.apiserver_version().await {
            return Err(anyhow!("Failed to connect to Kubernetes API server: {}", e));
        }
        
        // Set the global client
        if let Err(_) = KUBE_CLIENT.set(client) {
            return Err(anyhow!("Failed to set global Kubernetes client"));
        }
        
        info!("Kubernetes client initialized successfully");
    }
    
    KUBE_CLIENT.get().ok_or_else(|| anyhow!("Kubernetes client initialization failed"))
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
struct HardwareMetadata {
    instance: Instance,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct Instance {
    id: String,
    hostname: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct HardwareSpec {
    #[serde(skip_serializing_if = "Option::is_none")]
    metadata: Option<HardwareMetadata>,
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

    // Extract the memorable name from the machine
    let memorable_name = machine.memorable_name.clone().unwrap_or_else(|| resource_name.clone());

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
            metadata: Some(HardwareMetadata {
                instance: Instance {
                    id: memorable_name.clone(),
                    hostname: hostname.clone(),
                },
            }),
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

// Add this function to delete hardware resources
pub async fn delete_hardware(mac_address: &str) -> Result<()> {
    // Get the Kubernetes client
    let client = match get_client().await {
        Ok(c) => c,
        Err(e) => {
            warn!("Skipping Tinkerbell deletion: {}", e);
            return Err(anyhow!("Kubernetes client not initialized: {}", e));
        }
    };
    
    let resource_name = mac_address.to_lowercase();
    info!("Deleting hardware resource from Tinkerbell: {}", resource_name);
    
    // Create the ApiResource for the Hardware CRD
    let api_resource = kube::core::ApiResource {
        group: "tinkerbell.org".to_string(),
        version: "v1alpha1".to_string(),
        kind: "Hardware".to_string(),
        api_version: "tinkerbell.org/v1alpha1".to_string(),
        plural: "hardware".to_string(),
    };
    
    // Create a dynamic API to interact with the Hardware custom resource
    let api: Api<DynamicObject> = Api::namespaced_with(client.clone(), "tink", &api_resource);
    
    // Delete the hardware resource
    let hardware_result = api.delete(&resource_name, &kube::api::DeleteParams::default()).await;

    // Also delete any associated workflow
    let workflow_name = format!("os-install-{}", mac_address.replace(":", "-"));
    info!("Deleting workflow resource from Tinkerbell: {}", workflow_name);

    // Create the ApiResource for the Workflow CRD
    let workflow_api_resource = kube::core::ApiResource {
        group: "tinkerbell.org".to_string(),
        version: "v1alpha1".to_string(),
        kind: "Workflow".to_string(),
        api_version: "tinkerbell.org/v1alpha1".to_string(),
        plural: "workflows".to_string(),
    };

    // Create a dynamic API to interact with the Workflow custom resource
    let workflow_api: Api<DynamicObject> = Api::namespaced_with(client.clone(), "tink", &workflow_api_resource);

    // Delete the workflow resource
    let workflow_result = workflow_api.delete(&workflow_name, &kube::api::DeleteParams::default()).await;

    // Handle results
    match (hardware_result, workflow_result) {
        (Ok(_), Ok(_)) => {
            info!("Successfully deleted hardware and workflow resources");
            Ok(())
        },
        (Ok(_), Err(KubeError::Api(ae))) if ae.code == 404 => {
            info!("Successfully deleted hardware resource, workflow was not found");
            Ok(())
        },
        (Err(KubeError::Api(ae)), Ok(_)) if ae.code == 404 => {
            info!("Hardware resource not found, but successfully deleted workflow");
            Ok(())
        },
        (Err(KubeError::Api(ae1)), Err(KubeError::Api(ae2))) if ae1.code == 404 && ae2.code == 404 => {
            info!("Neither hardware nor workflow resources were found (already deleted)");
            Ok(())
        },
        (Err(e), _) => {
            error!("Failed to delete hardware resource from Tinkerbell: {}", e);
            Err(anyhow!("Failed to delete hardware resource: {}", e))
        },
        (_, Err(e)) => {
            error!("Failed to delete workflow resource from Tinkerbell: {}", e);
            Err(anyhow!("Failed to delete workflow resource: {}", e))
        }
    }
}

// Create a Workflow for OS installation
pub async fn create_workflow(machine: &Machine, _os_choice: &str) -> Result<()> {
    // Get the Kubernetes client
    let client = match get_client().await {
        Ok(c) => c,
        Err(e) => {
            warn!("Skipping Tinkerbell workflow creation: {}", e);
            return Ok(());
        }
    };
    
    // Use MAC address without colons as part of the workflow name
    let resource_name = format!("os-install-{}", machine.mac_address.replace(":", "-"));
    
    // Hardware reference name (matches what we create in register_machine)
    let hardware_ref = format!("machine-{}", machine.mac_address.replace(":", "-"));
    
    info!("Creating workflow {} for machine {}", resource_name, machine.id);
    
    // Map OS choice to template reference
    let template_ref = match machine.os_choice.as_ref() {
        Some(os) if os == "ubuntu-2204" => "ubuntu-2204",
        Some(os) if os == "ubuntu-2404" => "ubuntu-2404",
        Some(os) if os == "debian-12" => "debian-12",
        Some(os) if os == "proxmox" => "proxmox",
        Some(os) if os == "talos" => "talos",
        Some(os) => os,
        None => "ubuntu-2204", // Default if no OS choice is specified
    };
    
    // Create the Workflow resource
    let workflow_json = serde_json::json!({
        "apiVersion": "tinkerbell.org/v1alpha1",
        "kind": "Workflow",
        "metadata": {
            "name": resource_name,
            "namespace": "tink"
        },
        "spec": {
            "templateRef": template_ref,
            "hardwareRef": hardware_ref,
            "hardwareMap": {
                "device_1": machine.mac_address
            }
        }
    });
    
    // Create the ApiResource for the Workflow CRD
    let api_resource = kube::core::ApiResource {
        group: "tinkerbell.org".to_string(),
        version: "v1alpha1".to_string(),
        kind: "Workflow".to_string(),
        api_version: "tinkerbell.org/v1alpha1".to_string(),
        plural: "workflows".to_string(),
    };
    
    info!("Using Kubernetes API Resource for Workflow: group={}, version={}, kind={}, plural={}", 
          api_resource.group, api_resource.version, api_resource.kind, api_resource.plural);
    
    // Create a dynamic API to interact with the Workflow custom resource
    let api: Api<DynamicObject> = Api::namespaced_with(client.clone(), "tink", &api_resource);
    
    // Create a DynamicObject from our workflow_json
    let dynamic_obj = DynamicObject {
        metadata: kube::core::ObjectMeta {
            name: Some(resource_name.clone()),
            namespace: Some("tink".to_string()),
            ..Default::default()
        },
        types: Some(kube::core::TypeMeta {
            api_version: "tinkerbell.org/v1alpha1".to_string(),
            kind: "Workflow".to_string(),
        }),
        data: workflow_json,
    };
    
    // Check if the workflow resource already exists
    match api.get(&resource_name).await {
        Ok(_existing) => {
            info!("Found existing Workflow resource in Tinkerbell: {}", resource_name);
            
            // Use JSON merge patch to update the resource
            let patch_params = PatchParams::default();
            match api.patch(&resource_name, &patch_params, &Patch::Merge(&dynamic_obj)).await {
                Ok(patched) => {
                    info!(
                        "Updated Workflow resource in Tinkerbell: {} (resourceVersion: {:?})",
                        resource_name,
                        patched.metadata.resource_version
                    );
                    Ok(())
                },
                Err(e) => {
                    error!("Failed to update Workflow resource in Tinkerbell: {}", e);
                    Err(anyhow!("Failed to update Workflow resource: {}", e))
                }
            }
        },
        Err(KubeError::Api(ae)) if ae.code == 404 => {
            info!("No existing Workflow resource found, creating new one: {}", resource_name);
            
            // Create a new workflow resource
            match api.create(&PostParams::default(), &dynamic_obj).await {
                Ok(created) => {
                    info!(
                        "Created new Workflow resource in Tinkerbell: {} (initial resourceVersion: {:?})",
                        resource_name,
                        created.metadata.resource_version
                    );
                    Ok(())
                },
                Err(e) => {
                    error!("Failed to create Workflow resource in Tinkerbell: {}", e);
                    Err(anyhow!("Failed to create Workflow resource: {}", e))
                }
            }
        },
        Err(e) => {
            error!("Error checking Workflow resource in Tinkerbell: {}", e);
            Err(anyhow!("Error checking Workflow resource: {}", e))
        }
    }
}

// Define structs for the workflow status information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskInfo {
    pub name: String,
    pub status: String,
    pub started_at: String,
    pub duration: u64,
    pub reported_duration: u64,
    pub estimated_duration: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowInfo {
    pub state: String,
    pub current_action: Option<String>,
    pub progress: u8,
    pub tasks: Vec<TaskInfo>,
    pub estimated_completion: Option<String>,
    pub template_name: String,
}

// Create a static map to store historical timing data
use std::collections::HashMap;
use std::sync::RwLock;
use once_cell::sync::Lazy;

// Historical timing map indexed by template name, then action name
// This allows us to store different timing profiles for different OS templates
static HISTORICAL_TIMINGS: Lazy<RwLock<HashMap<String, HashMap<String, Vec<u64>>>>> = Lazy::new(|| {
    RwLock::new(HashMap::new())
});

// Calculate average time for a specific action based on historical data for a specific template
fn get_avg_time_for_action(template_name: &str, action_name: &str) -> Option<u64> {
    if let Ok(timings) = HISTORICAL_TIMINGS.read() {
        if let Some(template_timings) = timings.get(template_name) {
            if let Some(durations) = template_timings.get(action_name) {
                if !durations.is_empty() {
                    let sum: u64 = durations.iter().sum();
                    return Some(sum / durations.len() as u64);
                }
            }
            
            // If no data for this specific template/action, try to use data from any template as fallback
            // This handles the case where we have no template-specific data yet
            for (_, template_data) in timings.iter() {
                if let Some(durations) = template_data.get(action_name) {
                    if !durations.is_empty() {
                        let sum: u64 = durations.iter().sum();
                        return Some(sum / durations.len() as u64);
                    }
                }
            }
        }
    }
    None
}

// Load previously saved timing data from the database
pub async fn load_historical_timings() -> Result<()> {
    info!("Loading historical timing data from database");
    
    // Get timing data from database
    let timings = match crate::db::load_template_timings().await {
        Ok(t) => t,
        Err(e) => {
            warn!("Failed to load template timings from database: {}", e);
            return Ok(()); // Continue without historical data
        }
    };
    
    // Update in-memory timing data
    if let Ok(mut timing_map) = HISTORICAL_TIMINGS.write() {
        for timing in timings {
            let template_timings = timing_map
                .entry(timing.template_name)
                .or_insert_with(HashMap::new);
                
            template_timings.insert(timing.action_name, timing.durations);
        }
    }
    
    info!("Loaded historical timing data for {} templates", 
        HISTORICAL_TIMINGS.read().map(|map| map.len()).unwrap_or(0));
    
    Ok(())
}

// Store timing information after a successful workflow
fn store_timing_info(template_name: &str, tasks: &[TaskInfo]) {
    const MAX_TIMING_HISTORY: usize = 50; // Keep only the last 50 runs of timing data
    
    if let Ok(mut timings) = HISTORICAL_TIMINGS.write() {
        // Get or create the template's timing map
        let template_timings = timings
            .entry(template_name.to_string())
            .or_insert_with(HashMap::new);
            
        // Add each task's timing data and save to database
        for task in tasks {
            let durations = template_timings
                .entry(task.name.clone())
                .or_insert_with(Vec::new);
                
            // Only store reported_duration (actual time taken)
            durations.push(task.reported_duration);
            
            // Trim the list to keep only the most recent MAX_TIMING_HISTORY entries
            if durations.len() > MAX_TIMING_HISTORY {
                // Remove the oldest entries (those at the start of the vector)
                *durations = durations.iter().skip(durations.len() - MAX_TIMING_HISTORY).cloned().collect();
            }
            
            // Save to database asynchronously
            tokio::spawn(save_timing_to_db(
                template_name.to_string(),
                task.name.clone(),
                durations.clone()
            ));
        }
    }
}

// Save timing data to database asynchronously
async fn save_timing_to_db(template_name: String, action_name: String, durations: Vec<u64>) {
    if let Err(e) = crate::db::save_template_timing(&template_name, &action_name, &durations).await {
        warn!("Failed to save timing data for {}/{} to database: {}", template_name, action_name, e);
    }
}

// Get workflow information from Kubernetes for a specific machine
pub async fn get_workflow_info(machine: &Machine) -> Result<Option<WorkflowInfo>> {
    // Get the Kubernetes client
    let client = match get_client().await {
        Ok(c) => c,
        Err(e) => {
            warn!("Skipping workflow status check: {}", e);
            return Ok(None);
        }
    };
    
    // Create the workflow resource name based on the MAC address
    let workflow_name = format!("os-install-{}", machine.mac_address.replace(":", "-"));
    
    // Create the ApiResource for the Workflow CRD
    let api_resource = kube::core::ApiResource {
        group: "tinkerbell.org".to_string(),
        version: "v1alpha1".to_string(),
        kind: "Workflow".to_string(),
        api_version: "tinkerbell.org/v1alpha1".to_string(),
        plural: "workflows".to_string(),
    };
    
    // Create a dynamic API to interact with the Workflow custom resource
    let api: Api<DynamicObject> = Api::namespaced_with(client.clone(), "tink", &api_resource);
    
    // Try to get the workflow
    match api.get(&workflow_name).await {
        Ok(workflow) => {
            // Extract template reference from the workflow spec for time tracking
            let template_ref = workflow.data.get("spec")
                .and_then(|spec| spec.get("templateRef"))
                .and_then(|t| t.as_str())
                .unwrap_or("unknown");
            
            // Process workflow status from the DynamicObject
            if let Some(status) = workflow.data.get("status") {
                let state = status.get("state").and_then(|s| s.as_str()).unwrap_or("UNKNOWN");
                let current_action = status.get("currentAction").and_then(|a| a.as_str()).map(|s| s.to_string());
                
                // HACK: If a machine is stuck in STATE_RUNNING with current action "kexec to boot OS", 
                // it has likely successfully booted the OS. Mark it as Ready and delete the workflow.
                // STATE_FAILED is always considered a failure regardless of the current action.
                if (state == "STATE_RUNNING" && current_action.as_deref() == Some("kexec to boot OS")) ||
                   is_workflow_timed_out(status, current_action.as_deref()) {
                    info!("HACK: Detected machine {} in STATE_RUNNING for 'kexec to boot OS' or timed out. Marking as Ready and deleting workflow.", 
                          machine.id);
                    
                    // Update machine status to Ready
                    if let Err(e) = update_machine_status_on_success(machine).await {
                        warn!("Failed to update machine status after kexec detection: {}", e);
                    } else {
                        info!("Successfully marked machine {} as Ready", machine.id);
                    }
                    
                    // Delete the workflow
                    let delete_params = kube::api::DeleteParams::default();
                    match api.delete(&workflow_name, &delete_params).await {
                        Ok(_) => info!("Successfully deleted workflow {}", workflow_name),
                        Err(e) => warn!("Failed to delete workflow {}: {}", workflow_name, e),
                    }
                    
                    // Create a special WorkflowInfo to indicate this was handled by the hack
                    let workflow_info = WorkflowInfo {
                        state: "STATE_SUCCESS".to_string(),
                        current_action: Some("Completed via kexec detection".to_string()),
                        progress: 100,
                        tasks: vec![],
                        estimated_completion: Some("Deployment complete".to_string()),
                        template_name: template_ref.to_string(),
                    };
                    
                    return Ok(Some(workflow_info));
                }
                
                // Extract all tasks from the workflow
                let mut tasks = Vec::new();
                let mut total_seconds = 0;
                let mut completed_seconds = 0;
                let mut running_task_info = None;
                let mut running_task_started_at = None;
                
                if let Some(task_array) = status.get("tasks") {
                    if let Some(task_array) = task_array.as_array() {
                        for task_obj in task_array {
                            if let Some(actions) = task_obj.get("actions") {
                                if let Some(actions) = actions.as_array() {
                                    for action in actions {
                                        let name = action.get("name").and_then(|n| n.as_str()).unwrap_or("unknown").to_string();
                                        let status = action.get("status").and_then(|s| s.as_str()).unwrap_or("UNKNOWN").to_string();
                                        let started_at = action.get("startedAt").and_then(|s| s.as_str()).unwrap_or("").to_string();
                                        
                                        // Get actual duration from completed actions or estimate from template history
                                        let reported_seconds = action.get("seconds").and_then(|s| s.as_i64()).unwrap_or(0) as u64;
                                        let estimated_seconds = get_avg_time_for_action(template_ref, &name).unwrap_or(reported_seconds);
                                        
                                        // Use the reported seconds for completed tasks, but estimated seconds for planning
                                        let seconds = if status == "STATE_SUCCESS" {
                                            reported_seconds  // Use actual time for completed tasks
                                        } else {
                                            estimated_seconds // Use estimated time for planning
                                        };
                                        
                                        total_seconds += seconds;
                                        
                                        if status == "STATE_SUCCESS" {
                                            completed_seconds += seconds;
                                        } else if status == "STATE_RUNNING" {
                                            // Store information about the currently running task
                                            running_task_info = Some((name.clone(), seconds));
                                            running_task_started_at = Some(started_at.clone());
                                        }
                                        
                                        tasks.push(TaskInfo {
                                            name,
                                            status,
                                            started_at,
                                            duration: seconds,
                                            reported_duration: reported_seconds,
                                            estimated_duration: estimated_seconds,
                                        });
                                    }
                                }
                            }
                        }
                    }
                }
                
                // Calculate fluid progress percentage using both task completion and timing data
                let progress = if total_seconds > 0 {
                    // Calculate progress based on completed tasks vs total tasks
                    let total_tasks = tasks.len() as f64;
                    let completed_tasks = tasks.iter().filter(|t| t.status == "STATE_SUCCESS").count() as f64;
                    let task_based_progress = (completed_tasks / total_tasks) * 100.0;

                    // Calculate progress based on time elapsed vs expected total time
                    let mut time_based_progress = completed_seconds as f64 / total_seconds as f64 * 100.0;
                    
                    // If there's a running task, add its partial progress
                    if let (Some((_task_name, expected_duration)), Some(started_at_str)) = (&running_task_info, &running_task_started_at) {
                        if !started_at_str.is_empty() {
                            if let Ok(started_at) = chrono::DateTime::parse_from_rfc3339(started_at_str) {
                                let now = chrono::Utc::now();
                                let elapsed = now.signed_duration_since(started_at).num_seconds() as f64;
                                
                                // Cap elapsed time at 1.5x expected duration
                                let capped_elapsed = elapsed.min(*expected_duration as f64 * 1.5);
                                
                                // Calculate partial progress for current task
                                let task_progress_ratio = if *expected_duration > 0 {
                                    capped_elapsed / *expected_duration as f64
                                } else {
                                    0.0
                                };
                                
                                // Weight of this task in the overall time
                                let task_weight = *expected_duration as f64 / total_seconds as f64;
                                
                                // Add partial progress from running task
                                time_based_progress += task_weight * task_progress_ratio * 100.0;
                            }
                        }
                    }

                    // Combine both progress calculations with weights
                    // We weight time-based progress more heavily (0.7) since it's more accurate
                    let combined_progress = (time_based_progress * 0.7 + task_based_progress * 0.3)
                        .min(100.0) // Ensure we don't exceed 100%
                        .max(0.0);  // Ensure we don't go below 0%

                    combined_progress as u8
                } else {
                    0
                };
                
                // Calculate estimated completion time using template-specific timing data
                let estimated_completion = if state != "STATE_SUCCESS" && state != "STATE_FAILED" && !tasks.is_empty() {
                    if let (Some((_task_name, expected_duration)), Some(started_at_str)) = (&running_task_info, &running_task_started_at) {
                        if !started_at_str.is_empty() {
                            // Parse the started_at time
                            if let Ok(started_at) = chrono::DateTime::parse_from_rfc3339(started_at_str) {
                                let now = chrono::Utc::now();
                                let elapsed = now.signed_duration_since(started_at).num_seconds() as i64;
                                
                                // Calculate remaining time for current task
                                let remaining_seconds = *expected_duration as i64 - elapsed;
                                let remaining_seconds = remaining_seconds.max(0); // Ensure non-negative
                                
                                // If we're near completion of this task, look ahead to how much time is left overall
                                if remaining_seconds < 10 {
                                    // Sum the durations of all remaining tasks
                                    let mut remaining_total = remaining_seconds;
                                    let mut found_current = false;
                                    
                                    for task in &tasks {
                                        if found_current {
                                            // This is a future task
                                            remaining_total += task.duration as i64;
                                        } else if task.name == *_task_name && task.status == "STATE_RUNNING" {
                                            // This is the current task, we've found it
                                            found_current = true;
                                        }
                                    }
                                    
                                    format_remaining_time(remaining_total)
                                } else {
                                    // Just focus on current task
                                    format_remaining_time(remaining_seconds)
                                }
                            } else {
                                None
                            }
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                } else {
                    None
                };
                
                // If the workflow completed successfully, store the timing information with template reference
                if state == "STATE_SUCCESS" && tasks.iter().all(|t| t.status == "STATE_SUCCESS") {
                    store_timing_info(template_ref, &tasks);
                }
                
                // If the workflow failed, update the machine status to Error
                if state == "STATE_FAILED" {
                    if let Err(e) = update_machine_status_on_failure(machine).await {
                        warn!("Failed to update machine status after workflow failure: {}", e);
                    }
                }
                
                // Only mark as Ready if ALL tasks are complete successfully
                if state == "STATE_SUCCESS" && tasks.iter().all(|t| t.status == "STATE_SUCCESS") {
                    if let Err(e) = update_machine_status_on_success(machine).await {
                        warn!("Failed to update machine status after workflow success: {}", e);
                    }
                }
                
                let workflow_info = WorkflowInfo {
                    state: state.to_string(),
                    current_action,
                    progress,
                    tasks,
                    estimated_completion,
                    template_name: template_ref.to_string(),
                };
                
                Ok(Some(workflow_info))
            } else {
                info!("No status information found for workflow {}", workflow_name);
                Ok(None)
            }
        },
        Err(KubeError::Api(ae)) if ae.code == 404 => {
            info!("No workflow found with name: {}", workflow_name);
            Ok(None)
        },
        Err(e) => {
            error!("Error fetching workflow {}: {}", workflow_name, e);
            Err(anyhow!("Error fetching workflow: {}", e))
        }
    }
}

// Update machine status when workflow fails
async fn update_machine_status_on_failure(machine: &Machine) -> Result<()> {
    use dragonfly_common::models::MachineStatus;
    
    info!("Workflow failed for machine {}, updating status to Error", machine.id);
    
    let mut updated_machine = machine.clone();
    updated_machine.status = MachineStatus::Error("OS installation failed".to_string());
    
    crate::db::update_machine(&updated_machine).await?;
    Ok(())
}

// Update machine status when workflow succeeds
async fn update_machine_status_on_success(machine: &Machine) -> Result<()> {
    use dragonfly_common::models::MachineStatus;
    
    info!("Workflow completed successfully for machine {}, updating status to Ready", machine.id);
    
    let mut updated_machine = machine.clone();
    updated_machine.status = MachineStatus::Ready;
    
    crate::db::update_machine(&updated_machine).await?;
    Ok(())
}

// Helper function to format remaining time in a human-readable way
fn format_remaining_time(seconds: i64) -> Option<String> {
    if seconds <= 0 {
        return Some("Completing soon".to_string());
    }
    
    if seconds < 60 {
        return Some(format!("Less than a minute remaining"));
    }
    
    let minutes = seconds / 60;
    if minutes < 60 {
        return Some(format!("Approximately {} minute{} remaining", 
            minutes, if minutes == 1 { "" } else { "s" }));
    }
    
    let hours = minutes / 60;
    let remaining_minutes = minutes % 60;
    
    if remaining_minutes == 0 {
        Some(format!("Approximately {} hour{} remaining",
            hours, if hours == 1 { "" } else { "s" }))
    } else {
        Some(format!("Approximately {} hour{} and {} minute{} remaining",
            hours, if hours == 1 { "" } else { "s" },
            remaining_minutes, if remaining_minutes == 1 { "" } else { "s" }))
    }
}

// Helper function to check if a workflow has timed out
fn is_workflow_timed_out(status: &serde_json::Value, current_action: Option<&str>) -> bool {
    // First, check the state - we only consider timing out workflows that are in STATE_RUNNING
    let state = status.get("state").and_then(|s| s.as_str()).unwrap_or("UNKNOWN");
    if state != "STATE_RUNNING" {
        return false;
    }
    
    // Check if the current action is kexec to boot OS
    if current_action != Some("kexec to boot OS") {
        return false;
    }
    
    // Try to get the time for the last action
    if let Some(tasks) = status.get("tasks") {
        if let Some(tasks_array) = tasks.as_array() {
            for task_obj in tasks_array {
                if let Some(actions) = task_obj.get("actions") {
                    if let Some(actions_array) = actions.as_array() {
                        if let Some(last_action) = actions_array.last() {
                            if last_action.get("name").and_then(|n| n.as_str()) == Some("kexec to boot OS") {
                                if let Some(started_at_str) = last_action.get("startedAt").and_then(|s| s.as_str()) {
                                    // Try to parse the started_at time
                                    if let Ok(started_at) = chrono::DateTime::parse_from_rfc3339(started_at_str) {
                                        let now = chrono::Utc::now();
                                        let elapsed = now.signed_duration_since(started_at);
                                        
                                        // If it's been more than 30 minutes, consider it timed out
                                        if elapsed.num_minutes() > 30 {
                                            return true;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    
    false
}

// Clean up historical timing data to maintain only MAX_TIMING_HISTORY entries per template/action
pub async fn cleanup_historical_timings() -> anyhow::Result<()> {
    // Get write lock on timings and collect data to save
    let to_save = {
        let mut timings = HISTORICAL_TIMINGS.write().map_err(|_| anyhow::anyhow!("Failed to acquire write lock"))?;
        let mut data = Vec::new();
        
        // Clone the data we need
        for (template_name, actions) in timings.iter() {
            for (action_name, durations) in actions.iter() {
                data.push((
                    template_name.clone(),
                    action_name.clone(),
                    durations.clone()
                ));
            }
        }
        
        // Clear the in-memory timings
        timings.clear();
        data
    }; // Lock is dropped here
    
    // Save each timing to the database
    for (template_name, action_name, durations) in to_save {
        if let Err(e) = crate::db::save_template_timing(&template_name, &action_name, &durations).await {
            error!("Failed to save timing data: {}", e);
        }
    }
    
    Ok(())
}

// Periodically clean up historical timing data
pub async fn start_timing_cleanup_task() {
    tokio::spawn(async move {
        // Run the cleanup task every 24 hours
        let cleanup_interval = std::time::Duration::from_secs(24 * 60 * 60);
        
        loop {
            tokio::time::sleep(cleanup_interval).await;
            
            info!("Running timing cleanup task");
            if let Err(e) = cleanup_historical_timings().await {
                error!("Error during timing cleanup: {}", e);
            }
        }
    });
} 