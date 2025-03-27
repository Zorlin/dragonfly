use axum::{
    routing::get,
    Router,
};
use askama::Template;
use askama_axum::{Response, IntoResponse};
use dragonfly_common::*;
use tracing::{error, info};

use crate::db;

mod filters {
    use askama::Result;

    pub fn length<T>(collection: &[T]) -> Result<usize> {
        Ok(collection.len())
    }
}

#[derive(Template)]
#[template(path = "index.html")]
pub struct IndexTemplate {
    pub title: String,
    pub machines: Vec<Machine>,
}

#[derive(Template)]
#[template(path = "machine_list.html")]
pub struct MachineListTemplate {
    pub machines: Vec<Machine>,
}

pub fn ui_router() -> Router {
    Router::new()
        .route("/", get(index))
        .route("/machines", get(machine_list))
}

pub async fn index() -> Response {
    match db::get_all_machines().await {
        Ok(machines) => {
            info!("Rendering index page with {} machines", machines.len());
            let template = IndexTemplate {
                title: "Dragonfly".to_string(),
                machines,
            };
            template.into_response()
        },
        Err(e) => {
            error!("Error fetching machines for index page: {}", e);
            let template = IndexTemplate {
                title: "Dragonfly".to_string(),
                machines: vec![],
            };
            template.into_response()
        }
    }
}

pub async fn machine_list() -> Response {
    match db::get_all_machines().await {
        Ok(machines) => {
            info!("Rendering machine list page with {} machines", machines.len());
            let template = MachineListTemplate { machines };
            template.into_response()
        },
        Err(e) => {
            error!("Error fetching machines for machine list page: {}", e);
            let template = MachineListTemplate { machines: vec![] };
            template.into_response()
        }
    }
} 