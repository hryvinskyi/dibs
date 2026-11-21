//! `POST /v1/sessions`, `POST /v1/heartbeat` and `POST /v1/sessions/{id}/end`: a session
//! announcing itself, staying alive, and saying it ended.

use std::collections::BTreeSet;

use axum::Json;
use axum::extract::rejection::{JsonRejection, PathRejection};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Response;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use dibs_app::use_cases::{FromCheckout, OnBranch, RegisterSessionInput};
use dibs_domain::area::QualifiedArea;
use dibs_domain::edit::{ReportedEdits, ReportedEditsError};
use dibs_domain::ids::ItemId;
use dibs_domain::message::SessionLabel;
use dibs_domain::radar::BranchName;
use dibs_domain::repo::RepoId;
use dibs_domain::report::ReportDay;
use dibs_domain::session::{AgentLabel, CheckoutToken, HostName};

use crate::auth::Authed;
use crate::error::ApiError;
use crate::fields::{parse, parse_required, required, session_in_body, session_in_path};
use crate::http::AppState;
use crate::routes::reports::heartbeat_reports;
use crate::views::worktrees::OwnHoldView;
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
    checkout: Option<String>,
    repo: Option<String>,
    branch: Option<String>,
    machine: Option<Value>,
    reports: Option<Value>,
    edits: Option<EditsBody>,
}

/// The edits a heartbeat reports.
#[derive(Deserialize)]
pub struct EditsBody {
    areas: Option<Vec<String>>,
    outside: Option<bool>,
}

impl EditsBody {
    /// The edits, each area qualified by its repository.
    ///
    /// # Errors
    /// `invalid_request` naming `edits.areas` for more areas than a heartbeat may report or an
    /// area that is not `repo:area`.
    fn parse(self) -> Result<ReportedEdits, ApiError> {
        let written = self.areas.unwrap_or_default();
        let too_many = ReportedEditsError::TooManyAreas {
            max: ReportedEdits::MAX_AREAS,
        };
        if written.len() > ReportedEdits::MAX_AREAS {
            return Err(ApiError::invalid_field("edits.areas", &too_many));
        }
        let areas = written
            .iter()
            .map(|text| parse("edits.areas", text, QualifiedArea::parse))
            .collect::<Result<BTreeSet<_>, _>>()?;
        ReportedEdits::new(areas, self.outside.unwrap_or(false))
            .map_err(|reason| ApiError::invalid_field("edits.areas", &reason))
    }
}

#[derive(Serialize)]
struct HeartbeatAnswer {
    refreshed: Vec<ItemId>,
    reported: Vec<ReportDay>,
    reports_accepted: Vec<ReportDay>,
}

/// Records a session's sign of life and answers the claims the edits it reports refreshed. A
/// heartbeat keeps the session seen and no claim by itself. `edits` reports what the session's
/// machine let through without asking since the last heartbeat that reported it: `areas`, at most
/// 200, each `repo:area`, the areas of edits made where the member was known to hold the area, and
/// `outside`, whether an edit was made in a path no area governs. Every live claim of the caller
/// that holds a reported area is seen now, and an edit outside every area makes the caller's target
/// claim (the live claim most recently claimed or renewed) seen now; `refreshed` names those claims.
/// A heartbeat without `edits` comes from a client that does not report its edits: it refreshes no
/// claim, and `refreshed` is empty. `checkout` names the
/// checkout the session works in, by the 32 lowercase hexadecimal characters its client makes of
/// it, and `repo` and `branch` the repository it is of and the branch it is on; a checkout on a
/// detached head sends no branch, and a branch needs its repository. A heartbeat that names its
/// checkout stores that the session works there, and records the branch on the session's one
/// working claim when no other checkout of the session and no other session in this checkout
/// stand in the way; one that names none records no branch. `reports` carries what the session's
/// machine counted, one entry per day, and `machine` the id its dibs gave that machine: each day
/// is kept for the caller and the machine, replacing what was kept for that day. `reported` names
/// the days whose report changed it, and `reports_accepted` every day whose report was read and is
/// kept, so the client sends the others again; a report that cannot be read is left out and never
/// fails the heartbeat.
///
/// # Errors
/// `forbidden` for an integration;
/// `invalid_request` without a session, for a malformed checkout, repository or branch, a branch
/// without its repository, or reported edits naming more than 200 areas or a malformed one; the
/// answer for the use case's failure.
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
    let from = body
        .checkout
        .as_deref()
        .map(|text| parse("checkout", text, CheckoutToken::parse))
        .transpose()?
        .map(|token| FromCheckout { token, branch });
    let edits = body.edits.map(EditsBody::parse).transpose()?;
    let reports = heartbeat_reports(actor.member, body.machine.as_ref(), body.reports.as_ref());
    let outcome = state
        .app
        .heartbeat
        .execute_reporting(
            &actor,
            session,
            from.as_ref(),
            reports.as_ref(),
            edits.as_ref(),
        )
        .await?;
    let answer = HeartbeatAnswer {
        refreshed: outcome.refreshed,
        reported: outcome.reported,
        reports_accepted: outcome.accepted,
    };
    Ok(reply(StatusCode::OK, &answer))
}

#[derive(Serialize)]
struct EndedAnswer {
    /// The worktree holds that ended with the session, as they were.
    released: Vec<OwnHoldView>,
}

/// The caller's session ended: every worktree hold it, or one of its agents, has in any of the
/// caller's pools ends with it, and the checkouts it reported working in are forgotten. The
/// session itself stays registered, and a session that holds nothing is answered with nothing
/// released.
///
/// # Errors
/// `forbidden` for an integration; `not_found` for a session not the caller's.
pub async fn end(
    State(state): State<AppState>,
    Authed(actor): Authed,
    path: Result<Path<String>, PathRejection>,
) -> Result<Response, ApiError> {
    actor.require_person()?;
    let Path(text) = path?;
    let session = session_in_path(&text)?;
    let ended = state.app.end_session.execute(&actor, session).await?;
    Ok(reply(
        StatusCode::OK,
        &EndedAnswer {
            released: ended.into_iter().map(OwnHoldView::of).collect(),
        },
    ))
}
