use color_eyre::eyre::{Result, eyre};
use color_eyre::eyre::WrapErr;
use kube::{Client, Api};
use k8s_openapi::api::apps::v1::StatefulSet;
use k8s_openapi::api::core::v1::{Service, Namespace};
use tracing::{debug, warn, info};

const DRAGONFLY_NAMESPACE: &str = "tink";
const DRAGONFLY_STATEFULSET: &str = "dragonfly";
const WEBUI_NAMESPACE: &str = "tink";
const WEBUI_SERVICE: &str = "tink-stack";
const WEBUI_EXTERNAL_PORT: i32 = 3000;

/// Checks if the Kubernetes API server is reachable by attempting to list namespaces.
pub async fn check_kubernetes_connectivity() -> Result<()> {
    debug!("Attempting to connect to Kubernetes API server and list namespaces...");
    let client = Client::try_default().await.wrap_err("Failed to create Kubernetes client. Is k3s running and KUBECONFIG configured?")?;
    
    // Attempt a basic API call to confirm connectivity
    let namespaces: Api<Namespace> = Api::all(client);
    let _ = namespaces.list(&Default::default()).await.wrap_err("Failed to list namespaces. Cluster might be unreachable or unresponsive.")?;

    debug!("Successfully connected to Kubernetes API server and listed namespaces.");
    Ok(())
}

/// Checks the status of the Dragonfly StatefulSet.
/// Returns Ok(true) if ready, Ok(false) if not ready, Err if API call fails.
pub async fn check_dragonfly_statefulset_status() -> Result<bool> {
    debug!("Checking status of Dragonfly StatefulSet...");
    let client = Client::try_default().await.wrap_err("Failed to create Kubernetes client")?;
    
    let statefulsets: Api<StatefulSet> = Api::namespaced(client, DRAGONFLY_NAMESPACE);

    match statefulsets.get(DRAGONFLY_STATEFULSET).await {
        Ok(sts) => {
            let spec_replicas = sts.spec.as_ref().and_then(|s| s.replicas).unwrap_or(1); // Default to 1 if not specified
            let status_replicas = sts.status.as_ref().map(|s| s.replicas).unwrap_or(0);
            let ready_replicas = sts.status.as_ref().and_then(|s| s.ready_replicas).unwrap_or(0);

            debug!(
                "StatefulSet '{}': Spec Replicas={}, Status Replicas={}, Ready Replicas={}",
                DRAGONFLY_STATEFULSET, spec_replicas, status_replicas, ready_replicas
            );

            if ready_replicas >= spec_replicas && status_replicas >= spec_replicas {
                debug!("Dragonfly StatefulSet is ready.");
                Ok(true)
            } else {
                debug!("Dragonfly StatefulSet is not ready (Ready: {}/{}, Status: {}).", ready_replicas, spec_replicas, status_replicas);
                Ok(false)
            }
        }
        Err(kube::Error::Api(ae)) if ae.code == 404 => {
            warn!("Dragonfly StatefulSet '{}' not found in namespace '{}'.", DRAGONFLY_STATEFULSET, DRAGONFLY_NAMESPACE);
            Ok(false) // Not found means not ready
        }
        Err(e) => {
            Err(e).wrap_err_with(|| format!("Failed to get StatefulSet '{}' in namespace '{}'", DRAGONFLY_STATEFULSET, DRAGONFLY_NAMESPACE))
        }
    }
}

/// Attempts to determine the WebUI access address by inspecting the Kubernetes Service.
pub async fn get_webui_address() -> Result<Option<String>> {
    debug!("Attempting to determine WebUI address from Service '{}/{}'...", WEBUI_NAMESPACE, WEBUI_SERVICE);
    let client = Client::try_default().await.wrap_err("Failed to create Kubernetes client")?;
    
    let services: Api<Service> = Api::namespaced(client, WEBUI_NAMESPACE);
    let service_name = WEBUI_SERVICE;

    match services.get(service_name).await {
        Ok(service) => {
            let spec = service.spec.ok_or_else(|| eyre!("Service '{}' has no spec", service_name))?;
            let status = service.status.ok_or_else(|| eyre!("Service '{}' has no status", service_name))?;

            let ports = spec.ports.unwrap_or_default();
            // Find the specific external port we are looking for (e.g., 3000 for tink-stack LB)
            let service_port_info = ports.iter().find(|p| p.port == WEBUI_EXTERNAL_PORT);

            if service_port_info.is_none() {
                warn!("Could not find external port {} configured for service '{}'", WEBUI_EXTERNAL_PORT, service_name);
                return Ok(None); // Cannot construct URL without the correct port mapping
            }
            // We'll use WEBUI_EXTERNAL_PORT for the final URL construction
            let external_port = WEBUI_EXTERNAL_PORT;

            // Check service type and status
            match spec.type_.as_deref() {
                Some("LoadBalancer") => {
                    if let Some(lb_status) = status.load_balancer {
                        if let Some(ingress) = lb_status.ingress {
                            if let Some(ingress_point) = ingress.first() {
                                let address = ingress_point.ip.as_deref()
                                    .or(ingress_point.hostname.as_deref());
                                
                                if let Some(addr) = address {
                                    // Use the external port defined for the LB service
                                    let url = format!("http://{}:{}", addr, external_port);
                                    info!("Determined WebUI address from LoadBalancer: {}", url);
                                    return Ok(Some(url));
                                } else {
                                     debug!("LoadBalancer ingress exists but has no IP or hostname yet.");
                                }
                            } else {
                                debug!("LoadBalancer status has no ingress points defined.");
                            }
                        } else {
                           debug!("LoadBalancer status is missing ingress information.");
                        }
                    }
                     warn!("Service '{}' is LoadBalancer type, but address is not yet available.", service_name);
                    Ok(None) // LoadBalancer IP not ready yet
                }
                Some("NodePort") => {
                     // If the target service was NodePort, find the node port corresponding to our external port
                     if let Some(np) = service_port_info.and_then(|p| p.node_port) {
                        // Cannot easily get Node IP here, so suggest localhost
                        let url = format!("http://localhost:{}", np);
                         info!("Determined WebUI address from NodePort: {} (using localhost as node IP)", url);
                        Ok(Some(url))
                    } else {
                        warn!("Service '{}' is NodePort type, but couldn't find nodePort for external port {}", service_name, external_port);
                        Ok(None)
                    }
                }
                Some("ClusterIP") | None => {
                    // ClusterIP is not directly useful for external access in this context
                    warn!("WebUI Service '{}' is ClusterIP type, cannot determine external address.", service_name);
                    Ok(None) 
                }
                Some(other) => {
                    warn!("Service '{}' has unhandled type: {}", service_name, other);
                    Ok(None)
                }
            }
        }
        Err(kube::Error::Api(ae)) if ae.code == 404 => {
            warn!("WebUI Service '{}' not found in namespace '{}'.", service_name, WEBUI_NAMESPACE);
            Ok(None) // Service not found
        }
        Err(e) => {
            Err(e).wrap_err_with(|| format!("Failed to get Service '{}' in namespace '{}'", service_name, WEBUI_NAMESPACE))
        }
    }
} 