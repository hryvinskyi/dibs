//! How answers are printed: as sentences and lines for a person, or with `--json` as the client's
//! typed answer in JSON.

use std::fmt::Write as _;
use std::io::Write as _;

use serde::Serialize;

use dibs_client::api::{
    Claim, ClaimKind, HeldReason, Item, ItemName, Message, OpenEscalation, OverlapInfo,
    PathVerdict, ThreadSummary, Unacked, Verdict,
};
use dibs_domain::handle::Handle;
use dibs_domain::ids::ItemId;
use dibs_domain::radar::OverlapKind;

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

/// What a claim holds: `alice on laptop, areas api:billing`.
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
                    None => "message them or ask for a grant".to_owned(),
                },
                HeldReason::AskHolder => match waiting {
                    Some(waiting) => format!(
                        "{}'s claim on item {} already waits for it; message {who}",
                        waiting.holder,
                        waiting.item_label()
                    ),
                    None => "message them or ask for a grant".to_owned(),
                },
                HeldReason::Other => "message them or ask for a grant".to_owned(),
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
/// `Escalated by bob: work item gh:acme/api#7 "Invoices" — waiting for carol (conversation #31)`.
pub fn escalation_line(escalation: &OpenEscalation) -> String {
    let arbiters: Vec<&str> = escalation.arbiters.iter().map(Handle::as_str).collect();
    format!(
        "Escalated by {}: {} — waiting for {} (conversation #{})",
        escalation.by,
        escalation.about,
        arbiters.join(", "),
        escalation.thread_id
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
    fn into_a_pipe_the_secret_goes_alone_and_its_sentence_to_standard_error() {
        let shown = secret_shown(false, "Added bob; hand it over", "dibs_secret");
        assert_eq!(shown.out, "dibs_secret");
        assert_eq!(shown.err.as_deref(), Some("Added bob; hand it over."));
    }
}
