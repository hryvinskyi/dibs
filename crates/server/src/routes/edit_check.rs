//! `POST /v1/edit-check`: for each path the caller is about to change, may they.

use axum::Json;
use axum::extract::State;
use axum::extract::rejection::JsonRejection;
use axum::http::StatusCode;
use axum::response::Response;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use dibs_app::use_cases::{EditCheckInput, FromCheckout, OnBranch};
use dibs_domain::paths::{RelPath, RepoPath};
use dibs_domain::radar::{BranchName, WorktreeRef};
use dibs_domain::repo::RepoId;
use dibs_domain::report::MachineId;
use dibs_domain::session::CheckoutToken;
use dibs_domain::worktree::{PoolId, WorktreeName};

use crate::auth::Authed;
use crate::error::ApiError;
use crate::fields::{parse, parse_required, required, session_in_body};
use crate::http::AppState;
use crate::views::{PathVerdictView, reply};

/// The body of `POST /v1/edit-check`.
#[derive(Deserialize)]
pub struct CheckBody {
    session_id: Option<u64>,
    repo: Option<String>,
    paths: Option<Vec<String>>,
    reserve: Option<bool>,
    checkout: Option<String>,
    branch: Option<String>,
    worktree: Option<Value>,
}

#[derive(Serialize)]
struct CheckAnswer {
    verdicts: Vec<PathVerdictView>,
}

/// Answers a verdict per path, in the order asked. Every path lies in the one repository `repo`
/// names (its id in the project's settings; a project that lists no repositories has one, named by
/// the id `/v1/me` reports) and is relative to that repository's root, `/`-separated. With
/// `reserve: true` the edits are being made and what the check plans is recorded; without it
/// (the default) the check writes nothing. `checkout` names the checkout the edits are made in, by
/// the 32 lowercase hexadecimal characters its client makes of it, and `branch` the branch it is
/// on; a checkout on a detached head sends no branch. A check that reserves and names its checkout
/// stores that the session works there, and records the branch on the session's one working claim
/// when the edits were attributed to it and no other checkout of the session and no other session
/// in this checkout stand in the way; one that names none records no branch. `worktree`,
/// `{machine, pool, name}`, names the worktree of the caller's pool the edits are made in: a check
/// that reserves, when the hold is held under the check's session and not silent and the edits
/// were attributed to exactly one claim, which that session took or last renewed and which holds
/// an edited area, names it wide or was granted edits in it — not one the edits went to only as
/// the caller's newest claim — gives a hold whose take named no item that claim's item, replacing
/// one it learned before, and records
/// `branch` on the caller's live claim on the item the hold names, learned from that worktree. A
/// `branch` is read only with its `checkout`: a check naming a worktree and no checkout may teach
/// the hold its item, and records no branch.
///
/// # Errors
/// `forbidden` for an integration;
/// `invalid_request` for a missing repository, session or path list, or an invalid path, checkout,
/// branch or worktree;
/// `unknown_repo` for a repository the project does not list; `not_found` for a session not the
/// caller's.
pub async fn check(
    State(state): State<AppState>,
    Authed(actor): Authed,
    body: Result<Json<CheckBody>, JsonRejection>,
) -> Result<Response, ApiError> {
    actor.require_person()?;
    let Json(body) = body?;
    let session = session_in_body("session_id", body.session_id)?;
    let repo = parse_required("repo", body.repo.as_deref(), RepoId::parse)?;
    let paths = required("paths", body.paths)?
        .iter()
        .map(|text| {
            parse("paths", text, RelPath::parse).map(|path| RepoPath {
                repo: repo.clone(),
                path,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let branch = body
        .branch
        .as_deref()
        .map(|text| parse("branch", text, BranchName::parse))
        .transpose()?
        .map(|branch| OnBranch {
            repo: repo.clone(),
            branch,
        });
    let checkout = body
        .checkout
        .as_deref()
        .map(|text| parse("checkout", text, CheckoutToken::parse))
        .transpose()?
        .map(|token| FromCheckout { token, branch });
    let input = EditCheckInput {
        session,
        paths,
        reserve: body.reserve.unwrap_or(false),
        checkout,
        worktree: worktree_in_body(body.worktree)?,
    };
    let check = state.app.edit_check.execute(&actor, input).await?;
    let answer = CheckAnswer {
        verdicts: check
            .verdicts
            .into_iter()
            .map(PathVerdictView::of)
            .collect(),
    };
    Ok(reply(StatusCode::OK, &answer))
}

/// The pool worktree a check's body names: an object of the machine and the pool its client
/// names the pool by, as a take names them, and the worktree's name.
fn worktree_in_body(worktree: Option<Value>) -> Result<Option<WorktreeRef>, ApiError> {
    let fields = match worktree {
        None | Some(Value::Null) => return Ok(None),
        Some(Value::Object(fields)) => fields,
        Some(_) => return Err(ApiError::invalid_field("worktree", &NOT_WORKTREE)),
    };
    let text = |name: &str| match fields.get(name) {
        Some(Value::String(text)) => Ok(text.as_str()),
        _ => Err(ApiError::invalid_field("worktree", &NOT_WORKTREE)),
    };
    Ok(Some(WorktreeRef {
        machine: parse("worktree", text("machine")?, MachineId::parse)?,
        pool: parse("worktree", text("pool")?, PoolId::parse)?,
        worktree: parse("worktree", text("name")?, WorktreeName::parse)?,
    }))
}

/// What `worktree` must be.
const NOT_WORKTREE: &str = "it is an object of the machine, the pool and the worktree's name";
