//! `POST /v1/edit-check`: for each path the caller is about to change, may they.

use axum::Json;
use axum::extract::State;
use axum::extract::rejection::JsonRejection;
use axum::http::StatusCode;
use axum::response::Response;
use serde::{Deserialize, Serialize};

use dibs_app::use_cases::{EditCheckInput, OnBranch};
use dibs_domain::paths::{RelPath, RepoPath};
use dibs_domain::radar::BranchName;
use dibs_domain::repo::RepoId;

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
    branch: Option<String>,
}

#[derive(Serialize)]
struct CheckAnswer {
    verdicts: Vec<PathVerdictView>,
}

/// Answers a verdict per path, in the order asked. Every path lies in the one repository `repo`
/// names (its id in the project's settings; a project that lists no repositories has one, named by
/// the id `/v1/me` reports) and is relative to that repository's root, `/`-separated. With
/// `reserve: true` the edits are being made and what the check plans is recorded; without it
/// (the default) the check writes nothing. `branch` names the branch the checkout is on, which the
/// claims the edits are attributed to record; a checkout on a detached head sends none.
///
/// # Errors
/// `forbidden` for an integration;
/// `invalid_request` for a missing repository, session or path list, or an invalid path;
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
    let input = EditCheckInput {
        session,
        paths,
        reserve: body.reserve.unwrap_or(false),
        branch,
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
