//! The handlers. Each one reads the request, calls one use case and writes its answer; none decides
//! anything the use cases decide, and none opens a transaction of its own: what an answer shows,
//! the handles of the members it names included, comes from the use case.

pub mod admin;
pub mod claims;
pub mod dependencies;
pub mod edit_check;
pub mod escalations;
pub mod events;
pub mod grants;
pub mod items;
pub mod me;
pub mod members;
pub mod messages;
pub mod radar;
pub mod ready;
pub mod reports;
pub mod review;
pub mod sessions;
pub mod slots;
pub mod status;
pub mod threads;
pub mod version;

use axum::Json;
use serde_json::{Value, json};

/// `GET /health`: the process answers. It reads nothing and needs no token.
pub async fn health() -> Json<Value> {
    Json(json!({ "ok": true }))
}
