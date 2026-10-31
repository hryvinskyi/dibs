//! `GET /v1/worktrees`, `POST /v1/worktrees/take`, `/release` and `/beat`, and the admin's
//! `POST /v1/admin/worktrees/force-release`: the worktrees a member's machine lends to their
//! sessions and to the agents those sessions start, one holder each.
//!
//! A request names its pool by the machine and the pool id its client computed; the server keeps
//! them for the caller alone and never shows them to another member.

use axum::Json;
use axum::extract::rejection::{JsonRejection, QueryRejection};
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::Response;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use dibs_app::use_cases::{
    BeatWorktreesInput, ForceReleaseWorktreeInput, ReleaseWorktreeInput, TakeWorktreeInput,
    TookWorktree, WorktreeBeatInput, WorktreeRelease,
};
use dibs_domain::handle::Handle;
use dibs_domain::ids::ThreadId;
use dibs_domain::message::Body;
use dibs_domain::report::MachineId;
use dibs_domain::session::HostName;
use dibs_domain::slot::Purpose;
use dibs_domain::worktree::{AgentKey, PoolId, WorktreeName};

use crate::auth::Authed;
use crate::error::ApiError;
use crate::fields::{item_in_body, parse, parse_required, required, session_in_body};
use crate::http::AppState;
use crate::views::reply;
use crate::views::worktrees::{BeatenView, BoardHoldView, OwnHoldView, OwnPoolView};

/// The machine and the pool a body names.
fn pool_in_body(
    machine: Option<&str>,
    pool: Option<&str>,
) -> Result<(MachineId, PoolId), ApiError> {
    Ok((
        parse_required("machine", machine, MachineId::parse)?,
        parse_required("pool", pool, PoolId::parse)?,
    ))
}

/// The agent a field names, when it names one.
fn agent_in_body(field: &str, text: Option<&str>) -> Result<Option<AgentKey>, ApiError> {
    text.map(|text| parse(field, text, AgentKey::parse))
        .transpose()
}

/// The query of `GET /v1/worktrees`.
#[derive(Deserialize)]
pub struct ListQuery {
    machine: Option<String>,
}

#[derive(Serialize)]
struct ListAnswer {
    /// The caller's own pools with a hold.
    pools: Vec<OwnPoolView>,
    /// Every member's holds, the caller's included.
    board: Vec<BoardHoldView>,
}

/// The caller's own pools with a hold — only those on `machine` when the query names one — and
/// every member's holds as the board shows them. Holds gone silent end before the answer is read.
/// An integration may read them; it has no pool of its own.
///
/// # Errors
/// `invalid_query` for a malformed query string; `invalid_request` for a `machine` that is no
/// machine id.
pub async fn list(
    State(state): State<AppState>,
    Authed(actor): Authed,
    query: Result<Query<ListQuery>, QueryRejection>,
) -> Result<Response, ApiError> {
    let Query(query) = query?;
    let machine = query
        .machine
        .as_deref()
        .map(|text| parse("machine", text, MachineId::parse))
        .transpose()?;
    let view = state
        .app
        .worktrees
        .execute(&actor, machine.as_ref())
        .await?;
    Ok(reply(
        StatusCode::OK,
        &ListAnswer {
            pools: view.mine.into_iter().map(OwnPoolView::of).collect(),
            board: view.board.into_iter().map(BoardHoldView::of).collect(),
        },
    ))
}

/// The body of `POST /v1/worktrees/take`.
#[derive(Deserialize)]
pub struct TakeBody {
    session_id: Option<u64>,
    machine: Option<String>,
    pool: Option<String>,
    names: Option<Vec<String>>,
    agent: Option<String>,
    purpose: Option<String>,
    item: Option<u64>,
}

#[derive(Serialize)]
struct TakeAnswer {
    /// `took` or `busy`.
    outcome: &'static str,
    /// The worktree the holder holds now; `null` when busy.
    worktree: Option<WorktreeName>,
    /// Whether the holder held it before asking; `false` when busy.
    again: bool,
    /// The hold of each worktree named, in the order named, when busy; empty otherwise.
    held: Vec<OwnHoldView>,
}

/// Takes a worktree of the caller's pool for their session `session_id`, or for its `agent`: a
/// holder that holds one gets it again (`again`); otherwise the first of `names` nobody holds;
/// otherwise the answer is `busy` with the hold of each name. The server never queues a take.
///
/// # Errors
/// `forbidden` for an integration; `not_found` for a session not the caller's or an unknown item;
/// `worktree_names_invalid` (`422`) for no name or a name given twice; `invalid_request` for a
/// missing or invalid field.
pub async fn take(
    State(state): State<AppState>,
    Authed(actor): Authed,
    body: Result<Json<TakeBody>, JsonRejection>,
) -> Result<Response, ApiError> {
    actor.require_person()?;
    let Json(body) = body?;
    let (machine, pool) = pool_in_body(body.machine.as_deref(), body.pool.as_deref())?;
    let names = required("names", body.names)?
        .iter()
        .map(|text| parse("names", text, WorktreeName::parse))
        .collect::<Result<Vec<_>, _>>()?;
    let input = TakeWorktreeInput {
        session: session_in_body("session_id", body.session_id)?,
        machine,
        pool,
        names,
        agent: agent_in_body("agent", body.agent.as_deref())?,
        purpose: parse_required("purpose", body.purpose.as_deref(), Purpose::parse)?,
        item: body
            .item
            .map(|item| item_in_body("item", Some(item)))
            .transpose()?,
    };
    let answer = match state.app.take_worktree.execute(&actor, input).await? {
        TookWorktree::Took { worktree, again } => TakeAnswer {
            outcome: "took",
            worktree: Some(worktree),
            again,
            held: Vec::new(),
        },
        TookWorktree::Busy { held } => TakeAnswer {
            outcome: "busy",
            worktree: None,
            again: false,
            held: held.into_iter().map(OwnHoldView::of).collect(),
        },
    };
    Ok(reply(StatusCode::OK, &answer))
}

/// The body of `POST /v1/worktrees/release`.
#[derive(Deserialize)]
pub struct ReleaseBody {
    session_id: Option<u64>,
    machine: Option<String>,
    pool: Option<String>,
    worktree: Option<String>,
    holder_session: Option<u64>,
    agent: Option<String>,
}

#[derive(Serialize)]
struct ReleasedAnswer {
    /// The hold that ended, as it was.
    released: OwnHoldView,
}

/// What a release body names: a worktree, or a holder — a session and, for one of its agents,
/// the agent's key.
fn release_in_body(body: &ReleaseBody) -> Result<WorktreeRelease, ApiError> {
    const ONE: &str = "name either a worktree or a holder_session";
    match (body.worktree.as_deref(), body.holder_session) {
        (Some(text), None) => {
            if body.agent.is_some() {
                return Err(ApiError::invalid_field(
                    "agent",
                    &"an agent is named with its holder_session",
                ));
            }
            Ok(WorktreeRelease::Name(parse(
                "worktree",
                text,
                WorktreeName::parse,
            )?))
        }
        (None, Some(session)) => Ok(WorktreeRelease::Holder {
            session: session_in_body("holder_session", Some(session))?,
            agent: agent_in_body("agent", body.agent.as_deref())?,
        }),
        (Some(_), Some(_)) | (None, None) => Err(ApiError::invalid_field("worktree", &ONE)),
    }
}

/// Ends a hold of the caller's own pool: the hold of `worktree`, whichever of their sessions or
/// agents holds it, or the hold of the holder `holder_session` (with `agent` for one of its
/// agents). Answers the hold as it was.
///
/// # Errors
/// `forbidden` for an integration; `not_found` for a session not the caller's;
/// `worktree_not_held` (`409`) when nothing named is held; `invalid_request` for a missing or
/// invalid field, or for naming both a worktree and a holder, or neither.
pub async fn release(
    State(state): State<AppState>,
    Authed(actor): Authed,
    body: Result<Json<ReleaseBody>, JsonRejection>,
) -> Result<Response, ApiError> {
    actor.require_person()?;
    let Json(body) = body?;
    let session = session_in_body("session_id", body.session_id)?;
    let (machine, pool) = pool_in_body(body.machine.as_deref(), body.pool.as_deref())?;
    let input = ReleaseWorktreeInput {
        session,
        machine,
        pool,
        release: release_in_body(&body)?,
    };
    let held = state.app.release_worktree.execute(&actor, input).await?;
    Ok(reply(
        StatusCode::OK,
        &ReleasedAnswer {
            released: OwnHoldView::of(held),
        },
    ))
}

/// One beat of `POST /v1/worktrees/beat`.
#[derive(Deserialize)]
pub struct BeatEntry {
    worktree: Option<String>,
    holder_session: Option<u64>,
    agent: Option<String>,
    seen_at: Option<String>,
}

/// The body of `POST /v1/worktrees/beat`.
#[derive(Deserialize)]
pub struct BeatBody {
    session_id: Option<u64>,
    machine: Option<String>,
    pool: Option<String>,
    beats: Option<Vec<BeatEntry>>,
}

#[derive(Serialize)]
struct BeatAnswer {
    /// The worktrees and holders whose holds the beats kept.
    kept: Vec<BeatenView>,
    /// The worktrees and holders that hold nothing: the worktree is free or another holder has it.
    unknown: Vec<BeatenView>,
}

/// A beat of the body, each field named by its place in `beats`.
fn beat_in_body(at: usize, beat: &BeatEntry) -> Result<WorktreeBeatInput, ApiError> {
    let field = |name: &str| format!("beats[{at}].{name}");
    Ok(WorktreeBeatInput {
        worktree: parse_required(
            &field("worktree"),
            beat.worktree.as_deref(),
            WorktreeName::parse,
        )?,
        session: session_in_body(&field("holder_session"), beat.holder_session)?,
        agent: agent_in_body(&field("agent"), beat.agent.as_deref())?,
        seen_at: parse_required(&field("seen_at"), beat.seen_at.as_deref(), |text| {
            OffsetDateTime::parse(text, &Rfc3339).map_err(|_| "it is a time in RFC 3339")
        })?,
    })
}

/// The holders' signs of life, sent by the caller's session `session_id` on behalf of any of the
/// caller's sessions and their agents: each beat that names a worktree and its holder keeps that
/// hold seen at `seen_at` (a time after now counts as now; a hold's last sight never moves back).
/// A beat naming a worktree that is free or held by another holder changes nothing and is
/// answered `unknown`. Each worktree and holder is answered once, in the order first named.
///
/// # Errors
/// `forbidden` for an integration; `not_found` for a session not the caller's, sending or named
/// by a beat; `invalid_request` for a missing or invalid field.
pub async fn beat(
    State(state): State<AppState>,
    Authed(actor): Authed,
    body: Result<Json<BeatBody>, JsonRejection>,
) -> Result<Response, ApiError> {
    actor.require_person()?;
    let Json(body) = body?;
    let session = session_in_body("session_id", body.session_id)?;
    let (machine, pool) = pool_in_body(body.machine.as_deref(), body.pool.as_deref())?;
    let beats = required("beats", body.beats)?
        .iter()
        .enumerate()
        .map(|(at, beat)| beat_in_body(at, beat))
        .collect::<Result<Vec<_>, _>>()?;
    let input = BeatWorktreesInput {
        session,
        machine,
        pool,
        beats,
    };
    let beaten = state.app.beat_worktrees.execute(&actor, input).await?;
    Ok(reply(
        StatusCode::OK,
        &BeatAnswer {
            kept: beaten.kept.into_iter().map(BeatenView::of).collect(),
            unknown: beaten.unknown.into_iter().map(BeatenView::of).collect(),
        },
    ))
}

/// The body of `POST /v1/admin/worktrees/force-release`.
#[derive(Deserialize)]
pub struct ForceBody {
    session_id: Option<u64>,
    member: Option<String>,
    host: Option<String>,
    worktree: Option<String>,
    reason: Option<String>,
}

#[derive(Serialize)]
struct ForcedAnswer {
    /// The holds that ended, as the board showed them: one for each of the member's pools on that
    /// host that held the worktree, usually one.
    released: Vec<BoardHoldView>,
    /// The thread that told the member the reason; `null` when the admin's own hold ended.
    thread_id: Option<ThreadId>,
}

/// An admin ends a member's hold of a worktree on a host, named as the board names it, and the
/// member is sent `reason`.
///
/// # Errors
/// `forbidden` unless the caller is an admin; `not_found` for a session not theirs or a member
/// the project does not have; `worktree_not_held` (`409`) when the member holds that worktree on
/// that host in no pool; `invalid_request` for a missing or invalid field.
pub async fn force_release(
    State(state): State<AppState>,
    Authed(actor): Authed,
    body: Result<Json<ForceBody>, JsonRejection>,
) -> Result<Response, ApiError> {
    actor.require_admin()?;
    let Json(body) = body?;
    let input = ForceReleaseWorktreeInput {
        session: session_in_body("session_id", body.session_id)?,
        member: parse_required("member", body.member.as_deref(), Handle::parse)?,
        host: parse_required("host", body.host.as_deref(), HostName::parse)?,
        worktree: parse_required("worktree", body.worktree.as_deref(), WorktreeName::parse)?,
        reason: parse_required("reason", body.reason.as_deref(), Body::parse)?,
    };
    let forced = state
        .app
        .force_release_worktree
        .execute(&actor, input)
        .await?;
    Ok(reply(
        StatusCode::OK,
        &ForcedAnswer {
            released: forced.ended.into_iter().map(BoardHoldView::of).collect(),
            thread_id: forced.thread,
        },
    ))
}
