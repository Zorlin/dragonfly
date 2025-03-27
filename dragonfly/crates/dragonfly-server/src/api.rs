use axum::{
    extract::{Json, Path},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{post, get},
    Router,
};
use uuid::Uuid;
use dragonfly_common::*;
use tracing::{error, info};

use crate::db;

pub fn api_router() -> Router {
    Router::new()
        .route("/api/machines", post(register_machine))
        .route("/api/machines", get(get_all_machines))
        .route("/api/machines/:id", get(get_machine))
        .route("/api/machines/:id/os", post(assign_os))
        .route("/api/machines/:id/status", post(update_status))
}

async fn register_machine(
    Json(payload): Json<RegisterRequest>,
) -> Response {
    info!("Registering machine with MAC: {}", payload.mac_address);
    
    match db::register_machine(&payload).await {
        Ok(machine_id) => {
            let response = RegisterResponse {
                machine_id,
                next_step: "awaiting_os_assignment".to_string(),
            };
            (StatusCode::CREATED, Json(response)).into_response()
        },
        Err(e) => {
            error!("Failed to register machine: {}", e);
            let error_response = ErrorResponse {
                error: "Registration Failed".to_string(),
                message: e.to_string(),
            };
            (StatusCode::INTERNAL_SERVER_ERROR, Json(error_response)).into_response()
        }
    }
}

async fn get_all_machines() -> Response {
    match db::get_all_machines().await {
        Ok(machines) => {
            (StatusCode::OK, Json(machines)).into_response()
        },
        Err(e) => {
            error!("Failed to retrieve machines: {}", e);
            let error_response = ErrorResponse {
                error: "Database Error".to_string(),
                message: e.to_string(),
            };
            (StatusCode::INTERNAL_SERVER_ERROR, Json(error_response)).into_response()
        }
    }
}

async fn get_machine(
    Path(id): Path<Uuid>,
) -> Response {
    match db::get_machine_by_id(&id).await {
        Ok(Some(machine)) => {
            (StatusCode::OK, Json(machine)).into_response()
        },
        Ok(None) => {
            let error_response = ErrorResponse {
                error: "Not Found".to_string(),
                message: format!("Machine with ID {} not found", id),
            };
            (StatusCode::NOT_FOUND, Json(error_response)).into_response()
        },
        Err(e) => {
            error!("Failed to retrieve machine {}: {}", id, e);
            let error_response = ErrorResponse {
                error: "Database Error".to_string(),
                message: e.to_string(),
            };
            (StatusCode::INTERNAL_SERVER_ERROR, Json(error_response)).into_response()
        }
    }
}

async fn assign_os(
    Path(id): Path<Uuid>,
    Json(payload): Json<OsAssignmentRequest>,
) -> Response {
    info!("Assigning OS {} to machine {}", payload.os_choice, id);
    
    match db::assign_os(&id, &payload.os_choice).await {
        Ok(true) => {
            let response = OsAssignmentResponse {
                success: true,
                message: format!("OS {} assigned to machine {}", payload.os_choice, id),
            };
            (StatusCode::OK, Json(response)).into_response()
        },
        Ok(false) => {
            let error_response = ErrorResponse {
                error: "Not Found".to_string(),
                message: format!("Machine with ID {} not found", id),
            };
            (StatusCode::NOT_FOUND, Json(error_response)).into_response()
        },
        Err(e) => {
            error!("Failed to assign OS to machine {}: {}", id, e);
            let error_response = ErrorResponse {
                error: "Database Error".to_string(),
                message: e.to_string(),
            };
            (StatusCode::INTERNAL_SERVER_ERROR, Json(error_response)).into_response()
        }
    }
}

async fn update_status(
    Path(id): Path<Uuid>,
    Json(payload): Json<StatusUpdateRequest>,
) -> Response {
    info!("Updating status for machine {} to {:?}", id, payload.status);
    
    match db::update_status(&id, payload.status).await {
        Ok(true) => {
            let response = StatusUpdateResponse {
                success: true,
                message: format!("Status updated for machine {}", id),
            };
            (StatusCode::OK, Json(response)).into_response()
        },
        Ok(false) => {
            let error_response = ErrorResponse {
                error: "Not Found".to_string(),
                message: format!("Machine with ID {} not found", id),
            };
            (StatusCode::NOT_FOUND, Json(error_response)).into_response()
        },
        Err(e) => {
            error!("Failed to update status for machine {}: {}", id, e);
            let error_response = ErrorResponse {
                error: "Database Error".to_string(),
                message: e.to_string(),
            };
            (StatusCode::INTERNAL_SERVER_ERROR, Json(error_response)).into_response()
        }
    }
}

// Error handling
pub async fn handle_error(err: anyhow::Error) -> Response {
    error!("Internal server error: {}", err);
    let error_response = ErrorResponse {
        error: "Internal Server Error".to_string(),
        message: err.to_string(),
    };

    (StatusCode::INTERNAL_SERVER_ERROR, Json(error_response)).into_response()
} 