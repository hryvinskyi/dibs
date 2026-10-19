//! How answers are printed: as sentences and lines for a person, or with `--json` as the client's
//! typed answer in JSON.

use std::fmt::Write as _;

use serde::Serialize;

use dibs_client::api::{Claim, Item, PathVerdict, ThreadSummary, Verdict};

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
    /// already printed its answer, its code and sentence on standard output.
    pub fn failure(self, failure: &Failure) {
        eprintln!("dibs: {}", failure.message);
        if self.json && !failure.answered {
            let body = serde_json::json!({
                "error": {"code": failure.code, "message": failure.message}
            });
            println!("{body}");
        }
    }

    /// Prints a warning on standard error.
    pub fn warn(self, text: &str) {
        eprintln!("warning: {text}");
    }
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
                    .map(|other| format!("{} on item {}", other.holder, other.item))
                    .collect();
                let _ = write!(text, " (also edited by {})", others.join(", "));
            }
            text
        }
        Verdict::NeedsClaim => "needs a claim first; claim an item with dibs claim".to_owned(),
        Verdict::HeldBy {
            holder, item, area, ..
        } => {
            let who = holder
                .as_ref()
                .map_or_else(|| "another member".to_owned(), ToString::to_string);
            format!("held by {who} (item {item}, area {area}); message them or ask for a grant")
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

/// One line for a conversation: `#4 Billing rates (from alice, 2 unread)`.
pub fn thread_line(summary: &ThreadSummary) -> String {
    let thread = &summary.thread;
    let mut line = format!("#{} {} (from {}", thread.id, thread.subject, thread.author);
    if summary.unread > 0 {
        let _ = write!(line, ", {} unread", summary.unread);
    }
    line.push(')');
    line
}
