//! How answers are printed: as sentences and lines for a person, or with `--json` as the client's
//! typed answer in JSON.

use std::fmt::Write as _;
use std::io::Write as _;

use serde::Serialize;

use dibs_client::api::{
    Claim, ClaimKind, HeldReason, Item, ItemName, Message, OpenEscalation, OverlapInfo,
    PathVerdict, ThreadSummary, Unacked, UnreadConversations, Verdict,
};
use dibs_domain::handle::Handle;
use dibs_domain::ids::ItemId;
use dibs_domain::radar::OverlapKind;
use dibs_domain::review::PrRef;

use crate::failure::Failure;

/// Prints answers in the form asked for.
#[derive(Debug, Clone, Copy)]
pub struct Printer {
    json: bool,
    terminal: bool,
}

/// Where the text of an answer that hands over a secret goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretShown {
    /// What is printed on standard output.
    pub out: String,
    /// What is printed on standard error, if anything.
    pub err: Option<String>,
}

/// An answer that hands over a secret, as a person reads it: at a terminal, the sentence and the
/// secret on the line after it; otherwise the secret alone on standard output and the sentence on
/// standard error, so that a pipe or `$(…)` takes exactly the secret.
pub fn secret_shown(terminal: bool, sentence: &str, secret: &str) -> SecretShown {
    if terminal {
        SecretShown {
            out: format!("{sentence}:\n{secret}"),
            err: None,
        }
    } else {
        SecretShown {
            out: secret.to_owned(),
            err: Some(format!("{sentence}.")),
        }
    }
}

impl Printer {
    /// A printer of JSON when `json`, of text otherwise; `terminal` says whether standard output
    /// is a terminal.
    pub fn new(json: bool, terminal: bool) -> Self {
        Self { json, terminal }
    }

    /// Whether it prints JSON.
    pub fn is_json(self) -> bool {
        self.json
    }

    /// Prints `value` as JSON, or the text `human` writes for it.
    pub fn show<T: Serialize>(self, value: &T, human: impl FnOnce() -> String) {
        if self.json {
            match serde_json::to_string_pretty(value) {
                Ok(text) => println!("{text}"),
                Err(error) => tracing::debug!(error = %error, "an answer could not be written"),
            }
        } else {
            let text = human();
            if !text.is_empty() {
                println!("{}", text.trim_end());
            }
        }
    }

    /// Prints an answer that hands over `secret`: as JSON, or as [`secret_shown`] says, `sentence`
    /// being what it says about the secret, without a closing full stop.
    ///
    /// # Errors
    /// When standard output cannot take it, a closed pipe say: the secret did not reach anyone.
    pub fn show_secret<T: Serialize>(
        self,
        value: &T,
        sentence: &str,
        secret: &str,
    ) -> std::io::Result<()> {
        let out = if self.json {
            serde_json::to_string_pretty(value).map_err(std::io::Error::other)?
        } else {
            let shown = secret_shown(self.terminal, sentence, secret);
            if let Some(err) = &shown.err {
                eprintln!("{err}");
            }
            shown.out
        };
        let mut stdout = std::io::stdout().lock();
        writeln!(stdout, "{out}")?;
        stdout.flush()
    }

    /// Prints a failure: the sentence on standard error, and with `--json`, unless the command
    /// already printed its answer, its code, its sentence and what the refusal names on standard
    /// output.
    pub fn failure(self, failure: &Failure) {
        eprintln!("dibs: {}", failure.message);
        if self.json && !failure.answered {
            println!("{}", failure_json(failure));
        }
    }

    /// Prints a warning on standard error.
    pub fn warn(self, text: &str) {
        eprintln!("warning: {text}");
    }

    /// Prints what dibs is doing on standard error, out of the way of a command's own output.
    pub fn note(self, text: &str) {
        eprintln!("dibs: {text}");
    }
}

/// A failure as `--json` prints it: `{"error": {"code", "message", "details"?}}`, `details` being
/// what the server's refusal names.
pub fn failure_json(failure: &Failure) -> serde_json::Value {
    let mut error = serde_json::Map::new();
    error.insert("code".to_owned(), failure.code.clone().into());
    error.insert("message".to_owned(), failure.message.clone().into());
    if let Some(details) = &failure.details
        && let Ok(value) = serde_json::to_value(details)
    {
        error.insert("details".to_owned(), value);
    }
    serde_json::json!({ "error": error })
}

/// One line for an item: `#12 gh:acme/api#4 Fix billing [claimed]`.
pub fn item_line(item: &Item) -> String {
    let mut line = format!("#{}", item.id);
    if let Some(reference) = &item.external_ref {
        let _ = write!(line, " {reference}");
    }
    let status = serde_json::to_value(item.status)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default();
    let _ = write!(line, " {} [{status}]", item.title);
    line
}

/// How a line names an item an answer mentions: by its reference when it has one, else `#<id>`.
/// An answer from a server that does not name the item falls back to the id.
pub fn item_label(named: Option<&ItemName>, id: ItemId) -> String {
    named.map_or_else(|| format!("#{id}"), ItemName::label)
}

/// What a claim holds, and the branch its work is on with where that was learned: `alice on
/// laptop, areas api:billing, on branch api:alice/42-refunds (named)`.
pub fn claim_text(claim: &Claim) -> String {
    let mut text = format!("{} on {}", claim.holder, claim.host);
    if claim.kind == ClaimKind::Debt {
        text.push_str(", debt work");
    }
    if !claim.areas.is_empty() {
        let areas: Vec<String> = claim.areas.iter().map(ToString::to_string).collect();
        let _ = write!(text, ", areas {}", areas.join(", "));
    }
    if !claim.pending.is_empty() {
        let waiting: Vec<String> = claim.pending.keys().map(ToString::to_string).collect();
        let _ = write!(text, ", waiting for {}", waiting.join(", "));
    }
    if let Some(review) = &claim.review {
        let prs: Vec<String> = review.prs.iter().map(ToString::to_string).collect();
        let _ = write!(text, ", in review on {}", prs.join(", "));
    }
    if !claim.branches.is_empty() {
        let branches: Vec<String> = claim
            .branches
            .iter()
            .map(|(repo, branch)| {
                let source = claim
                    .branch_sources
                    .get(repo)
                    .and_then(|source| source.words());
                match source {
                    Some(words) => format!("{repo}:{branch} ({words})"),
                    None => format!("{repo}:{branch}"),
                }
            })
            .collect();
        let noun = if branches.len() == 1 {
            "branch"
        } else {
            "branches"
        };
        let _ = write!(text, ", on {noun} {}", branches.join(", "));
    }
    text
}

/// A time of day in UTC, `HH:MM UTC`.
fn clock(at: time::OffsetDateTime) -> String {
    let at = at.to_offset(time::UtcOffset::UTC);
    format!("{:02}:{:02} UTC", at.hour(), at.minute())
}

/// One line for a verdict: `src/billing/a.rs: held by bob (item 3, area api:billing)`.
pub fn verdict_line(verdict: &PathVerdict) -> String {
    let what = match &verdict.verdict {
        Verdict::Allowed => "allowed".to_owned(),
        Verdict::AllowedWithNote {
            rule,
            note,
            touched_by,
        } => {
            let mut text = format!("allowed under the {} rule", rule.0);
            if let Some(note) = note {
                let _ = write!(text, ": {note}");
            }
            if !touched_by.is_empty() {
                let others: Vec<String> = touched_by
                    .iter()
                    .map(|other| format!("{} on item {}", other.holder, other.item_label()))
                    .collect();
                let _ = write!(text, " (also edited by {})", others.join(", "));
            }
            text
        }
        Verdict::NeedsClaim => "needs a claim first; claim an item with dibs claim".to_owned(),
        Verdict::HeldBy {
            holder,
            item,
            item_ref,
            area,
            reason,
            prs,
            claim,
            waiting,
            escalates_at,
        } => {
            let who = holder
                .as_ref()
                .map_or_else(|| "another member".to_owned(), ToString::to_string);
            let item = item_ref
                .as_ref()
                .map_or_else(|| item.to_string(), ToString::to_string);
            let way = match reason {
                HeldReason::InReview => {
                    let prs: Vec<String> = prs.iter().map(ToString::to_string).collect();
                    format!("in review on {}; free when it merges", prs.join(", "))
                }
                HeldReason::AwaitingAck => format!(
                    "your wide claim names it; waiting for {who}'s acknowledgement, which grants the edit"
                ),
                HeldReason::AwaitingHandover => match escalates_at {
                    Some(at) => format!(
                        "your claim waits for {who} to hand it over; it goes to the arbiters at {}",
                        clock(*at)
                    ),
                    None => format!("your claim waits for {who} to hand it over"),
                },
                HeldReason::AskHandover => match claim {
                    Some(claim) => format!(
                        "debt work; dibs claim {claim} --area {area} asks {who} to hand it over"
                    ),
                    None => format!(
                        "ask them for a grant with dibs ask-grant <your item> --area {area}, or message them"
                    ),
                },
                HeldReason::AskHolder => match waiting {
                    Some(waiting) => format!(
                        "{}'s claim on item {} already waits for it; message {who}",
                        waiting.holder,
                        waiting.item_label()
                    ),
                    None => format!(
                        "ask them for a grant with dibs ask-grant <your item> --area {area}, or message them"
                    ),
                },
                HeldReason::Other => format!(
                    "ask them for a grant with dibs ask-grant <your item> --area {area}, or message them"
                ),
            };
            format!("held by {who} (item {item}, area {area}); {way}")
        }
        Verdict::Granted { by, area } => {
            let who = by
                .as_ref()
                .map_or_else(|| "its holder".to_owned(), ToString::to_string);
            format!("granted by {who} (area {area})")
        }
    };
    format!("{}: {what}", verdict.path)
}

/// One line for a conversation:
/// `#4 Billing rates (from alice, urgent, 2 unread from people, 5 from dibs, 1 to ack)`. A server
/// that does not tell people's unread messages from dibs's gives only `2 unread`.
pub fn thread_line(summary: &ThreadSummary) -> String {
    let thread = &summary.thread;
    let mut line = format!("#{} {} (from {}", thread.id, thread.subject, thread.author);
    if summary.importance.is_raised() {
        let _ = write!(line, ", {}", summary.importance);
    }
    let unread = match (summary.unread_from_people, summary.unread_from_dibs) {
        (Some(people), Some(dibs)) => match (people, dibs) {
            (0, 0) => None,
            (people, 0) => Some(format!("{people} unread from people")),
            (0, dibs) => Some(format!("{dibs} unread from dibs")),
            (people, dibs) => Some(format!("{people} unread from people, {dibs} from dibs")),
        },
        _ => (summary.unread > 0).then(|| format!("{} unread", summary.unread)),
    };
    if let Some(unread) = unread {
        let _ = write!(line, ", {unread}");
    }
    if summary.awaiting_ack > 0 {
        let _ = write!(line, ", {} to ack", summary.awaiting_ack);
    }
    line.push(')');
    line
}

/// The first line of the inbox: how many conversations have unread messages from people and how
/// many from dibs, with the way to mark dibs's read.
pub fn unread_line(unread: UnreadConversations) -> String {
    format!(
        "Unread from people: {}. Unread from dibs: {} (dibs read --dibs marks them read).",
        conversations(unread.from_people),
        conversations(unread.from_dibs)
    )
}

/// `n` conversations, in words: `1 conversation`, `3 conversations`.
fn conversations(n: u64) -> String {
    if n == 1 {
        "1 conversation".to_owned()
    } else {
        format!("{n} conversations")
    }
}

/// One line for a message awaiting the caller's acknowledgement:
/// `- message 12 from alice in #4 Staging is down [urgent]; dibs ack 12`.
pub fn unacked_line(unacked: &Unacked) -> String {
    let message = &unacked.message;
    let mut line = format!(
        "- message {} from {} in #{} {}",
        message.id, message.author, unacked.thread.id, unacked.thread.subject
    );
    if message.importance.is_raised() {
        let _ = write!(line, " [{}]", message.importance);
    }
    let _ = write!(line, "; dibs ack {}", message.id);
    line
}

/// Who a message is from, as a conversation shows it: `alice (laptop/cli)`, or `dibs` for what
/// dibs wrote in the project's name, which comes from no session.
pub fn author_text(message: &Message) -> String {
    if message.system {
        "dibs".to_owned()
    } else {
        format!("{} ({})", message.author, message.label)
    }
}

/// One line for an escalation waiting for an arbiter:
/// `Escalated by bob: work item gh:acme/api#7 "Invoices" — waiting for carol (conversation #31)`,
/// or `Escalated by dibs: …` for one dibs raised on its own.
pub fn escalation_line(escalation: &OpenEscalation) -> String {
    let arbiters: Vec<&str> = escalation.arbiters.iter().map(Handle::as_str).collect();
    let by = if escalation.raised_by_dibs {
        "dibs"
    } else {
        escalation.by.as_str()
    };
    format!(
        "Escalated by {by}: {} — waiting for {} (conversation #{})",
        escalation.about,
        arbiters.join(", "),
        escalation.thread_id
    )
}

/// What a claim line adds for the merges that moved the base under the claim's branch: `, the base
/// moved under it: acme/widgets#7, acme/widgets#9 (rebase onto the base when you next work on it)`, or nothing when no
/// merge did.
pub fn base_moved_text(prs: &[PrRef]) -> String {
    if prs.is_empty() {
        return String::new();
    }
    let names: Vec<String> = prs.iter().map(ToString::to_string).collect();
    format!(
        ", the base moved under it: {} (rebase onto the base when you next work on it)",
        names.join(", ")
    )
}

/// One line for an overlap between live claims:
/// `same file: gh:acme/api#1 (alice) and gh:acme/api#2 (bob) on api:config/app.yaml`, the paths
/// after the first five counted.
pub fn overlap_line(overlap: &OverlapInfo) -> String {
    const NAMED: usize = 5;
    let parties: Vec<String> = overlap
        .items
        .iter()
        .map(|party| {
            let name = party
                .external_ref
                .as_ref()
                .map_or_else(|| format!("#{}", party.id), ToString::to_string);
            format!("{name} ({})", party.holder)
        })
        .collect();
    let mut paths: Vec<String> = overlap
        .paths
        .iter()
        .take(NAMED)
        .map(ToString::to_string)
        .collect();
    let more = overlap.paths.len().saturating_sub(NAMED);
    if more > 0 {
        paths.push(format!("{more} more"));
    }
    let kind = match overlap.kind {
        OverlapKind::SameFile => "same file".to_owned(),
        OverlapKind::Contract => "contract".to_owned(),
        OverlapKind::MainMoved => format!(
            "base moved by {}",
            overlap
                .pr
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_default()
        ),
    };
    format!("{kind}: {} on {}", parties.join(" and "), paths.join(", "))
}

/// What a sent message asks of its recipients, as a sentence to follow the line that sent it;
/// empty for an ordinary message.
pub fn asking_text(message: &Message) -> String {
    match (message.importance.is_raised(), message.ack_required) {
        (false, false) => String::new(),
        (true, false) => format!(" It is marked {}.", message.importance),
        (false, true) => " Each recipient is asked to acknowledge it.".to_owned(),
        (true, true) => format!(
            " It is marked {}, and each recipient is asked to acknowledge it.",
            message.importance
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn at_a_terminal_the_secret_follows_its_sentence_on_standard_output() {
        let shown = secret_shown(true, "Added bob; hand it over", "dibs_secret");
        assert_eq!(shown.out, "Added bob; hand it over:\ndibs_secret");
        assert_eq!(shown.err, None);
    }

    #[test]
    fn a_claim_line_names_every_merge_that_moved_its_base_and_nothing_without_one() {
        let prs = [
            PrRef::parse("acme/widgets#7").unwrap(),
            PrRef::parse("acme/widgets#9").unwrap(),
        ];
        assert_eq!(
            base_moved_text(&prs),
            ", the base moved under it: acme/widgets#7, acme/widgets#9 (rebase onto the base when you next work on it)"
        );
        assert_eq!(base_moved_text(&[]), "");
    }

    /// An escalation recorded for bob, as the status answers it, with `more` merged in.
    fn escalation(more: serde_json::Value) -> OpenEscalation {
        let mut escalation = serde_json::json!({
            "subject": {"item": 7}, "about": "work item gh:acme/api#7 \"Invoices\"", "by": "bob",
            "arbiters": ["carol", "dave"], "thread_id": 31, "message_id": 32,
            "at": "2026-03-02T09:00:00Z"
        });
        if let (Some(escalation), Some(more)) = (escalation.as_object_mut(), more.as_object()) {
            escalation.extend(more.clone());
        }
        serde_json::from_value(escalation).unwrap()
    }

    #[test]
    fn an_escalation_line_names_who_asked_and_dibs_for_one_it_raised_on_its_own() {
        let asked = "Escalated by bob: work item gh:acme/api#7 \"Invoices\" — waiting for carol, dave (conversation #31)";
        assert_eq!(
            escalation_line(&escalation(serde_json::json!({"raised_by_dibs": false}))),
            asked
        );
        assert_eq!(
            escalation_line(&escalation(serde_json::json!({}))),
            asked,
            "a server that does not say raised none"
        );
        assert_eq!(
            escalation_line(&escalation(serde_json::json!({"raised_by_dibs": true}))),
            "Escalated by dibs: work item gh:acme/api#7 \"Invoices\" — waiting for carol, dave (conversation #31)"
        );
    }

    /// A conversation alice started, with `unread` unread messages and `split` merged in.
    fn summary(unread: u64, split: serde_json::Value) -> ThreadSummary {
        let mut summary = serde_json::json!({
            "thread": {
                "id": 4, "subject": "Billing rates", "item": null, "author": "alice",
                "to_project": false, "to": ["bob"], "created_at": "2026-03-02T09:00:00Z"
            },
            "messages": 9,
            "unread": unread,
            "last_message_at": "2026-03-02T10:00:00Z",
            "importance": "urgent",
            "awaiting_ack": 1,
        });
        if let (Some(summary), Some(split)) = (summary.as_object_mut(), split.as_object()) {
            summary.extend(split.clone());
        }
        serde_json::from_value(summary).unwrap()
    }

    #[test]
    fn a_conversation_line_splits_its_unread_messages_into_peoples_and_dibss() {
        let split = |people: u64, dibs: u64| serde_json::json!({"unread_from_people": people, "unread_from_dibs": dibs});
        assert_eq!(
            thread_line(&summary(7, split(2, 5))),
            "#4 Billing rates (from alice, urgent, 2 unread from people, 5 from dibs, 1 to ack)"
        );
        assert_eq!(
            thread_line(&summary(2, split(2, 0))),
            "#4 Billing rates (from alice, urgent, 2 unread from people, 1 to ack)"
        );
        assert_eq!(
            thread_line(&summary(5, split(0, 5))),
            "#4 Billing rates (from alice, urgent, 5 unread from dibs, 1 to ack)"
        );
        assert_eq!(
            thread_line(&summary(0, split(0, 0))),
            "#4 Billing rates (from alice, urgent, 1 to ack)"
        );
        assert_eq!(
            thread_line(&summary(3, serde_json::json!({}))),
            "#4 Billing rates (from alice, urgent, 3 unread, 1 to ack)",
            "a server that does not split them gives the total"
        );
    }

    #[test]
    fn the_inbox_opens_with_the_conversations_unread_from_people_and_from_dibs() {
        assert_eq!(
            unread_line(UnreadConversations {
                from_people: 1,
                from_dibs: 3,
            }),
            "Unread from people: 1 conversation. Unread from dibs: 3 conversations (dibs read --dibs marks them read)."
        );
    }

    #[test]
    fn into_a_pipe_the_secret_goes_alone_and_its_sentence_to_standard_error() {
        let shown = secret_shown(false, "Added bob; hand it over", "dibs_secret");
        assert_eq!(shown.out, "dibs_secret");
        assert_eq!(shown.err.as_deref(), Some("Added bob; hand it over."));
    }
}
