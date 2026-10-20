//! Why a command did not do what it was asked, and the exit status that says so: `1` refused
//! (another member is in the way, a forbidden action), `2` usage or configuration, `3` the server
//! could not be reached or gave no decision.

use std::process::ExitCode;

use dibs_client::{ClientError, ErrorDetails};

/// The class of a failure, which decides the exit status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// The service refused: a conflict, a hold, a forbidden action.
    Refused,
    /// The command line or the configuration is wrong.
    Config,
    /// The server could not be reached, was too slow, or answered something this client does not
    /// understand.
    Unreachable,
}

/// A failure of a command: its class, a stable code, and a sentence for the person.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    /// The class.
    pub kind: Kind,
    /// The stable code.
    pub code: String,
    /// The sentence.
    pub message: String,
    /// What the server's refusal names — the holder, their item, the area — for a caller that
    /// reads them as data; `None` for a failure the server did not refuse.
    pub details: Option<Box<ErrorDetails>>,
    /// Whether the command already printed its answer, so only the sentence is left to print.
    pub answered: bool,
}

impl Failure {
    /// A configuration or usage failure.
    pub fn config(code: &str, message: impl Into<String>) -> Self {
        Self {
            kind: Kind::Config,
            code: code.to_owned(),
            message: message.into(),
            details: None,
            answered: false,
        }
    }

    /// A refusal decided by this client.
    pub fn refused(code: &str, message: impl Into<String>) -> Self {
        Self {
            kind: Kind::Refused,
            code: code.to_owned(),
            message: message.into(),
            details: None,
            answered: false,
        }
    }

    /// The same failure, marked as coming after the command printed its answer.
    #[must_use]
    pub fn after_answer(mut self) -> Self {
        self.answered = true;
        self
    }

    /// The exit status of the class.
    pub fn exit_code(&self) -> ExitCode {
        ExitCode::from(match self.kind {
            Kind::Refused => 1,
            Kind::Config => 2,
            Kind::Unreachable => 3,
        })
    }
}

impl From<ClientError> for Failure {
    fn from(error: ClientError) -> Self {
        let code = error.code().to_owned();
        let kind = if error.is_unavailable() {
            Kind::Unreachable
        } else if matches!(error, ClientError::Unauthorized { .. }) {
            Kind::Config
        } else {
            Kind::Refused
        };
        let mut message = sentence(&error.to_string());
        let details = error.details();
        if let Some(holder) = details.and_then(|details| details.holder.as_ref())
            && !message.contains(holder.as_str())
        {
            if code == "wide_overlap" {
                message.push_str(&format!(" It is in the wide claim of {holder}"));
            } else {
                message.push_str(&format!(" It is held by {holder}"));
            }
            if let Some(details) = details {
                match (&details.item_ref, details.item) {
                    (Some(reference), _) => message.push_str(&format!(" on item {reference}")),
                    (None, Some(item)) => message.push_str(&format!(" on item {item}")),
                    (None, None) => {}
                }
            }
            message.push('.');
        }
        if kind == Kind::Config {
            message.push_str(" Sign in again with dibs login.");
        }
        Self {
            kind,
            code,
            message,
            details: details
                .filter(|details| **details != ErrorDetails::default())
                .map(|details| Box::new(details.clone())),
            answered: false,
        }
    }
}

/// The text with a capital first letter and a full stop at its end.
pub fn sentence(text: &str) -> String {
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
    use dibs_client::ErrorDetails;

    use super::*;

    fn refused(status: u16) -> ClientError {
        ClientError::Refused {
            status,
            code: "x".to_owned(),
            message: "A sentence.".to_owned(),
            details: Box::new(ErrorDetails::default()),
        }
    }

    #[test]
    fn every_client_failure_has_its_exit_status() {
        let server = "https://dibs.example.com".to_owned();
        let cases = [
            (
                ClientError::Network {
                    server: server.clone(),
                },
                3,
            ),
            (
                ClientError::Timeout {
                    server: server.clone(),
                },
                3,
            ),
            (
                ClientError::Unexpected {
                    server,
                    status: 403,
                },
                3,
            ),
            (refused(503), 3),
            (
                ClientError::Unauthorized {
                    message: "The token is not valid.".to_owned(),
                },
                2,
            ),
            (
                ClientError::Conflict {
                    code: "held".to_owned(),
                    message: "Held.".to_owned(),
                    details: Box::default(),
                },
                1,
            ),
            (refused(403), 1),
            (refused(422), 1),
        ];
        for (error, status) in cases {
            let failure = Failure::from(error.clone());
            assert_eq!(failure.exit_code(), ExitCode::from(status), "{error:?}");
            assert!(failure.message.ends_with('.'), "{}", failure.message);
        }
    }

    #[test]
    fn a_conflict_names_the_member_in_the_way() {
        let failure = Failure::from(ClientError::Conflict {
            code: "held".to_owned(),
            message: "Another member holds this item.".to_owned(),
            details: Box::new(ErrorDetails {
                holder: Some("alice".parse().unwrap()),
                item: Some(dibs_domain::ids::ItemId(4)),
                ..ErrorDetails::default()
            }),
        });
        assert_eq!(
            failure.message,
            "Another member holds this item. It is held by alice on item 4."
        );
        assert_eq!(
            failure
                .details
                .as_ref()
                .and_then(|d| d.holder.as_ref())
                .map(|h| h.as_str()),
            Some("alice")
        );
    }

    #[test]
    fn an_overlapping_wide_claim_names_whose_wide_claim_is_in_the_way() {
        let failure = Failure::from(ClientError::Conflict {
            code: "wide_overlap".to_owned(),
            message: "The area api:billing is already in another member's wide claim.".to_owned(),
            details: Box::new(ErrorDetails {
                holder: Some("bob".parse().unwrap()),
                item: Some(dibs_domain::ids::ItemId(9)),
                ..ErrorDetails::default()
            }),
        });
        assert_eq!(
            failure.message,
            "The area api:billing is already in another member's wide claim. It is in the wide claim of bob on item 9."
        );
    }

    #[test]
    fn a_held_area_names_the_holders_item_by_its_reference() {
        let failure = Failure::from(ClientError::Conflict {
            code: "area_held".to_owned(),
            message: "The area api:billing is held by another member.".to_owned(),
            details: Box::new(ErrorDetails {
                holder: Some("alice".parse().unwrap()),
                item: Some(dibs_domain::ids::ItemId(1_739_000_000_012)),
                item_ref: Some(dibs_domain::item::ExternalRef::parse("gh:acme/web#12").unwrap()),
                ..ErrorDetails::default()
            }),
        });
        assert_eq!(
            failure.message,
            "The area api:billing is held by another member. It is held by alice on item gh:acme/web#12."
        );
        assert!(Failure::config("x", "Y.").details.is_none());
    }

    #[test]
    fn a_refused_token_says_how_to_sign_in_again() {
        let failure = Failure::from(ClientError::Unauthorized {
            message: "The token is not valid.".to_owned(),
        });
        assert!(failure.message.ends_with("Sign in again with dibs login."));
    }

    #[test]
    fn sentences_start_high_and_end_with_a_stop() {
        assert_eq!(sentence("the server is away"), "The server is away.");
        assert_eq!(sentence("Done!"), "Done!");
    }
}
