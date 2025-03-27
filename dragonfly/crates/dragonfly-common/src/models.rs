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
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub enum MachineStatus {
    Registered,
    AwaitingOsAssignment,
    InstallingOs,
    Ready,
    Error(String),
}

impl fmt::Display for MachineStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MachineStatus::Registered => write!(f, "Registered"),
            MachineStatus::AwaitingOsAssignment => write!(f, "AwaitingOsAssignment"),
            MachineStatus::InstallingOs => write!(f, "InstallingOs"),
            MachineStatus::Ready => write!(f, "Ready"),
            MachineStatus::Error(msg) => write!(f, "Error: {}", msg),
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RegisterRequest {
    pub mac_address: String,
    pub ip_address: String,
    pub hostname: Option<String>,
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