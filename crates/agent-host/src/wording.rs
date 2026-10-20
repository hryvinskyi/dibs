//! Every sentence the guard says, on every host: the brief, the reasons of a refusal, the notes and
//! the notices. They are written for the agent or the person reading them, and name the ways on.

use std::fmt::Write as _;

use time::Duration;

use dibs_domain::area::QualifiedArea;
use dibs_domain::handle::Handle;
use dibs_domain::hot_rule::Note;
use dibs_domain::ids::ItemId;
use dibs_domain::item::ExternalRef;
use dibs_domain::paths::RelPath;
use dibs_domain::settings::EditPolicy;

use crate::ports::{
    ClaimLine, HeldWay, InboxView, ReadyLine, StatusView, TouchedBy, UnackedLine, WideLine,
};

/// How many unread subjects the brief names.
const BRIEF_SUBJECTS: usize = 3;

/// How many items of the ready list the brief names.
const BRIEF_READY: usize = 5;

/// How many messages awaiting an acknowledgement the brief names.
const BRIEF_UNACKED: usize = 5;

/// How a sentence names an item: by its reference in an outside tracker when it has one, which is
/// what people know it by, else by its local id.
pub fn item_name(item: ItemId, reference: Option<&ExternalRef>) -> String {
    reference.map_or_else(|| item.to_string(), ToString::to_string)
}

/// The reason an edit of `path` is refused: another member holds its area for `item`, named as
/// [`item_name`] names it, and `way` says what the member can do about it.
pub fn held_by(
    path: &RelPath,
    holder: Option<&Handle>,
    item: &str,
    area: &QualifiedArea,
    way: HeldWay,
) -> String {
    match (holder, way) {
        (Some(holder), HeldWay::AskHolder) => format!(
            "{path} is in the area {area}, which {holder} holds for work item {item}. Do not edit it now: message {holder} (dibs_send, or dibs send --to {holder}) to agree on the change or ask for a grant, or pick other work."
        ),
        (None, HeldWay::AskHolder) => format!(
            "{path} is in the area {area}, which another member holds for work item {item}. Do not edit it now: message the holder to agree on the change or ask for a grant, or pick other work."
        ),
        (Some(holder), HeldWay::AwaitingAck) => format!(
            "{path} is in the area {area}, which {holder} holds for work item {item}. Your wide claim names it, and dibs asked {holder} to acknowledge it; their acknowledgement grants the edit. Do not edit it now: go on where the areas are free, or message {holder} (dibs_send, or dibs send --to {holder})."
        ),
        (None, HeldWay::AwaitingAck) => format!(
            "{path} is in the area {area}, which another member holds for work item {item}. Your wide claim names it, and the edit waits for the holder to acknowledge it. Do not edit it now: go on where the areas are free, or message the holder."
        ),
    }
}

/// The reason an edit of `path` is refused: the member holds no live claim.
pub fn needs_claim(path: &RelPath, area: Option<&QualifiedArea>) -> String {
    let how = area.map_or_else(
        || "dibs claim <item> --area <area>".to_owned(),
        |area| format!("dibs claim <item> --area {area}"),
    );
    format!(
        "{path} is coordinated by dibs and you hold no live claim. Claim the work item you are doing first (dibs_claim, or {how}), then edit."
    )
}

/// A hot-file rule's note on `path`.
pub fn noted(path: &RelPath, rule: &str, note: Option<&Note>, touched_by: &[TouchedBy]) -> String {
    let mut text = format!("{path} falls under the {rule} rule");
    if let Some(note) = note {
        let _ = write!(text, ": {note}");
    }
    if !touched_by.is_empty() {
        let others: Vec<String> = touched_by
            .iter()
            .map(|other| {
                format!(
                    "{} (item {})",
                    other.holder,
                    item_name(other.item, other.item_ref.as_ref())
                )
            })
            .collect();
        let _ = write!(text, "; also edited by {}", others.join(", "));
    }
    text.push('.');
    text
}

/// The notice while the server cannot be reached.
pub fn unreachable(sentence: &str) -> String {
    format!(
        "dibs: {} Edits are not coordinated; for the next minute dibs does not ask the server again.",
        sentence.trim()
    )
}

/// A notice for any other failure.
pub fn failed(sentence: &str) -> String {
    format!(
        "dibs: {} Edits are not coordinated until this is fixed.",
        sentence.trim()
    )
}

/// The notice for a host this binary does not support.
pub fn unknown_host(host: &str, known: &str) -> String {
    format!(
        "dibs: there is no agent host '{host}' in this dibs (it knows: {known}); the hook did nothing. Update dibs, or fix the hook's configuration."
    )
}

/// The notice for input the host's adapter could not read.
pub fn unreadable(reason: &str) -> String {
    format!("dibs: {} The hook did nothing.", sentence(reason))
}

/// The notice for background upkeep that could not be started.
pub fn upkeep_failed(sentence_text: &str) -> String {
    format!(
        "dibs: the background refresh could not start: {} Claims may go stale while this lasts.",
        sentence_text.trim()
    )
}

/// The brief at the start of a session.
pub struct Brief<'a> {
    /// The member.
    pub member: &'a Handle,
    /// The project's edit policy.
    pub policy: EditPolicy,
    /// The status, when known.
    pub status: Option<&'a StatusView>,
    /// The inbox, when known.
    pub inbox: Option<&'a InboxView>,
    /// How old the status is, when it is not fresh.
    pub status_age: Option<Duration>,
}

impl Brief<'_> {
    /// The brief as text.
    pub fn text(&self) -> String {
        let mut text = format!("dibs coordinates this checkout; you are {}.", self.member);
        text.push('\n');
        text.push_str(match self.policy {
            EditPolicy::RequireClaim => {
                "Edit policy: claim a work item (with the areas you will change) before editing a governed path."
            }
            EditPolicy::Advise => {
                "Edit policy: advisory; governed paths nobody holds may be edited without a claim, but claim what you work on."
            }
        });
        match self.status {
            Some(status) => self.status_lines(status, &mut text),
            None => text.push_str("\nThe project's claims could not be read just now."),
        }
        if let Some(inbox) = self.inbox {
            inbox_lines(inbox, &mut text);
            unacked_lines(&inbox.unacked, &mut text);
        }
        if let Some(status) = self.status {
            ready_lines(status, &mut text);
        }
        text
    }

    fn status_lines(&self, status: &StatusView, text: &mut String) {
        if let Some(age) = self.status_age {
            let _ = write!(
                text,
                "\nClaims as of {} minutes ago:",
                age.whole_minutes().max(1)
            );
        }
        let (mine, others): (Vec<&ClaimLine>, Vec<&ClaimLine>) = status
            .live
            .iter()
            .partition(|claim| claim.holder == *self.member);
        claim_lines("Your live claims", &mine, text);
        claim_lines("Other members' live claims", &others, text);
        wide_lines(&status.wide, text);
        let stale: Vec<&ClaimLine> = status.stale.iter().collect();
        if !stale.is_empty() {
            claim_lines(
                "Stale claims (taken over only with dibs takeover and a reason)",
                &stale,
                text,
            );
        }
    }
}

fn claim_lines(heading: &str, claims: &[&ClaimLine], text: &mut String) {
    if claims.is_empty() {
        let _ = write!(text, "\n{heading}: none.");
        return;
    }
    let _ = write!(text, "\n{heading}:");
    for claim in claims {
        let _ = write!(
            text,
            "\n- item {} \"{}\", {}",
            item_name(claim.item, claim.reference.as_ref()),
            claim.title,
            claim.holder
        );
        if !claim.areas.is_empty() {
            let areas: Vec<String> = claim.areas.iter().map(ToString::to_string).collect();
            let _ = write!(text, ", areas {}", areas.join(", "));
        }
    }
}

fn wide_lines(wide: &[WideLine], text: &mut String) {
    if wide.is_empty() {
        return;
    }
    text.push_str(
        "\nWide claims (each holds none of its areas; a holder's acknowledgement of its announcement lets it edit theirs):",
    );
    for line in wide {
        let areas: Vec<String> = line
            .areas
            .iter()
            .map(|area| match &area.holder {
                Some(holder) if area.granted => format!("{} (acknowledged by {holder})", area.area),
                Some(holder) => format!("{} (waiting for {holder})", area.area),
                None => format!("{} (free)", area.area),
            })
            .collect();
        let _ = write!(
            text,
            "\n- item {} \"{}\", {}: {}; over {}",
            item_name(line.item, line.reference.as_ref()),
            line.title,
            line.holder,
            line.reason,
            areas.join(", ")
        );
    }
}

fn ready_lines(status: &StatusView, text: &mut String) {
    if status.ready.is_empty() {
        text.push_str("\nReady to pick up: nothing.");
    } else {
        let shown = status.ready.len().min(BRIEF_READY);
        let _ = write!(
            text,
            "\nReady to pick up, soonest first ({shown} of {}):",
            status.ready.len()
        );
        for line in status.ready.iter().take(BRIEF_READY) {
            ready_line(line, text);
        }
    }
    let mut waiting = Vec::new();
    if status.blocked > 0 {
        waiting.push(format!("{} wait for unfinished items", status.blocked));
    }
    if status.area_held > 0 {
        waiting.push(format!(
            "{} touch an area another member holds",
            status.area_held
        ));
    }
    if !waiting.is_empty() {
        let _ = write!(
            text,
            "\nNot ready: {}. dibs_ready or dibs ready says why, item by item.",
            waiting.join("; ")
        );
    }
}

fn ready_line(line: &ReadyLine, text: &mut String) {
    let _ = write!(
        text,
        "\n- {} \"{}\", priority {}",
        item_name(line.item, line.reference.as_ref()),
        line.title,
        line.priority
    );
    if !line.areas_hint.is_empty() {
        let areas: Vec<String> = line.areas_hint.iter().map(ToString::to_string).collect();
        let _ = write!(text, ", likely areas {}", areas.join(", "));
    }
    if let Some(holder) = &line.stale_holder {
        let _ = write!(
            text,
            "; {holder}'s claim on it went stale (dibs takeover with a reason)"
        );
    }
}

fn inbox_lines(inbox: &InboxView, text: &mut String) {
    if inbox.unread == 0 {
        text.push_str("\nNo unread conversations.");
        return;
    }
    let _ = write!(text, "\nUnread conversations: {}", inbox.unread);
    let subjects: Vec<String> = inbox
        .subjects
        .iter()
        .take(BRIEF_SUBJECTS)
        .map(|subject| format!("\"{subject}\""))
        .collect();
    if !subjects.is_empty() {
        let _ = write!(text, ", newest {}", subjects.join(", "));
    }
    text.push_str(". Read them with dibs_inbox or dibs inbox.");
}

fn unacked_lines(unacked: &[UnackedLine], text: &mut String) {
    if unacked.is_empty() {
        return;
    }
    let mut ordered: Vec<&UnackedLine> = unacked.iter().collect();
    ordered.sort_by_key(|line| std::cmp::Reverse(line.importance));
    let shown = ordered.len().min(BRIEF_UNACKED);
    let _ = write!(
        text,
        "\nMessages that ask for your acknowledgement, most pressing first ({shown} of {}):",
        ordered.len()
    );
    for line in ordered.into_iter().take(BRIEF_UNACKED) {
        let _ = write!(
            text,
            "\n- {} from {}: \"{}\" (message {})",
            line.importance, line.sender, line.subject, line.message
        );
    }
    text.push_str(
        "\nRead each (dibs_thread), answer if a decision is needed, then acknowledge it (dibs_ack, or dibs ack <message>). An acknowledgement says you read it; it authorises nothing, except that acknowledging a wide claim's announcement grants its member edits in your areas it names.",
    );
}

/// The text with a capital first letter and a full stop at its end.
fn sentence(text: &str) -> String {
    let mut chars = text.trim().chars();
    let mut out: String = match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    };
    if !out.ends_with(['.', '!', '?']) {
        out.push('.');
    }
    out
}

#[cfg(test)]
mod tests {
    use dibs_domain::ids::MessageId;
    use dibs_domain::item::Title;
    use dibs_domain::message::Importance;

    use super::*;

    fn handle(text: &str) -> Handle {
        Handle::parse(text).unwrap()
    }

    fn claim(item: u64, holder: &str, areas: &[&str]) -> ClaimLine {
        ClaimLine {
            item: ItemId(item),
            reference: None,
            title: Title::parse("Fix billing").unwrap(),
            holder: handle(holder),
            areas: areas
                .iter()
                .map(|area| QualifiedArea::parse(area).unwrap())
                .collect(),
        }
    }

    #[test]
    fn a_held_path_names_the_holder_the_item_and_the_way_on() {
        let path = RelPath::parse("src/billing/a.rs").unwrap();
        let area = QualifiedArea::parse("api:billing").unwrap();
        let reference = ExternalRef::parse("gh:acme/web#12").unwrap();
        let named = item_name(ItemId(1_739_000_000_123), Some(&reference));
        let text = held_by(
            &path,
            Some(&handle("alice")),
            &named,
            &area,
            HeldWay::AskHolder,
        );
        assert!(
            text.contains("alice holds for work item gh:acme/web#12"),
            "{text}"
        );
        assert!(!text.contains("1739000000123"), "{text}");
        assert!(text.contains("dibs send --to alice"), "{text}");
        let local = held_by(
            &path,
            None,
            &item_name(ItemId(4), None),
            &area,
            HeldWay::AskHolder,
        );
        assert!(
            local.contains("another member holds for work item 4"),
            "{local}"
        );
        for holder in [Some(handle("alice")), None] {
            let waiting = held_by(&path, holder.as_ref(), &named, &area, HeldWay::AwaitingAck);
            assert!(waiting.contains("Your wide claim names it"), "{waiting}");
            assert!(waiting.contains("Do not edit it now"), "{waiting}");
            assert!(!waiting.contains("ask for a grant"), "{waiting}");
        }
    }

    #[test]
    fn the_brief_splits_my_claims_from_the_others_and_counts_unread() {
        let me = handle("bob");
        let status = StatusView {
            live: vec![
                claim(3, "bob", &["api:billing"]),
                claim(4, "alice", &["api:search"]),
            ],
            stale: vec![claim(5, "carol", &[])],
            ..StatusView::default()
        };
        let inbox = InboxView {
            unread: 4,
            subjects: ["One", "Two", "Three", "Four"]
                .iter()
                .map(|subject| subject.parse().unwrap())
                .collect(),
            unacked: Vec::new(),
        };
        let text = Brief {
            member: &me,
            policy: EditPolicy::RequireClaim,
            status: Some(&status),
            inbox: Some(&inbox),
            status_age: None,
        }
        .text();
        assert!(
            text.contains("Your live claims:\n- item 3 \"Fix billing\", bob, areas api:billing"),
            "{text}"
        );
        assert!(
            text.contains("Other members' live claims:\n- item 4"),
            "{text}"
        );
        assert!(text.contains("Stale claims"), "{text}");
        assert!(
            text.contains("Unread conversations: 4, newest \"One\", \"Two\", \"Three\"."),
            "{text}"
        );
        assert!(!text.contains("Four"), "{text}");
        assert!(!text.contains("acknowledgement"), "{text}");
    }

    fn unacked(message: u64, sender: &str, subject: &str, importance: Importance) -> UnackedLine {
        UnackedLine {
            message: MessageId(message),
            sender: handle(sender),
            subject: subject.parse().unwrap(),
            importance,
        }
    }

    #[test]
    fn the_brief_names_what_awaits_my_ack_urgent_first_with_sender_and_subject() {
        let me = handle("bob");
        let inbox = InboxView {
            unread: 1,
            subjects: vec!["Staging".parse().unwrap()],
            unacked: vec![
                unacked(21, "carol", "Lunch order", Importance::Normal),
                unacked(20, "alice", "Staging is down", Importance::Urgent),
                unacked(19, "dave", "Rates table", Importance::High),
                unacked(18, "erin", "Freeze", Importance::Urgent),
                unacked(17, "carol", "Old one", Importance::Normal),
                unacked(16, "carol", "Oldest", Importance::Normal),
            ],
        };
        let text = Brief {
            member: &me,
            policy: EditPolicy::RequireClaim,
            status: None,
            inbox: Some(&inbox),
            status_age: None,
        }
        .text();
        assert!(
            text.contains(
                "Messages that ask for your acknowledgement, most pressing first (5 of 6):\n\
                 - urgent from alice: \"Staging is down\" (message 20)\n\
                 - urgent from erin: \"Freeze\" (message 18)\n\
                 - high from dave: \"Rates table\" (message 19)\n\
                 - normal from carol: \"Lunch order\" (message 21)\n\
                 - normal from carol: \"Old one\" (message 17)\n"
            ),
            "{text}"
        );
        assert!(!text.contains("Oldest"), "{text}");
        assert!(text.contains("dibs_ack"), "{text}");
        assert!(text.contains("it authorises nothing"), "{text}");
    }

    #[test]
    fn a_note_names_the_other_editors_items_by_reference() {
        let path = RelPath::parse("Cargo.lock").unwrap();
        let text = noted(
            &path,
            "warn-shared",
            None,
            &[
                TouchedBy {
                    holder: handle("alice"),
                    item: ItemId(1_739_000_000_123),
                    item_ref: Some(ExternalRef::parse("gh:acme/web#12").unwrap()),
                },
                TouchedBy {
                    holder: handle("carol"),
                    item: ItemId(4),
                    item_ref: None,
                },
            ],
        );
        assert_eq!(
            text,
            "Cargo.lock falls under the warn-shared rule; also edited by alice (item gh:acme/web#12), carol (item 4)."
        );
    }

    fn ready(item: u64, reference: &str, priority: u8, stale: Option<&str>) -> ReadyLine {
        ReadyLine {
            item: ItemId(item),
            reference: Some(ExternalRef::parse(reference).unwrap()),
            title: Title::parse("Ready work").unwrap(),
            priority: dibs_domain::item::Priority::new(priority).unwrap(),
            areas_hint: vec![QualifiedArea::parse("api:search").unwrap()],
            stale_holder: stale.map(handle),
        }
    }

    #[test]
    fn the_brief_names_the_top_five_of_the_ready_list_and_why_the_rest_waits() {
        let me = handle("bob");
        let status = StatusView {
            ready: (1..=7)
                .map(|n| {
                    ready(
                        n,
                        &format!("gh:acme/web#{n}"),
                        1,
                        (n == 2).then_some("carol"),
                    )
                })
                .collect(),
            blocked: 2,
            area_held: 1,
            ..StatusView::default()
        };
        let text = Brief {
            member: &me,
            policy: EditPolicy::RequireClaim,
            status: Some(&status),
            inbox: None,
            status_age: None,
        }
        .text();
        assert!(
            text.contains("Ready to pick up, soonest first (5 of 7):\n- gh:acme/web#1 \"Ready work\", priority 1, likely areas api:search"),
            "{text}"
        );
        assert!(
            text.contains("- gh:acme/web#2 \"Ready work\", priority 1, likely areas api:search; carol's claim on it went stale"),
            "{text}"
        );
        assert!(text.contains("gh:acme/web#5"), "{text}");
        assert!(!text.contains("gh:acme/web#6"), "{text}");
        assert!(
            text.contains(
                "Not ready: 2 wait for unfinished items; 1 touch an area another member holds."
            ),
            "{text}"
        );
        let nothing = Brief {
            member: &me,
            policy: EditPolicy::RequireClaim,
            status: Some(&StatusView::default()),
            inbox: None,
            status_age: None,
        }
        .text();
        assert!(nothing.contains("Ready to pick up: nothing."), "{nothing}");
        assert!(!nothing.contains("Not ready"), "{nothing}");
    }

    #[test]
    fn a_brief_without_a_status_says_so() {
        let me = handle("bob");
        let text = Brief {
            member: &me,
            policy: EditPolicy::Advise,
            status: None,
            inbox: None,
            status_age: None,
        }
        .text();
        assert!(text.contains("advisory"), "{text}");
        assert!(text.contains("could not be read"), "{text}");
    }
}
