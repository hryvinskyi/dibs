//! How answers are printed: as sentences and lines for a person, or with `--json` as the client's
//! typed answer in JSON.

use std::fmt::Write as _;

use serde::Serialize;

use dibs_client::api::{
    Claim, HeldReason, Item, ItemName, Message, PathVerdict, ThreadSummary, Unacked, Verdict,
};
use dibs_domain::ids::ItemId;

use crate::failure::Failure;

/// Prints answers in the form asked for.
#[derive(Debug, Clone, Copy)]
pub struct Printer {
    json: bool,
}

impl Printer {
    /// A printer of JSON when `json`, of text otherwise.
    pub fn new(json: bool) -> Self {
        Self { json }
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

/// What a claim holds: `alice on laptop, areas api:billing`.
pub fn claim_text(claim: &Claim) -> String {
    let mut text = format!("{} on {}", claim.holder, claim.host);
    if !claim.areas.is_empty() {
        let areas: Vec<String> = claim.areas.iter().map(ToString::to_string).collect();
        let _ = write!(text, ", areas {}", areas.join(", "));
    }
    text
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
        } => {
            let who = holder
                .as_ref()
                .map_or_else(|| "another member".to_owned(), ToString::to_string);
            let item = item_ref
                .as_ref()
                .map_or_else(|| item.to_string(), ToString::to_string);
            let way = match reason {
                HeldReason::AwaitingAck => format!(
                    "your wide claim names it; waiting for {who}'s acknowledgement, which grants the edit"
                ),
                HeldReason::AskHolder | HeldReason::Other => {
                    "message them or ask for a grant".to_owned()
                }
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

/// One line for a conversation: `#4 Billing rates (from alice, urgent, 2 unread, 1 to ack)`.
pub fn thread_line(summary: &ThreadSummary) -> String {
    let thread = &summary.thread;
    let mut line = format!("#{} {} (from {}", thread.id, thread.subject, thread.author);
    if summary.importance.is_raised() {
        let _ = write!(line, ", {}", summary.importance);
    }
    if summary.unread > 0 {
        let _ = write!(line, ", {} unread", summary.unread);
    }
    if summary.awaiting_ack > 0 {
        let _ = write!(line, ", {} to ack", summary.awaiting_ack);
    }
    line.push(')');
    line
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
