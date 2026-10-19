//! The handlers. Each one reads the request, calls one use case and writes its answer; none decides
//! anything the use cases decide, and none opens a transaction of its own: what an answer shows,
//! the handles of the members it names included, comes from the use case.

pub mod claims;
pub mod edit_check;
pub mod grants;
pub mod items;
pub mod me;
pub mod members;
pub mod sessions;
pub mod status;

use axum::Json;
use serde_json::{Value, json};

/// `GET /health`: the process answers. It reads nothing and needs no token.
pub async fn health() -> Json<Value> {
    Json(json!({ "ok": true }))
}
