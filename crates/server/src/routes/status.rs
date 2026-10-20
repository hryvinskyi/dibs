//! `GET /v1/status`: who holds what right now.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Response;
use serde::Serialize;

use dibs_app::use_cases::{OpenEscalation, WideClaimStatus};
use dibs_domain::area::QualifiedArea;
use dibs_domain::escalation::EscalationSubject;
use dibs_domain::handle::Handle;
use dibs_domain::ids::{ItemId, MessageId, ThreadId};
use time::OffsetDateTime;

use crate::auth::Authed;
use crate::error::ApiError;
use crate::http::AppState;
use crate::views::slots::SlotView;
use crate::views::{HeldItemView, ReadyView, reply};

#[derive(Serialize)]
struct StatusAnswer {
    live: Vec<HeldItemView>,
    stale: Vec<HeldItemView>,
    holds: Vec<HoldView>,
    wide: Vec<WideClaimView>,
    ready: ReadyView,
    slots: Vec<SlotView>,
    escalations: Vec<EscalationView>,
}

/// An escalation the caller may read that no arbiter has acknowledged yet.
#[derive(Serialize)]
struct EscalationView {
    subject: EscalationSubject,
    about: String,
    by: Handle,
    arbiters: Vec<Handle>,
    thread_id: ThreadId,
    message_id: MessageId,
    #[serde(serialize_with = "time::serde::rfc3339::serialize")]
    at: OffsetDateTime,
}

impl EscalationView {
    fn of(open: OpenEscalation) -> Self {
        Self {
            subject: open.subject,
            about: open.about,
            by: open.by,
            arbiters: open.arbiters,
            thread_id: open.thread,
            message_id: open.message,
            at: open.at,
        }
    }
}

/// A live wide claim and where each area it names stands.
#[derive(Serialize)]
struct WideClaimView {
    #[serde(flatten)]
    held: HeldItemView,
    areas: Vec<WideAreaView>,
}

/// One area of a wide claim: `free` when nobody else holds it, `granted` when its holder
/// acknowledged the announcement or granted the edits, `waiting` while they have not.
#[derive(Serialize)]
struct WideAreaView {
    area: QualifiedArea,
    state: &'static str,
    holder: Option<Handle>,
    item: Option<ItemId>,
}

impl WideClaimView {
    fn of(wide: WideClaimStatus) -> Self {
        Self {
            held: HeldItemView::of(wide.held),
            areas: wide
                .areas
                .into_iter()
                .map(|row| {
                    let state = match (&row.holder, row.granted) {
                        (None, _) => "free",
                        (Some(_), true) => "granted",
                        (Some(_), false) => "waiting",
                    };
                    let (holder, item) =
                        row.holder.map_or((None, None), |(h, i)| (Some(h), Some(i)));
                    WideAreaView {
                        area: row.area,
                        state,
                        holder,
                        item,
                    }
                })
                .collect(),
        }
    }
}

#[derive(Serialize)]
struct HoldView {
    area: QualifiedArea,
    holder: Handle,
    item: ItemId,
}

/// The project's live claims, its stale claims, the areas the live ones hold, the live wide claims
/// with where each of their areas stands, the caller's ready list, the slots and the escalations
/// still waiting for an arbiter, each ordered as the use case orders it.
///
/// # Errors
/// `forbidden` for an integration; the answer for the use case's failure.
pub async fn status(
    State(state): State<AppState>,
    Authed(actor): Authed,
) -> Result<Response, ApiError> {
    actor.require_person()?;
    let view = state.app.status.execute(&actor).await?;
    let answer = StatusAnswer {
        live: view.live.into_iter().map(HeldItemView::of).collect(),
        stale: view.stale.into_iter().map(HeldItemView::of).collect(),
        holds: view
            .holds
            .into_iter()
            .map(|hold| HoldView {
                holder: hold.holder,
                area: hold.area,
                item: hold.item,
            })
            .collect(),
        wide: view.wide.into_iter().map(WideClaimView::of).collect(),
        ready: ReadyView::of(view.ready),
        slots: view.slots.into_iter().map(SlotView::of).collect(),
        escalations: view
            .escalations
            .into_iter()
            .map(EscalationView::of)
            .collect(),
    };
    Ok(reply(StatusCode::OK, &answer))
}
