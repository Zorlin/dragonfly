use serde::{Deserialize, Serialize};
use chrono::{DateTime, Utc};
use uuid::Uuid;
use std::fmt;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Machine {
    pub id: Uuid,
    pub mac_address: String,
    pub ip_address: String,
    pub hostname: Option<String>,
    pub os_choice: Option<String>,
    pub status: MachineStatus,
    pub disks: Vec<DiskInfo>,
    pub nameservers: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memorable_name: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub enum MachineStatus {
    ExistingOS(String),    // Foreign existing OS (with OS name)
    ReadyForAdoption,      // Blank machine ready to be adopted
    InstallingOS,          // Installing an OS via tinkerbell
    Ready,                 // Part of the cluster, serving K8s workloads
    Offline,               // Machine is offline (can be WoL'd)
    Error(String),         // Error state with message
}

impl fmt::Display for MachineStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MachineStatus::ExistingOS(os) => write!(f, "ExistingOS: {}", os),
            MachineStatus::ReadyForAdoption => write!(f, "ReadyForAdoption"),
            MachineStatus::InstallingOS => write!(f, "InstallingOS"),
            MachineStatus::Ready => write!(f, "Ready"),
            MachineStatus::Offline => write!(f, "Offline"),
            MachineStatus::Error(msg) => write!(f, "Error: {}", msg),
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RegisterRequest {
    pub mac_address: String,
    pub ip_address: String,
    pub hostname: Option<String>,
    pub disks: Vec<DiskInfo>,
    pub nameservers: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct DiskInfo {
    pub device: String,
    pub size_bytes: u64,
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub calculated_size: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RegisterResponse {
    pub machine_id: Uuid,
    pub next_step: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct OsAssignmentRequest {
    pub os_choice: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct OsAssignmentResponse {
    pub success: bool,
    pub message: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct StatusUpdateRequest {
    pub status: MachineStatus,
    pub message: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct StatusUpdateResponse {
    pub success: bool,
    pub message: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ErrorResponse {
    pub error: String,
    pub message: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct HostnameUpdateRequest {
    pub hostname: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct HostnameUpdateResponse {
    pub success: bool,
    pub message: String,
} 