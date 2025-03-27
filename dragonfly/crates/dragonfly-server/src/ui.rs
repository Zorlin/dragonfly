use axum::{
    routing::get,
    Router,
};
use askama::Template;
use askama_axum::IntoResponse;
use dragonfly_common::*;
use dragonfly_common::models::MachineStatus;
use tracing::{error, info};
use std::collections::HashMap;
use serde_json;
use uuid;

use crate::db;

// Filters must be at a specific path where Askama can find them
mod filters {
    use askama::Result;

    pub fn length<T>(collection: &[T]) -> Result<usize> {
        Ok(collection.len())
    }
    
    pub fn string<T: std::fmt::Display>(value: T) -> Result<String> {
        Ok(format!("{}", value))
    }
}

#[derive(Template)]
#[template(path = "index.html")]
pub struct IndexTemplate {
    pub title: String,
    pub machines: Vec<Machine>,
    pub status_counts: HashMap<String, usize>,
    pub status_counts_json: String,
}

#[derive(Template)]
#[template(path = "machine_list.html")]
pub struct MachineListTemplate {
    pub machines: Vec<Machine>,
}

#[derive(Template)]
#[template(path = "machine_details.html", escape = "html")]
pub struct MachineDetailsTemplate {
    pub machine: Machine,
}

enum UiTemplate {
    Index(IndexTemplate),
    MachineList(MachineListTemplate),
    MachineDetails(MachineDetailsTemplate),
}

impl IntoResponse for UiTemplate {
    fn into_response(self) -> axum::response::Response {
        match self {
            UiTemplate::Index(template) => template.into_response(),
            UiTemplate::MachineList(template) => template.into_response(),
            UiTemplate::MachineDetails(template) => template.into_response(),
        }
    }
}

pub fn ui_router() -> Router {
    Router::new()
        .route("/", get(index))
        .route("/machines", get(machine_list))
        .route("/machines/:id", get(machine_details))
}

// Count machines by status and return a HashMap
fn count_machines_by_status(machines: &[Machine]) -> HashMap<String, usize> {
    let mut counts = HashMap::new();
    
    // Initialize counts for all statuses to ensure they're present in the chart
    counts.insert("Existing OS".to_string(), 0);
    counts.insert("Ready for Adoption".to_string(), 0);
    counts.insert("Installing OS".to_string(), 0);
    counts.insert("Ready".to_string(), 0);
    counts.insert("Offline".to_string(), 0);
    counts.insert("Error".to_string(), 0);
    
    // Count actual statuses
    for machine in machines {
        let status_key = match &machine.status {
            MachineStatus::ExistingOS(_) => "Existing OS",
            MachineStatus::ReadyForAdoption => "Ready for Adoption",
            MachineStatus::InstallingOS => "Installing OS",
            MachineStatus::Ready => "Ready",
            MachineStatus::Offline => "Offline",
            MachineStatus::Error(_) => "Error",
        };
        
        *counts.get_mut(status_key).unwrap() += 1;
    }
    
    counts
}

pub async fn index() -> impl IntoResponse {
    match db::get_all_machines().await {
        Ok(machines) => {
            info!("Rendering index page with {} machines", machines.len());
            
            // Count machines by status
            let status_counts = count_machines_by_status(&machines);
            
            // Convert status counts to JSON for the chart
            let status_counts_json = serde_json::to_string(&status_counts)
                .unwrap_or_else(|_| "{}".to_string());
            
            UiTemplate::Index(IndexTemplate {
                title: "Dragonfly".to_string(),
                machines,
                status_counts,
                status_counts_json,
            })
        },
        Err(e) => {
            error!("Error fetching machines for index page: {}", e);
            UiTemplate::Index(IndexTemplate {
                title: "Dragonfly".to_string(),
                machines: vec![],
                status_counts: HashMap::new(),
                status_counts_json: "{}".to_string(),
            })
        }
    }
}

pub async fn machine_list() -> impl IntoResponse {
    match db::get_all_machines().await {
        Ok(machines) => {
            info!("Rendering machine list page with {} machines", machines.len());
            UiTemplate::MachineList(MachineListTemplate { machines })
        },
        Err(e) => {
            error!("Error fetching machines for machine list page: {}", e);
            UiTemplate::MachineList(MachineListTemplate { machines: vec![] })
        }
    }
}

pub async fn machine_details(axum::extract::Path(id): axum::extract::Path<String>) -> impl IntoResponse {
    // Parse UUID from string
    match uuid::Uuid::parse_str(&id) {
        Ok(uuid) => {
            // Get machine by ID
            match db::get_machine_by_id(&uuid).await {
                Ok(Some(machine)) => {
                    info!("Rendering machine details page for machine {}", uuid);
                    UiTemplate::MachineDetails(MachineDetailsTemplate { machine })
                },
                Ok(None) => {
                    error!("Machine not found: {}", uuid);
                    // Return to index page with error
                    UiTemplate::Index(IndexTemplate {
                        title: "Dragonfly - Machine Not Found".to_string(),
                        machines: vec![],
                        status_counts: HashMap::new(),
                        status_counts_json: "{}".to_string(),
                    })
                },
                Err(e) => {
                    error!("Error fetching machine {}: {}", uuid, e);
                    // Return to index page with error
                    UiTemplate::Index(IndexTemplate {
                        title: "Dragonfly - Error".to_string(),
                        machines: vec![],
                        status_counts: HashMap::new(),
                        status_counts_json: "{}".to_string(),
                    })
                }
            }
        },
        Err(e) => {
            error!("Invalid UUID: {}", e);
            // Return to index page with error
            UiTemplate::Index(IndexTemplate {
                title: "Dragonfly - Invalid UUID".to_string(),
                machines: vec![],
                status_counts: HashMap::new(),
                status_counts_json: "{}".to_string(),
            })
        }
    }
} 