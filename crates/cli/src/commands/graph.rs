//! The work graph: `ready`, the work you can pick up now, and `dep add` / `dep rm`, recording that
//! one item is blocked by another.

use std::fmt::Write as _;

use dibs_client::api::{DependencyChange, ExcludedItem, Item, NotReady, ReadyItem, ReadyList};

use crate::cli::{DepArgs, DepCommand};
use crate::context::Context;
use crate::failure::Failure;

/// Shows the caller's ready list, soonest first, and the open items that are not ready with why.
///
/// # Errors
/// The [`Failure`] of the call.
pub async fn ready(ctx: &Context) -> Result<(), Failure> {
    let connection = ctx.connect().await?;
    let list = connection
        .client
        .ready()
        .await
        .map_err(|error| connection.failed(error))?;
    ctx.printer.show(&list, || ready_text(&list));
    Ok(())
}

/// Records or removes that an item is blocked by another.
///
/// # Errors
/// A refusal for an edge that would make the two items wait for each other; the [`Failure`] of
/// the call.
pub async fn dep(command: DepCommand, ctx: &Context) -> Result<(), Failure> {
    let connection = ctx.connect().await?;
    let (args, adding): (DepArgs, bool) = match command {
        DepCommand::Add(args) => (args, true),
        DepCommand::Rm(args) => (args, false),
    };
    let item = connection.resolve(&args.key).await?;
    let blocked_by = connection.resolve(&args.blocked_by).await?;
    let call = if adding {
        connection.client.add_dependency(item, blocked_by).await
    } else {
        connection.client.remove_dependency(item, blocked_by).await
    };
    let change = call.map_err(|error| connection.failed(error))?;
    ctx.printer.show(&change, || dep_text(&change, adding));
    Ok(())
}

fn dep_text(change: &DependencyChange, adding: bool) -> String {
    let item = name(&change.item);
    let blocker = name(&change.blocked_by);
    match (adding, change.changed) {
        (true, true) => format!("{item} is now blocked by {blocker}."),
        (true, false) => format!("{item} was already blocked by {blocker}."),
        (false, true) => format!("{item} is no longer blocked by {blocker}."),
        (false, false) => format!("{item} was not blocked by {blocker}."),
    }
}

/// How an item is named to a person: its reference, or `#<id>` for a local item.
fn name(item: &Item) -> String {
    item.external_ref
        .as_ref()
        .map_or_else(|| format!("#{}", item.id), ToString::to_string)
}

/// The ready list as lines for a person.
pub fn ready_text(list: &ReadyList) -> String {
    let mut text = String::new();
    if list.ready.is_empty() {
        text.push_str("Nothing is ready to pick up.\n");
    } else {
        text.push_str("Ready to pick up, soonest first:\n");
        for entry in &list.ready {
            let _ = writeln!(text, "  {}", ready_line(entry));
        }
    }
    let waiting: Vec<&ExcludedItem> = list
        .excluded
        .iter()
        .filter(|entry| {
            !entry
                .reasons
                .iter()
                .any(|reason| matches!(reason, NotReady::Claimed { .. }))
        })
        .collect();
    if !waiting.is_empty() {
        text.push_str("Not ready:\n");
        for entry in waiting {
            let reasons: Vec<String> = entry.reasons.iter().filter_map(reason_text).collect();
            let _ = write!(
                text,
                "  P{} {} {}",
                entry.item.priority,
                name(&entry.item),
                entry.item.title
            );
            if !reasons.is_empty() {
                let _ = write!(text, " — {}", reasons.join("; "));
            }
            text.push('\n');
        }
    }
    text
}

fn ready_line(entry: &ReadyItem) -> String {
    let mut line = format!(
        "P{} {} {}",
        entry.item.priority,
        name(&entry.item),
        entry.item.title
    );
    if !entry.item.areas_hint.is_empty() {
        let areas: Vec<String> = entry
            .item
            .areas_hint
            .iter()
            .map(ToString::to_string)
            .collect();
        let _ = write!(line, " — likely areas {}", areas.join(", "));
    }
    if let Some(holder) = &entry.stale_holder {
        let _ = write!(
            line,
            "; {holder}'s claim on it went stale (dibs takeover with a reason)"
        );
    }
    line
}

fn reason_text(reason: &NotReady) -> Option<String> {
    match reason {
        NotReady::BlockedBy { items } => {
            let names: Vec<String> = items.iter().map(|item| item.label()).collect();
            Some(format!("blocked by {}", names.join(", ")))
        }
        NotReady::AreaHeld { area, holder, item } => {
            Some(format!("{area} is held by {holder} for {}", item.label()))
        }
        NotReady::Claimed { holder } => Some(format!("claimed by {holder}")),
        NotReady::Other => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(json: serde_json::Value) -> ReadyList {
        serde_json::from_value(json).unwrap()
    }

    fn item(id: u64, reference: Option<&str>, priority: u8, hint: &[&str]) -> serde_json::Value {
        serde_json::json!({
            "id": id, "ref": reference, "title": format!("Work {id}"), "status": "open",
            "repo": null, "unlisted_remote": null, "priority": priority, "areas_hint": hint,
            "source_status": null, "closed": false, "source": "board", "created_by": "board",
            "created_at": "2026-03-02T09:00:00Z"
        })
    }

    #[test]
    fn the_ready_list_reads_soonest_first_with_the_reasons_of_the_rest() {
        let text = ready_text(&list(serde_json::json!({
            "ready": [
                {"item": item(1, Some("gh:acme/web#1"), 0, &["api:search"]), "link": null, "stale_holder": null},
                {"item": item(2, None, 2, &[]), "link": null, "stale_holder": "carol"}
            ],
            "excluded": [
                {"item": item(3, Some("gh:acme/web#3"), 1, &[]), "link": null, "reasons": [
                    {"reason": "blocked_by", "items": [{"id": 1, "ref": "gh:acme/web#1", "title": "One", "link": null}]},
                    {"reason": "area_held", "area": "api:billing", "holder": "alice",
                     "item": {"id": 9, "ref": null, "title": "Nine", "link": null}}
                ]},
                {"item": item(4, None, 1, &[]), "link": null, "reasons": [{"reason": "claimed", "holder": "bob"}]},
                {"item": item(5, None, 1, &[]), "link": null, "reasons": [{"reason": "something_new"}]}
            ]
        })));
        assert_eq!(
            text,
            "Ready to pick up, soonest first:\n\
             \x20 P0 gh:acme/web#1 Work 1 — likely areas api:search\n\
             \x20 P2 #2 Work 2; carol's claim on it went stale (dibs takeover with a reason)\n\
             Not ready:\n\
             \x20 P1 gh:acme/web#3 Work 3 — blocked by gh:acme/web#1; api:billing is held by alice for #9\n\
             \x20 P1 #5 Work 5\n"
        );
        assert_eq!(
            ready_text(&ReadyList::default()),
            "Nothing is ready to pick up.\n"
        );
    }
}
