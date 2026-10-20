//! `PUT /v1/items/{id}/footprint`: an integration reports what the branch of the item's claim
//! changes against its base; `GET /v1/claims`: the live claims and the branches they work on.

use std::collections::BTreeSet;

use axum::Json;
use axum::extract::rejection::{JsonRejection, PathRejection};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Response;
use serde::{Deserialize, Serialize};

use std::collections::BTreeMap;

use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use dibs_app::named::NamedClaim;
use dibs_app::use_cases::{FootprintInput, FootprintRecorded};
use dibs_domain::handle::Handle;
use dibs_domain::ids::ItemId;
use dibs_domain::paths::RelPath;
use dibs_domain::radar::{BranchName, CommitSha, FootprintBase, FootprintReport};
use dibs_domain::repo::RepoId;

use crate::auth::Authed;
use crate::error::ApiError;
use crate::fields::{item_in_path, parse, parse_required, required};
use crate::http::AppState;
use crate::views::reply;

/// The body of `PUT /v1/items/{id}/footprint`.
#[derive(Deserialize)]
pub struct FootprintBody {
    repo: Option<String>,
    branch: Option<String>,
    base: Option<String>,
    base_sha: Option<String>,
    base_at: Option<String>,
    paths: Option<Vec<String>>,
    truncated: Option<bool>,
}

#[derive(Serialize)]
struct FootprintAnswer {
    /// Whether the footprint is the one stored now.
    stored: bool,
    /// When stored, whether it differs from the one before.
    #[serde(skip_serializing_if = "Option::is_none")]
    changed: Option<bool>,
    /// When not stored, why: `no_live_claim` or `other_branch`.
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<&'static str>,
}

/// An integration reports that the branch `branch` of the item's claim in `repo` (its id, or its
/// remote's path such as `acme/api`) changes `paths` against the base branch `base` (`main`, or
/// `refs/heads/main`), at most 3000 of them; `base_sha` is the merge base of the two and `base_at`
/// (RFC 3339) that commit's committer time. `truncated` (default `false`) says the forge gave
/// fewer paths than the branch changes.
/// It replaces the claim's footprint of that repository, and the overlaps it makes with other
/// members' live claims are announced once to both holders. A report for an item no live claim
/// holds, or for another branch than the one the claim records there, answers `stored: false`; one
/// that repeats the stored footprint answers `changed: false`. Neither changes anything.
///
/// # Errors
/// `forbidden` for anyone but an integration or an admin; `not_found` for a repository the project
/// does not list; `invalid_request` for a missing or malformed field.
pub async fn footprint(
    State(state): State<AppState>,
    Authed(actor): Authed,
    path: Result<Path<String>, PathRejection>,
    body: Result<Json<FootprintBody>, JsonRejection>,
) -> Result<Response, ApiError> {
    actor.require_import()?;
    let Path(id) = path?;
    let item = item_in_path(&id)?;
    let Json(body) = body?;
    let written = required("paths", body.paths)?;
    if written.len() > FootprintReport::MAX_PATHS {
        return Err(ApiError::invalid_field(
            "paths",
            &format!(
                "a footprint lists at most {} paths; send the first ones with truncated: true",
                FootprintReport::MAX_PATHS
            ),
        ));
    }
    let paths = written
        .iter()
        .map(|text| parse("paths", text, RelPath::parse))
        .collect::<Result<BTreeSet<_>, _>>()?;
    let input = FootprintInput {
        item,
        repo: required("repo", body.repo)?,
        branch: parse_required("branch", body.branch.as_deref(), BranchName::parse)?,
        base: FootprintBase {
            branch: parse_required("base", body.base.as_deref(), BranchName::parse_ref)?,
            sha: parse_required("base_sha", body.base_sha.as_deref(), CommitSha::parse)?,
            at: parse_required("base_at", body.base_at.as_deref(), |text| {
                OffsetDateTime::parse(text, &Rfc3339).map_err(|_| "it is a time in RFC 3339")
            })?,
        },
        paths,
        truncated: body.truncated.unwrap_or(false),
    };
    let answer = match state.app.record_footprint.execute(&actor, input).await? {
        FootprintRecorded::Stored { changed } => FootprintAnswer {
            stored: true,
            changed: Some(changed),
            reason: None,
        },
        FootprintRecorded::NotStored(why) => FootprintAnswer {
            stored: false,
            changed: None,
            reason: Some(why.code()),
        },
    };
    Ok(reply(StatusCode::OK, &answer))
}

/// One live claim as `GET /v1/claims` lists it.
#[derive(Serialize)]
struct LiveClaimView {
    item: ItemId,
    holder: Handle,
    state: &'static str,
    branches: BTreeMap<RepoId, BranchName>,
}

impl LiveClaimView {
    fn of(named: NamedClaim) -> Self {
        Self {
            item: named.claim.item,
            holder: named.holder,
            state: named.claim.state.code(),
            branches: named.claim.branches,
        }
    }
}

#[derive(Serialize)]
struct ClaimsAnswer {
    claims: Vec<LiveClaimView>,
}

/// The live claims, ordered by item, each with its holder, `state` (`working` or `in_review`) and
/// the branch its work is on in each repository: what an integration measures footprints of. It
/// names no path and no conversation.
///
/// # Errors
/// The answer for the use case's failure.
pub async fn claims(
    State(state): State<AppState>,
    Authed(actor): Authed,
) -> Result<Response, ApiError> {
    let listed = state.app.list_claims.execute(&actor).await?;
    Ok(reply(
        StatusCode::OK,
        &ClaimsAnswer {
            claims: listed.into_iter().map(LiveClaimView::of).collect(),
        },
    ))
}
