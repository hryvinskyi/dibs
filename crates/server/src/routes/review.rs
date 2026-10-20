//! Review: `POST /v1/items/{id}/review` records a pull request on the caller's claim; an
//! integration reports merges and closes on `POST /v1/prs/merged` and `POST /v1/prs/closed`, and
//! reads `GET /v1/review-claims` and `GET /v1/pr-status`.

use std::collections::{BTreeMap, BTreeSet};

use axum::Json;
use axum::extract::rejection::{JsonRejection, PathRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::Response;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use dibs_app::use_cases::{
    DropPrInput, PrReport, PrReportOutcome, PrStatusQuery, ReportedEnd, ReviewClaim,
    StartReviewInput,
};
use dibs_domain::handle::Handle;
use dibs_domain::ids::ItemId;
use dibs_domain::paths::RelPath;
use dibs_domain::pr_status::PrState;
use dibs_domain::radar::BranchName;
use dibs_domain::repo::RepoId;
use dibs_domain::review::PrRef;

use crate::auth::Authed;
use crate::error::ApiError;
use crate::fields::{item_in_path, parse, parse_required, required};
use crate::http::AppState;
use crate::views::{ClaimView, ItemNameView, reply};

/// The most changed paths a merge report may carry.
pub const MAX_MERGED_FILES: usize = 3000;

/// The body of `POST /v1/items/{id}/review`.
#[derive(Deserialize)]
pub struct ReviewBody {
    pr: Option<String>,
    more: Option<bool>,
}

#[derive(Serialize)]
struct ReviewAnswer {
    claim: ClaimView,
    item: ItemNameView,
}

/// Records the pull request `pr` (`<owner>/<name>#<number>`) on the caller's claim of the item: the
/// claim is in review, keeps its areas and does not go stale while the pull request is open. With
/// `more: true`, merging it returns the claim to work rather than completing it, for the next pull
/// request of the item. Recording one the claim has changes at most `more`.
///
/// # Errors
/// `no_claim`, `claim_stale`, `pr_recorded` (`409`); `not_holder` (`403`, naming the holder);
/// `unlisted_repository` (`422`); `not_found`; `forbidden` for an integration; `invalid_request`
/// for a missing or malformed `pr`.
pub async fn start(
    State(state): State<AppState>,
    Authed(actor): Authed,
    path: Result<Path<String>, PathRejection>,
    body: Result<Json<ReviewBody>, JsonRejection>,
) -> Result<Response, ApiError> {
    actor.require_person()?;
    let Path(id) = path?;
    let item = item_in_path(&id)?;
    let Json(body) = body?;
    let input = StartReviewInput {
        item,
        pr: parse_required("pr", body.pr.as_deref(), PrRef::parse)?,
        more: body.more.unwrap_or(false),
    };
    let reviewed = state.app.start_review.execute(&actor, input).await?;
    Ok(reply(
        StatusCode::OK,
        &ReviewAnswer {
            claim: ClaimView::of(reviewed.claim),
            item: ItemNameView::of(reviewed.item),
        },
    ))
}

/// The body of `POST /v1/items/{id}/review/drop`.
#[derive(Deserialize)]
pub struct DropBody {
    pr: Option<String>,
}

/// Removes the pull request `pr` the caller recorded on their claim of the item by mistake. With
/// none left the claim is worked on again, seen now. Answers as recording one does.
///
/// # Errors
/// `no_claim`, `pr_not_recorded` (`409`); `not_holder` (`403`, naming the holder); `not_found`;
/// `forbidden` for an integration; `invalid_request` for a missing or malformed `pr`.
pub async fn drop(
    State(state): State<AppState>,
    Authed(actor): Authed,
    path: Result<Path<String>, PathRejection>,
    body: Result<Json<DropBody>, JsonRejection>,
) -> Result<Response, ApiError> {
    actor.require_person()?;
    let Path(id) = path?;
    let item = item_in_path(&id)?;
    let Json(body) = body?;
    let input = DropPrInput {
        item,
        pr: parse_required("pr", body.pr.as_deref(), PrRef::parse)?,
    };
    let reviewed = state.app.drop_pr.execute(&actor, input).await?;
    Ok(reply(
        StatusCode::OK,
        &ReviewAnswer {
            claim: ClaimView::of(reviewed.claim),
            item: ItemNameView::of(reviewed.item),
        },
    ))
}

/// The body of `POST /v1/prs/merged`.
#[derive(Deserialize)]
pub struct MergedBody {
    pr: Option<String>,
    base: Option<String>,
    merged_at: Option<String>,
    files: Option<Vec<String>>,
    head: Option<String>,
}

/// The body of `POST /v1/prs/closed`.
#[derive(Deserialize)]
pub struct ClosedBody {
    pr: Option<String>,
}

#[derive(Serialize)]
struct ReportAnswer {
    /// Whether a claim in review recorded the pull request; `false` changes nothing.
    matched: bool,
    /// The item of that claim.
    #[serde(skip_serializing_if = "Option::is_none")]
    item: Option<ItemId>,
    /// Where the claim stands now: `in_review`, `set_aside` (in review, its merges no longer
    /// completing it), `working` or `completed`.
    #[serde(skip_serializing_if = "Option::is_none")]
    claim: Option<&'static str>,
}

impl ReportAnswer {
    fn of(outcome: PrReportOutcome) -> Self {
        let (item, claim) = match outcome {
            PrReportOutcome::Unmatched => (None, None),
            PrReportOutcome::StillInReview(item) => (Some(item), Some("in_review")),
            PrReportOutcome::SetAside(item) => (Some(item), Some("set_aside")),
            PrReportOutcome::Working(item) => (Some(item), Some("working")),
            PrReportOutcome::Completed(item) => (Some(item), Some("completed")),
        };
        Self {
            matched: item.is_some(),
            item,
            claim,
        }
    }
}

/// An integration reports that the pull request `pr` was merged into `base` at `merged_at` (RFC
/// 3339). Only a pull request a claim in review recorded counts: it leaves that claim, and the
/// merge of the claim's last one completes it — areas free, item done, its holder told — unless
/// more were to follow, when the claim is worked on again. In a repository that protects branches,
/// a merge into any other base delivers nothing: the claim is set aside as after a close. A pull
/// request no claim recorded, or a report sent again, answers `matched: false` and changes no
/// claim. `files`, at most 3000 paths relative to the repository's root, are the paths the merge
/// changed, and `head` the branch it was merged from: a merge that delivers moves the base under
/// every live branch measured against `base` from an older merge base whose footprint holds one of
/// them, other than the merged branch, and each such branch's holder is told once. Without
/// `files`, the merged branch's footprint stands for them; without `head`, the footprint of the
/// claim that recorded the pull request is taken for the merged branch.
///
/// # Errors
/// `forbidden` for anyone but an integration or an admin; `invalid_request` for a missing or
/// malformed field.
pub async fn merged(
    State(state): State<AppState>,
    Authed(actor): Authed,
    body: Result<Json<MergedBody>, JsonRejection>,
) -> Result<Response, ApiError> {
    actor.require_import()?;
    let Json(body) = body?;
    let pr = parse_required("pr", body.pr.as_deref(), PrRef::parse)?;
    let merged_at = parse_required("merged_at", body.merged_at.as_deref(), |text| {
        OffsetDateTime::parse(text, &Rfc3339).map_err(|_| "it is a time in RFC 3339")
    })?;
    if body
        .files
        .as_ref()
        .is_some_and(|files| files.len() > MAX_MERGED_FILES)
    {
        return Err(ApiError::invalid_field(
            "files",
            &format!("a merge lists at most {MAX_MERGED_FILES} paths"),
        ));
    }
    let files = body
        .files
        .map(|files| {
            files
                .iter()
                .map(|text| parse("files", text, RelPath::parse))
                .collect::<Result<BTreeSet<_>, _>>()
        })
        .transpose()?;
    let head = body
        .head
        .as_deref()
        .map(|text| parse("head", text, BranchName::parse_ref))
        .transpose()?;
    let base = parse_required("base", body.base.as_deref(), branch_text)?;
    let report = PrReport {
        pr,
        end: ReportedEnd::Merged {
            base,
            merged_at,
            files,
            head,
        },
    };
    let outcome = state.app.report_pr.execute(&actor, report).await?;
    Ok(reply(StatusCode::OK, &ReportAnswer::of(outcome)))
}

/// An integration reports that the pull request `pr` was closed without being merged. Only a pull
/// request a claim in review recorded counts: it leaves that claim, and when it was the last one
/// the claim is worked on again, seen now, and its holder is told. Anything else answers
/// `matched: false` and changes nothing.
///
/// # Errors
/// `forbidden` for anyone but an integration or an admin; `invalid_request` for a missing or
/// malformed `pr`.
pub async fn closed(
    State(state): State<AppState>,
    Authed(actor): Authed,
    body: Result<Json<ClosedBody>, JsonRejection>,
) -> Result<Response, ApiError> {
    actor.require_import()?;
    let Json(body) = body?;
    let report = PrReport {
        pr: parse_required("pr", body.pr.as_deref(), PrRef::parse)?,
        end: ReportedEnd::Closed,
    };
    let outcome = state.app.report_pr.execute(&actor, report).await?;
    Ok(reply(StatusCode::OK, &ReportAnswer::of(outcome)))
}

/// A base branch as a forge names it: 1 to 255 characters, no whitespace or control characters.
fn branch_text(text: &str) -> Result<String, &'static str> {
    let ok = !text.is_empty()
        && text.len() <= 255
        && !text.chars().any(|c| c.is_whitespace() || c.is_control());
    if ok {
        Ok(text.to_owned())
    } else {
        Err("it is a branch name")
    }
}

#[derive(Serialize)]
struct ReviewClaimView {
    item: ItemNameView,
    holder: Handle,
    prs: BTreeSet<PrRef>,
    #[serde(serialize_with = "time::serde::rfc3339::serialize")]
    since: OffsetDateTime,
    more: bool,
    branches: BTreeMap<RepoId, BranchName>,
}

impl ReviewClaimView {
    fn of(claim: ReviewClaim) -> Self {
        Self {
            item: ItemNameView::of(claim.item),
            branches: claim.claim.claim.branches,
            holder: claim.claim.holder,
            prs: claim.prs,
            since: claim.since,
            more: claim.more,
        }
    }
}

#[derive(Serialize)]
struct ReviewClaimsAnswer {
    claims: Vec<ReviewClaimView>,
}

/// The claims in review, ordered by item, each with its holder, the pull requests it recorded and
/// the branch its work is on in each repository: what an integration watches for merges and
/// closes.
///
/// # Errors
/// `forbidden` for anyone but an integration or an admin.
pub async fn claims(
    State(state): State<AppState>,
    Authed(actor): Authed,
) -> Result<Response, ApiError> {
    actor.require_import()?;
    let listed = state.app.review_claims.execute(&actor).await?;
    Ok(reply(
        StatusCode::OK,
        &ReviewClaimsAnswer {
            claims: listed.into_iter().map(ReviewClaimView::of).collect(),
        },
    ))
}

/// The query of `GET /v1/pr-status`.
#[derive(Deserialize)]
pub struct StatusQuery {
    repo: Option<String>,
    base: Option<String>,
}

#[derive(Serialize)]
struct PrStatusAnswer {
    enabled: bool,
    context: String,
    state: PrState,
    description: String,
}

/// The status a forge shows on the pull requests of `repo` (its id, or its remote's path such as
/// `acme/api`) into `base` (`main` or `refs/heads/main`): `failure` while a held slot freezes that
/// branch, `success` otherwise, with the description the project's settings allow and the context
/// it is filed under. `enabled` says whether the project wants the repository's pull requests to
/// get one at all.
///
/// # Errors
/// `forbidden` for anyone but an integration or an admin; `not_found` for a repository the project
/// does not list or a base that is no branch; `invalid_request` for a missing field.
pub async fn status(
    State(state): State<AppState>,
    Authed(actor): Authed,
    query: Result<Query<StatusQuery>, QueryRejection>,
) -> Result<Response, ApiError> {
    actor.require_import()?;
    let Query(query) = query?;
    let query = PrStatusQuery {
        repo: required("repo", query.repo)?,
        base: required("base", query.base)?,
    };
    let view = state.app.pr_status.execute(&actor, query).await?;
    Ok(reply(
        StatusCode::OK,
        &PrStatusAnswer {
            enabled: view.enabled,
            context: view.context,
            state: view.state,
            description: view.description,
        },
    ))
}
