//! What the pilot of a project is judged by: the day reports heartbeats carry, `POST /v1/feedback`
//! from members' agents, `POST /v1/prs/state` from an integration, and `GET /v1/stats` for admins.

use std::collections::BTreeMap;

use axum::Json;
use axum::extract::rejection::{JsonRejection, QueryRejection};
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::Response;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use time::OffsetDateTime;

use dibs_app::use_cases::{FeedbackAbout, FeedbackInput, StatsView};
use dibs_domain::handle::Handle;
use dibs_domain::ids::MemberId;
use dibs_domain::paths::{RelPath, RepoPath};
use dibs_domain::repo::RepoId;
use dibs_domain::report::stats::{DurationFigure, StatsRange};
use dibs_domain::report::{
    BUCKETS, DayCounts, DayReport, DurationBuckets, Feedback, FeedbackReason, HeartbeatReports,
    HookDurations, HookEventName, MachineId, RefusalId, ReportDay, Tally,
};
use dibs_domain::review::PrRef;
use dibs_domain::session::AgentLabel;

use crate::auth::Authed;
use crate::error::ApiError;
use crate::fields::{parse, parse_required, required};
use crate::http::AppState;
use crate::views::reply;

/// The reports a heartbeat's body carries that can be read, of the machine `machine` names. A
/// report that cannot be read — a day that is not one, counts or durations of another shape, a day
/// given twice, more days than a heartbeat reports — is left out and logged as a warning naming the
/// member and why, never what it held; within a report, a count of a kind this server does not
/// know and a duration it cannot read are left out and the rest is kept. So a report never fails
/// the heartbeat that carries it. `None` when nothing can be read, or no machine is named.
pub fn heartbeat_reports(
    member: MemberId,
    machine: Option<&Value>,
    reports: Option<&Value>,
) -> Option<HeartbeatReports> {
    let entries = match reports {
        None | Some(Value::Null) => return None,
        Some(Value::Array(entries)) => entries,
        Some(_) => {
            left_out(member, "the reports are not a list");
            return None;
        }
    };
    let Some(machine) = machine
        .and_then(Value::as_str)
        .and_then(|text| MachineId::parse(text).ok())
    else {
        left_out(member, "the reports name no machine id");
        return None;
    };
    let mut kept: Vec<DayReport> = Vec::new();
    for entry in entries {
        match day_report(entry) {
            Ok(_) if kept.len() == HeartbeatReports::MAX => {
                left_out(member, "more days than a heartbeat reports");
            }
            Ok(report) if kept.iter().any(|known| known.day == report.day) => {
                left_out(member, "a day reported twice");
            }
            Ok(report) => kept.push(report),
            Err(reason) => left_out(member, reason),
        }
    }
    match HeartbeatReports::new(machine, kept) {
        Ok(reports) => Some(reports),
        Err(error) => {
            tracing::warn!(member = %member, reason = %error, "a heartbeat's reports were left out");
            None
        }
    }
}

fn left_out(member: MemberId, reason: &str) {
    tracing::warn!(member = %member, reason, "a report of a heartbeat was left out");
}

/// One day's report, what of it can be read.
fn day_report(entry: &Value) -> Result<DayReport, &'static str> {
    let Value::Object(fields) = entry else {
        return Err("a report is not an object");
    };
    let day = fields
        .get("day")
        .and_then(Value::as_str)
        .and_then(|text| ReportDay::parse(text).ok())
        .ok_or("a report names no day")?;
    let counts: BTreeMap<Tally, u64> = match fields.get("counts") {
        None | Some(Value::Null) => BTreeMap::new(),
        Some(Value::Object(counts)) => counts
            .iter()
            .filter_map(|(name, count)| Some((Tally::parse(name)?, count.as_u64()?)))
            .collect(),
        Some(_) => return Err("a report's counts are not an object"),
    };
    let mut durations: Vec<HookDurations> = Vec::new();
    match fields.get("durations") {
        None | Some(Value::Null) => {}
        Some(Value::Array(entries)) => {
            for entry in entries.iter().filter_map(hook_durations) {
                let known = durations
                    .iter()
                    .any(|seen| seen.agent == entry.agent && seen.event == entry.event);
                if !known && durations.len() < DayCounts::MAX_DURATIONS {
                    durations.push(entry);
                }
            }
        }
        Some(_) => return Err("a report's durations are not a list"),
    }
    let counts = DayCounts::new(counts, durations).map_err(|_| "a report's durations repeat")?;
    Ok(DayReport { day, counts })
}

/// One agent's hook durations, when they can be read.
fn hook_durations(entry: &Value) -> Option<HookDurations> {
    let text = |name: &str| entry.get(name).and_then(Value::as_str);
    let buckets: Vec<u64> = entry
        .get("buckets")?
        .as_array()?
        .iter()
        .map(Value::as_u64)
        .collect::<Option<_>>()?;
    let buckets: [u64; BUCKETS] = buckets.try_into().ok()?;
    Some(HookDurations {
        agent: AgentLabel::parse(text("agent")?).ok()?,
        event: HookEventName::parse(text("event")?).ok()?,
        buckets: DurationBuckets::new(buckets),
    })
}

/// The body of `POST /v1/feedback`.
#[derive(Deserialize)]
pub struct FeedbackBody {
    kind: Option<String>,
    refusal: Option<String>,
    tally: Option<String>,
    repo: Option<String>,
    path: Option<String>,
    reason: Option<String>,
}

#[derive(Serialize)]
struct FeedbackAnswer {
    feedback: Feedback,
}

/// Records what a member's agent says of the guard, as one `feedback_given` event only the member
/// and the project's admins see, and answers `201` with it as it was kept. `kind` is
/// `false_refusal` — a refusal the agent found wrong, named by the id it was shown with
/// (`refusal`, `r-7f3a`) and, when the agent's machine still knows it, its kind (`tally`, a kind
/// this server does not know left out) — or `conflict`, a rebase conflict the agent resolved by
/// hand in the file `path` of the repository `repo`, which is kept as the area it lies in, never
/// as the path. `reason`, 1 to 500 characters, says why.
///
/// # Errors
/// `forbidden` for an integration; `invalid_request` for a missing or unknown kind, or a missing
/// or invalid field of it; `unknown_repo` for a repository the project does not list.
pub async fn feedback(
    State(state): State<AppState>,
    Authed(actor): Authed,
    body: Result<Json<FeedbackBody>, JsonRejection>,
) -> Result<Response, ApiError> {
    actor.require_person()?;
    let Json(body) = body?;
    let kind = required("kind", body.kind.as_deref())?;
    let about = match kind {
        "false_refusal" => FeedbackAbout::FalseRefusal {
            refusal: parse_required("refusal", body.refusal.as_deref(), RefusalId::parse)?,
            tally: body.tally.as_deref().and_then(Tally::parse),
        },
        "conflict" => FeedbackAbout::Conflict {
            path: RepoPath {
                repo: parse_required("repo", body.repo.as_deref(), RepoId::parse)?,
                path: parse_required("path", body.path.as_deref(), RelPath::parse)?,
            },
        },
        _ => {
            return Err(ApiError::invalid_field(
                "kind",
                &"a kind of feedback is false_refusal or conflict",
            ));
        }
    };
    let reason = parse_required("reason", body.reason.as_deref(), FeedbackReason::parse)?;
    let feedback = state
        .app
        .give_feedback
        .execute(&actor, FeedbackInput { about, reason })
        .await?;
    Ok(reply(StatusCode::CREATED, &FeedbackAnswer { feedback }))
}

/// The body of `POST /v1/prs/state`.
#[derive(Deserialize)]
pub struct PrStateBody {
    pr: Option<String>,
    mergeable: Option<bool>,
}

#[derive(Serialize)]
struct PrStateAnswer {
    /// Whether the report appended a `pr_state_reported` event.
    recorded: bool,
}

/// An integration's report of whether the pull request `pr` merges cleanly into its base
/// (`mergeable`). A pull request reported as not merging cleanly, or as merging cleanly again
/// after that, appends `pr_state_reported`; the same report again appends nothing, and one first
/// reported as merging cleanly is only remembered. Any pull request may be reported, not only one a
/// claim recorded.
///
/// # Errors
/// `forbidden` for a member who is not an admin; `invalid_request` for a missing or malformed `pr`
/// or a missing `mergeable`.
pub async fn pr_state(
    State(state): State<AppState>,
    Authed(actor): Authed,
    body: Result<Json<PrStateBody>, JsonRejection>,
) -> Result<Response, ApiError> {
    actor.require_import()?;
    let Json(body) = body?;
    let pr = parse_required("pr", body.pr.as_deref(), PrRef::parse)?;
    let mergeable = required("mergeable", body.mergeable)?;
    let recorded = state
        .app
        .report_pr_state
        .execute(&actor, pr, mergeable)
        .await?;
    Ok(reply(StatusCode::OK, &PrStateAnswer { recorded }))
}

/// The query of `GET /v1/stats`.
#[derive(Deserialize)]
pub struct StatsQuery {
    from: Option<String>,
    to: Option<String>,
}

/// The figures of the days from `from` to `to` (`YYYY-MM-DD`, both included, at most 92 days),
/// for admins: `to` is today (UTC) when absent, `from` six days before `to`.
///
/// # Errors
/// `forbidden` for anyone but an admin; `invalid_request` for a day that is not one, or a range
/// that ends before it starts or is longer than 92 days; `invalid_query` for a malformed query
/// string.
pub async fn stats(
    State(state): State<AppState>,
    Authed(actor): Authed,
    query: Result<Query<StatsQuery>, QueryRejection>,
) -> Result<Response, ApiError> {
    actor.require_admin()?;
    let Query(query) = query?;
    let to = query
        .to
        .as_deref()
        .map(|text| parse("to", text, ReportDay::parse))
        .transpose()?
        .unwrap_or_else(|| ReportDay::of(state.clock.now()));
    let range = match query.from.as_deref() {
        Some(text) => StatsRange::new(parse("from", text, ReportDay::parse)?, to)
            .map_err(|error| ApiError::invalid_field("to", &error))?,
        None => StatsRange::week_to(to),
    };
    let view = state.app.read_stats.execute(&actor, range).await?;
    Ok(reply(StatusCode::OK, &StatsAnswer::of(&view)?))
}

#[derive(Serialize)]
struct StatsAnswer<'a> {
    from: ReportDay,
    to: ReportDay,
    truncated: bool,
    claims: u64,
    takeovers: u64,
    stalls: u64,
    escalations: u64,
    slot_waits: SlotWaitsView,
    reported: BTreeMap<&'static str, u64>,
    reported_days: Vec<MemberDayView<'a>>,
    hook_durations: Vec<DurationView<'a>>,
    hook_durations_by_member: Vec<DurationView<'a>>,
    feedback: FeedbackView<'a>,
    conflicting_prs: ConflictingView<'a>,
}

#[derive(Serialize)]
struct SlotWaitsView {
    count: u64,
    median_seconds: Option<i64>,
    max_seconds: Option<i64>,
}

#[derive(Serialize)]
struct MemberDayView<'a> {
    member: &'a Handle,
    day: ReportDay,
    counts: BTreeMap<&'static str, u64>,
}

#[derive(Serialize)]
struct DurationView<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    member: Option<&'a Handle>,
    agent: &'a AgentLabel,
    event: &'a HookEventName,
    runs: u64,
    p95_ms: Option<u64>,
    buckets: DurationBuckets,
}

#[derive(Serialize)]
struct FeedbackView<'a> {
    false_refusals: u64,
    conflicts: u64,
    entries: Vec<FeedbackEntryView<'a>>,
}

#[derive(Serialize)]
struct FeedbackEntryView<'a> {
    #[serde(serialize_with = "time::serde::rfc3339::serialize")]
    at: OffsetDateTime,
    member: &'a Handle,
    #[serde(flatten)]
    about: &'a Feedback,
    reason: &'a FeedbackReason,
}

#[derive(Serialize)]
struct ConflictingView<'a> {
    reports: u64,
    prs: Vec<&'a PrRef>,
}

/// Every kind with its count, those that did not happen as `0`.
fn every_kind(counts: &BTreeMap<Tally, u64>) -> BTreeMap<&'static str, u64> {
    Tally::ALL
        .into_iter()
        .map(|tally| (tally.name(), counts.get(&tally).copied().unwrap_or(0)))
        .collect()
}

impl<'a> StatsAnswer<'a> {
    fn of(view: &'a StatsView) -> Result<Self, ApiError> {
        let handle = |member: MemberId| {
            view.members
                .get(&member)
                .ok_or_else(|| {
                    tracing::error!(member = %member, "the figures name a member they have no handle for");
                    ApiError::internal()
                })
        };
        let stats = &view.stats;
        let duration = |figure: &'a DurationFigure| -> Result<DurationView<'a>, ApiError> {
            Ok(DurationView {
                member: figure.member.map(handle).transpose()?,
                agent: &figure.agent,
                event: &figure.event,
                runs: figure.runs(),
                p95_ms: figure.p95_ms(),
                buckets: figure.buckets,
            })
        };
        Ok(Self {
            from: view.range.from(),
            to: view.range.to(),
            truncated: view.truncated,
            claims: stats.claims,
            takeovers: stats.takeovers,
            stalls: stats.stalls,
            escalations: stats.escalations,
            slot_waits: SlotWaitsView {
                count: stats.slot_waits.count,
                median_seconds: stats.slot_waits.median.map(|wait| wait.whole_seconds()),
                max_seconds: stats.slot_waits.max.map(|wait| wait.whole_seconds()),
            },
            reported: every_kind(&stats.reported),
            reported_days: stats
                .member_days
                .iter()
                .map(|day| {
                    Ok(MemberDayView {
                        member: handle(day.member)?,
                        day: day.day,
                        counts: day
                            .counts
                            .iter()
                            .map(|(tally, count)| (tally.name(), *count))
                            .collect(),
                    })
                })
                .collect::<Result<_, ApiError>>()?,
            hook_durations: stats
                .durations
                .iter()
                .map(duration)
                .collect::<Result<_, _>>()?,
            hook_durations_by_member: stats
                .member_durations
                .iter()
                .map(duration)
                .collect::<Result<_, _>>()?,
            feedback: FeedbackView {
                false_refusals: stats.false_refusals,
                conflicts: stats.conflicts,
                entries: stats
                    .feedback
                    .iter()
                    .map(|entry| {
                        Ok(FeedbackEntryView {
                            at: entry.at,
                            member: handle(entry.member)?,
                            about: &entry.about,
                            reason: &entry.reason,
                        })
                    })
                    .collect::<Result<_, ApiError>>()?,
            },
            conflicting_prs: ConflictingView {
                reports: stats.conflicting_reports,
                prs: stats.conflicting_prs.iter().collect(),
            },
        })
    }
}
