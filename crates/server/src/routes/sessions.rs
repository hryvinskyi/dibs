//! `POST /v1/sessions` and `POST /v1/heartbeat`: a session announcing itself and staying alive.

use axum::Json;
use axum::extract::State;
use axum::extract::rejection::JsonRejection;
use axum::http::StatusCode;
use axum::response::Response;
use serde::{Deserialize, Serialize};

use dibs_app::use_cases::{OnBranch, RegisterSessionInput};
use dibs_domain::ids::ItemId;
use dibs_domain::message::SessionLabel;
use dibs_domain::radar::BranchName;
use dibs_domain::repo::RepoId;
use dibs_domain::session::{AgentLabel, HostName};

use crate::auth::Authed;
use crate::error::ApiError;
use crate::fields::{parse, parse_required, required, session_in_body};
use crate::http::AppState;
use crate::views::{SessionView, reply};

/// The body of `POST /v1/sessions`.
#[derive(Deserialize)]
pub struct RegisterBody {
    host: Option<String>,
    label: Option<String>,
    agent: Option<String>,
    push: Option<bool>,
}

/// Registers a session of the caller, or updates the one of the same label, which keeps its id.
/// `agent` is a free label the service stores and never interprets; `push` (default `false`) says
/// the session can be woken while idle.
///
/// # Errors
/// `forbidden` for an integration;
/// `invalid_request` naming a missing or invalid field; the answer for the use case's failure.
pub async fn register(
    State(state): State<AppState>,
    Authed(actor): Authed,
    body: Result<Json<RegisterBody>, JsonRejection>,
) -> Result<Response, ApiError> {
    actor.require_person()?;
    let Json(body) = body?;
    let input = RegisterSessionInput {
        host: parse_required("host", body.host.as_deref(), HostName::parse)?,
        label: parse_required("label", body.label.as_deref(), SessionLabel::parse)?,
        agent: parse_required("agent", body.agent.as_deref(), AgentLabel::parse)?,
        push: body.push.unwrap_or(false),
    };
    let session = state.app.register_session.execute(&actor, input).await?;
    Ok(reply(StatusCode::OK, &SessionView::of(session)))
}

/// The body of `POST /v1/heartbeat`.
#[derive(Deserialize)]
pub struct HeartbeatBody {
    session_id: Option<u64>,
    repo: Option<String>,
    branch: Option<String>,
}

#[derive(Serialize)]
struct HeartbeatAnswer {
    refreshed: Vec<ItemId>,
}

/// Records a session's sign of life and answers the claims it kept alive. `repo` and `branch` name
/// the checkout the session works in and the branch it is on, which the claims of this session it
/// kept alive record; a checkout on a detached head sends no branch, and a branch needs its
/// repository.
///
/// # Errors
/// `forbidden` for an integration;
/// `invalid_request` without a session, for a malformed repository or branch, or a branch without
/// its repository; the answer for the use case's failure.
pub async fn heartbeat(
    State(state): State<AppState>,
    Authed(actor): Authed,
    body: Result<Json<HeartbeatBody>, JsonRejection>,
) -> Result<Response, ApiError> {
    actor.require_person()?;
    let Json(body) = body?;
    let session = session_in_body("session_id", body.session_id)?;
    let repo = body
        .repo
        .as_deref()
        .map(|text| parse("repo", text, RepoId::parse))
        .transpose()?;
    let branch = match body.branch.as_deref() {
        Some(text) => Some(OnBranch {
            branch: parse("branch", text, BranchName::parse)?,
            repo: required("repo", repo)?,
        }),
        None => None,
    };
    let outcome = state
        .app
        .heartbeat
        .execute(&actor, session, branch.as_ref())
        .await?;
    let answer = HeartbeatAnswer {
        refreshed: outcome.refreshed,
    };
    Ok(reply(StatusCode::OK, &answer))
}
