//! Every sentence the guard says, on every host: the brief, the reasons of a refusal, the notes and
//! the notices. They are written for the agent or the person reading them, and name the ways on.

use std::fmt::Write as _;

use time::Duration;

use dibs_domain::area::QualifiedArea;
use dibs_domain::handle::Handle;
use dibs_domain::hot_rule::Note;
use dibs_domain::ids::ItemId;
use dibs_domain::paths::RelPath;
use dibs_domain::settings::EditPolicy;

use crate::ports::{ClaimLine, InboxView, StatusView};

/// How many unread subjects the brief names.
const BRIEF_SUBJECTS: usize = 3;

/// The reason an edit of `path` is refused: another member holds its area.
pub fn held_by(
    path: &RelPath,
    holder: Option<&Handle>,
    item: ItemId,
    area: &QualifiedArea,
) -> String {
    match holder {
        Some(holder) => format!(
            "{path} is in the area {area}, which {holder} holds for work item {item}. Do not edit it now: message {holder} (dibs_send, or dibs send --to {holder}) to agree on the change or ask for a grant, or pick other work."
        ),
        None => format!(
            "{path} is in the area {area}, which another member holds for work item {item}. Do not edit it now: message the holder to agree on the change or ask for a grant, or pick other work."
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
pub fn noted(
    path: &RelPath,
    rule: &str,
    note: Option<&Note>,
    touched_by: &[(Handle, ItemId)],
) -> String {
    let mut text = format!("{path} falls under the {rule} rule");
    if let Some(note) = note {
        let _ = write!(text, ": {note}");
    }
    if !touched_by.is_empty() {
        let others: Vec<String> = touched_by
            .iter()
            .map(|(holder, item)| format!("{holder} (item {item})"))
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
            claim.item, claim.title, claim.holder
        );
        if !claim.areas.is_empty() {
            let areas: Vec<String> = claim.areas.iter().map(ToString::to_string).collect();
            let _ = write!(text, ", areas {}", areas.join(", "));
        }
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
    use dibs_domain::item::Title;

    use super::*;

    fn handle(text: &str) -> Handle {
        Handle::parse(text).unwrap()
    }

    fn claim(item: u64, holder: &str, areas: &[&str]) -> ClaimLine {
        ClaimLine {
            item: ItemId(item),
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
        let text = held_by(&path, Some(&handle("alice")), ItemId(4), &area);
        assert!(text.contains("alice holds for work item 4"), "{text}");
        assert!(text.contains("dibs send --to alice"), "{text}");
        let anonymous = held_by(&path, None, ItemId(4), &area);
        assert!(anonymous.contains("another member"), "{anonymous}");
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
            holds: Vec::new(),
        };
        let inbox = InboxView {
            unread: 4,
            subjects: ["One", "Two", "Three", "Four"]
                .iter()
                .map(|subject| subject.parse().unwrap())
                .collect(),
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
