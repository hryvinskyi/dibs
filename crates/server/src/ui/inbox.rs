//! `GET /ui/inbox` and `GET /ui/inbox/{thread}`: Inbox — the messages from people, the grant
//! requests waiting for the viewer's decision, and dibs's notices folded onto the items they are
//! about — filtered by text, and a conversation in the inspector beside it, with its reply form.
//!
//! Opening a conversation marks it read. The position in the log is read before the conversation,
//! the conversation is marked read before the frame is read, and the page follows the stream from
//! that first position: the rail counts the conversation as read, and a message posted between the
//! reads reaches the conversation's live view.
//!
//! An address whose segment does not read as a conversation, or names one the viewer may not read
//! or that does not exist, answers `404` with the whole page and an inspector saying so; nothing is
//! marked read.

mod list;
mod query;
pub(crate) mod summary;

use std::collections::BTreeMap;

use askama::Template;
use axum::extract::rejection::{PathRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::Response;

use dibs_app::AppError;
use dibs_app::named::NamedSummary;
use dibs_app::records::ItemRecord;
use dibs_app::use_cases::{ItemQuery, StatusView};
use dibs_domain::ids::{ItemId, ThreadId};
use dibs_domain::message::Importance;

use crate::error::ApiError;
use crate::fields::thread_in_path;
use crate::http::AppState;
use crate::ui::claims::view::{ClaimView, Scene};
use crate::ui::conversation::{Conversation, Draft, ReplyProblem, item_of};
use crate::ui::inbox::list::{Body, ItemFacts, Mailbox, TABS, Tab, TabEntry};
use crate::ui::inbox::query::{ListQuery, RawQuery};
use crate::ui::inbox::summary::{ConversationSummary, notice, waiting_grants};
use crate::ui::links::{LinkContext, TicketView};
use crate::ui::live::latest_event;
use crate::ui::moments::{Moment, moment};
use crate::ui::render::html;
use crate::ui::session::{PageSession, UiSession};
use crate::ui::shell::{self, Here, Loaded, Screen, ShellView};
use crate::ui::signed_page;

#[derive(Template)]
#[template(path = "screens/inbox.html")]
struct InboxPage {
    shell: ShellView,
    list: ListView,
    inspector: Inspector,
}

/// The list of conversations.
#[derive(Debug)]
struct ListView {
    /// The tabs, each with its count.
    tabs: Vec<TabEntry>,
    /// The name of the tab shown, unless it is the default.
    tab: Option<&'static str>,
    /// The text the conversations match.
    q: String,
    /// The address of the list alone, which closes the inspector.
    close: String,
    /// The rows of the tab shown.
    body: Body,
}

/// What the inspector shows.
#[derive(Debug)]
enum Inspector {
    /// Nothing selected.
    Hint,
    /// A conversation the viewer cannot read.
    Gone,
    /// A conversation.
    Open(Box<OpenView>),
}

/// A conversation in the inspector.
#[derive(Debug)]
struct OpenView {
    /// The conversation as the list shows it.
    summary: ConversationSummary,
    /// Its messages and reply form.
    conversation: Conversation,
    /// When its last message was written.
    last: Moment,
}

/// Which conversation an address selects.
#[derive(Debug, Clone, Copy)]
enum Selected {
    /// None.
    Nothing,
    /// This one.
    Thread(ThreadId),
    /// A segment that names no conversation.
    Unreadable,
}

/// Inbox, nothing selected.
///
/// # Errors
/// The answer for a use case's failure.
pub async fn list(
    State(state): State<AppState>,
    PageSession(session): PageSession,
    here: Here,
    query: Result<Query<RawQuery>, QueryRejection>,
) -> Result<Response, ApiError> {
    render(
        &state,
        &session,
        &here,
        state_of(query),
        Selected::Nothing,
        None,
    )
    .await
}

/// Inbox with a conversation selected.
///
/// # Errors
/// The answer for a use case's failure.
pub async fn inspect(
    State(state): State<AppState>,
    PageSession(session): PageSession,
    here: Here,
    path: Result<Path<String>, PathRejection>,
    query: Result<Query<RawQuery>, QueryRejection>,
) -> Result<Response, ApiError> {
    let selected = path
        .ok()
        .and_then(|Path(segment)| thread_in_path(&segment).ok())
        .map_or(Selected::Unreadable, Selected::Thread);
    render(&state, &session, &here, state_of(query), selected, None).await
}

/// Inbox with the conversation `thread` selected and a reply refused for its text in its form,
/// answered `422`; the page's own address is the conversation's.
///
/// # Errors
/// The answer for a use case's failure.
pub(crate) async fn refused_reply(
    state: &AppState,
    session: &UiSession,
    thread: ThreadId,
    draft: Draft,
) -> Result<Response, ApiError> {
    let here = Here::at(format!("/ui/inbox/{thread}"));
    render(
        state,
        session,
        &here,
        ListQuery::default(),
        Selected::Thread(thread),
        Some(draft),
    )
    .await
}

/// The list's state from the address's query; a query that does not read is the default state.
fn state_of(query: Result<Query<RawQuery>, QueryRejection>) -> ListQuery {
    ListQuery::of(query.map(|Query(raw)| raw).unwrap_or_default())
}

/// The page in its frame: `200`; `404` when the address selects no conversation the viewer can
/// read; `422` with a refused reply in the form.
async fn render(
    state: &AppState,
    session: &UiSession,
    here: &Here,
    query: ListQuery,
    selected: Selected,
    draft: Option<Draft>,
) -> Result<Response, ApiError> {
    let actor = session.actor();
    let opened = match selected {
        Selected::Thread(thread) => {
            let cursor = latest_event(state, actor).await?;
            match state.app.read_thread.execute(actor, thread).await {
                Ok(view) => {
                    state.app.mark_read.execute(actor, thread).await?;
                    Some((cursor, view))
                }
                Err(AppError::NotFound(_)) => None,
                Err(error) => return Err(error.into()),
            }
        }
        Selected::Nothing | Selected::Unreadable => None,
    };
    let (mut shell, mut loaded, links) = shell::load(state, session, Screen::Inbox, here).await?;
    if let Some((cursor, _)) = &opened {
        shell.after = *cursor;
        loaded.after = *cursor;
    }
    if draft.is_some() {
        shell = shell.answering_a_form();
    }
    let threads = state.app.list_threads.execute(actor, None).await?;
    let summaries: Vec<ConversationSummary> = threads
        .iter()
        .map(|summary| ConversationSummary::of(summary, &links))
        .collect();
    let waiting = waiting_grants(&loaded.unread);
    let none = BTreeMap::new();
    let mut mail = Mailbox {
        threads: &threads,
        summaries: &summaries,
        waiting: &waiting,
        grants: &loaded.unread.every_grant_ask,
        items: &none,
        links: &links,
        now: loaded.now,
    };
    let open = opened.and_then(|(_, view)| {
        let at = threads
            .iter()
            .position(|summary| summary.thread.record.id == view.thread.record.id)?;
        Some((at, view))
    });
    let tab = query
        .tab
        .or_else(|| open.as_ref().map(|(at, _)| Tab::of(&mail, &threads[*at])))
        .unwrap_or(&TABS[0]);
    let items = if tab.names_items {
        notice_items(state, session, &threads, &loaded, &links).await?
    } else {
        BTreeMap::new()
    };
    mail.items = &items;
    let shown = query.showing(tab);
    let current = open.as_ref().map(|(at, _)| summaries[*at].id);
    let list = ListView {
        tabs: mail.tabs(&query, tab),
        tab: (!tab.is_default()).then_some(tab.code),
        q: query.q.clone(),
        close: shown.address(None),
        body: mail.rows(tab, &shown, current),
    };
    let refused = draft.is_some();
    let (inspector, status) = match open {
        Some((at, view)) => {
            let item = item_of(state, session, &view).await?;
            let summary = summaries[at].clone();
            let conversation = Conversation::of(
                &view,
                item.as_ref(),
                actor.member,
                loaded.now,
                &links,
                draft,
            );
            let status = if refused {
                StatusCode::UNPROCESSABLE_ENTITY
            } else {
                StatusCode::OK
            };
            let last = moment(summary.last_at, loaded.now);
            (
                Inspector::Open(Box::new(OpenView {
                    summary,
                    conversation,
                    last,
                })),
                status,
            )
        }
        None => match selected {
            Selected::Nothing => (Inspector::Hint, StatusCode::OK),
            Selected::Thread(_) | Selected::Unreadable => (Inspector::Gone, StatusCode::NOT_FOUND),
        },
    };
    let page = InboxPage {
        shell,
        list,
        inspector,
    };
    Ok(signed_page(session, html(status, &page)))
}

/// The items dibs's notices are about, each with its title, its ticket and its claim's state mark:
/// from the claims and the queue the status shows, else from the project's items.
///
/// # Errors
/// The answer for a use case's failure.
async fn notice_items(
    state: &AppState,
    session: &UiSession,
    threads: &[NamedSummary],
    loaded: &Loaded,
    links: &LinkContext,
) -> Result<BTreeMap<ItemId, ItemFacts>, ApiError> {
    let waiting = waiting_grants(&loaded.unread);
    let wanted: Vec<ItemId> = threads
        .iter()
        .filter(|summary| notice(summary, &waiting))
        .filter_map(|summary| summary.thread.record.item)
        .collect();
    let mut items = BTreeMap::new();
    if wanted.is_empty() {
        return Ok(items);
    }
    let scene = Scene {
        status: &loaded.status,
        viewer: &loaded.me.member.handle,
        now: loaded.now,
        stale_after: loaded.me.settings.stale_after(),
        links,
    };
    for held in loaded.status.live.iter().chain(&loaded.status.stale) {
        let claim = ClaimView::of(held, &scene);
        items.insert(
            claim.id,
            ItemFacts {
                title: Some(
                    claim
                        .title
                        .pieces
                        .iter()
                        .map(|piece| piece.text.as_str())
                        .collect(),
                ),
                ticket: claim.ticket,
                mark: Some(claim.activity.mark()),
            },
        );
    }
    for record in queued(&loaded.status) {
        items
            .entry(record.id)
            .or_insert_with(|| facts(record, links));
    }
    if wanted.iter().any(|id| !items.contains_key(id)) {
        let known = state
            .app
            .list_items
            .execute(session.actor(), ItemQuery::default())
            .await?;
        for named in &known {
            if wanted.contains(&named.item.id) {
                items
                    .entry(named.item.id)
                    .or_insert_with(|| facts(&named.item, links));
            }
        }
    }
    Ok(items)
}

/// The items the queue lists.
fn queued(status: &StatusView) -> impl Iterator<Item = &ItemRecord> {
    crate::ui::queue::listing::listed(status).map(|entry| entry.item)
}

/// An item no claim holds, as a row of From dibs names it.
fn facts(record: &ItemRecord, links: &LinkContext) -> ItemFacts {
    ItemFacts {
        title: Some(record.title.to_string()),
        ticket: TicketView::of(record.id, record.external_ref.as_ref(), links),
        mark: None,
    }
}
