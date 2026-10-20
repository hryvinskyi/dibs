//! What a call to the server can end in other than its answer, and how a response becomes one of
//! them.
//!
//! A refusal in the server's error shape keeps its stable `code` and the sentence written for the
//! person. A response that is not in that shape — a proxy's error page, a challenge page, an
//! answer whose JSON this client cannot parse — is [`ClientError::Unexpected`]: its body is logged
//! at debug level and never shown, and every caller that fails open treats it like an unreachable
//! server.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use time::OffsetDateTime;

use dibs_domain::area::QualifiedArea;
use dibs_domain::handle::Handle;
use dibs_domain::ids::ItemId;
use dibs_domain::item::{ExternalRef, ItemStatus};
use dibs_domain::repo::{RemoteUrl, RepoId};

/// Why a call did not give its answer.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ClientError {
    /// The server could not be reached: no connection, a broken one, a TLS failure.
    #[error("the dibs server at {server} could not be reached")]
    Network {
        /// The server.
        server: String,
    },
    /// The server did not answer within the client's time limit.
    #[error("the dibs server at {server} did not answer in time")]
    Timeout {
        /// The server.
        server: String,
    },
    /// The token is missing, unknown or revoked.
    #[error("{message}")]
    Unauthorized {
        /// The server's sentence.
        message: String,
    },
    /// The server serves only newer clients than this one (`426`); the sentence says how to
    /// update.
    #[error("{message}")]
    Outdated {
        /// The server's sentence.
        message: String,
    },
    /// Another member is in the way (`409`): the item or an area is held, a claim is not stale.
    #[error("{message}")]
    Conflict {
        /// The stable code (`held`, `area_held`, `not_stale`, …).
        code: String,
        /// The server's sentence.
        message: String,
        /// What the refusal names.
        details: Box<ErrorDetails>,
    },
    /// Any other refusal in the server's error shape.
    #[error("{message}")]
    Refused {
        /// The HTTP status.
        status: u16,
        /// The stable code.
        code: String,
        /// The server's sentence.
        message: String,
        /// What the refusal names.
        details: Box<ErrorDetails>,
    },
    /// An answer this client does not understand.
    #[error(
        "the dibs server at {server} gave an answer this client does not understand (HTTP {status})"
    )]
    Unexpected {
        /// The server.
        server: String,
        /// The HTTP status.
        status: u16,
    },
}

impl ClientError {
    /// The stable code of a refusal, or the kind of failure (`network`, `timeout`, `unexpected`).
    pub fn code(&self) -> &str {
        match self {
            Self::Network { .. } => "network",
            Self::Timeout { .. } => "timeout",
            Self::Unauthorized { .. } => "unauthorized",
            Self::Unexpected { .. } => "unexpected",
            Self::Outdated { .. } => "client_too_old",
            Self::Conflict { code, .. } | Self::Refused { code, .. } => code,
        }
    }

    /// Whether the server could not give a decision: it was unreachable, too slow, failed itself,
    /// answered something this client does not understand, or serves only newer clients. A caller
    /// that must never block work on the service's account treats such a failure as no decision.
    pub fn is_unavailable(&self) -> bool {
        match self {
            Self::Network { .. }
            | Self::Timeout { .. }
            | Self::Unexpected { .. }
            | Self::Outdated { .. } => true,
            Self::Refused { status, .. } => *status >= 500,
            Self::Unauthorized { .. } | Self::Conflict { .. } => false,
        }
    }

    /// What the refusal names, when it is one.
    pub fn details(&self) -> Option<&ErrorDetails> {
        match self {
            Self::Conflict { details, .. } | Self::Refused { details, .. } => Some(details),
            _ => None,
        }
    }

    /// A refusal the client makes itself when the server says nothing exists, such as a reference
    /// no item carries.
    pub(crate) fn not_found(message: String) -> Self {
        Self::Refused {
            status: 404,
            code: "not_found".to_owned(),
            message,
            details: Box::default(),
        }
    }
}

/// What a refusal names that the caller can act on. Every field is optional; the server sends the
/// ones its refusal concerns.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorDetails {
    /// The member in the way.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub holder: Option<Handle>,
    /// The item they hold.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item: Option<ItemId>,
    /// That item's reference, when it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item_ref: Option<ExternalRef>,
    /// The area concerned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub area: Option<QualifiedArea>,
    /// Since when the holder holds it.
    #[serde(
        default,
        with = "time::serde::rfc3339::option",
        skip_serializing_if = "Option::is_none"
    )]
    pub since: Option<OffsetDateTime>,
    /// The holder's last sign of life.
    #[serde(
        default,
        with = "time::serde::rfc3339::option",
        skip_serializing_if = "Option::is_none"
    )]
    pub last_seen: Option<OffsetDateTime>,
    /// The state of a closed item.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<ItemStatus>,
    /// The areas an ambiguous name could mean.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub candidates: Vec<QualifiedArea>,
    /// A repository the project does not list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote: Option<RemoteUrl>,
    /// A repository id the project does not list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<RepoId>,
    /// The request field at fault.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    /// Seconds to wait before trying again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_after: Option<u64>,
}

#[derive(Deserialize)]
struct ErrorBody {
    error: ErrorFields,
}

#[derive(Deserialize)]
struct ErrorFields {
    code: String,
    message: String,
    #[serde(default)]
    details: ErrorDetails,
}

/// A response as the client received it.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Response<'a> {
    /// The server it came from, for the sentences of failures.
    pub server: &'a str,
    /// The HTTP status.
    pub status: u16,
    /// The `Content-Type` header.
    pub content_type: Option<&'a str>,
    /// The body.
    pub body: &'a [u8],
}

/// The answer a response carries, or why it carries none.
///
/// # Errors
/// The [`ClientError`] the response stands for.
pub(crate) fn decode<T: DeserializeOwned>(response: Response<'_>) -> Result<T, ClientError> {
    let unexpected = || {
        tracing::debug!(
            status = response.status,
            content_type = ?response.content_type,
            body = %String::from_utf8_lossy(response.body),
            "an answer is not in a shape this client reads"
        );
        ClientError::Unexpected {
            server: response.server.to_owned(),
            status: response.status,
        }
    };
    if !is_json(response.content_type) {
        return Err(unexpected());
    }
    if (200..300).contains(&response.status) {
        return serde_json::from_slice(response.body).map_err(|_| unexpected());
    }
    let ErrorBody { error } = serde_json::from_slice(response.body).map_err(|_| unexpected())?;
    Err(match response.status {
        401 => ClientError::Unauthorized {
            message: error.message,
        },
        426 => ClientError::Outdated {
            message: error.message,
        },
        409 => ClientError::Conflict {
            code: error.code,
            message: error.message,
            details: Box::new(error.details),
        },
        status => ClientError::Refused {
            status,
            code: error.code,
            message: error.message,
            details: Box::new(error.details),
        },
    })
}

fn is_json(content_type: Option<&str>) -> bool {
    content_type
        .and_then(|value| value.split(';').next())
        .is_some_and(|mime| mime.trim().eq_ignore_ascii_case("application/json"))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::api::Heartbeat;

    const SERVER: &str = "https://dibs.example.com";

    fn response<'a>(status: u16, content_type: Option<&'a str>, body: &'a [u8]) -> Response<'a> {
        Response {
            server: SERVER,
            status,
            content_type,
            body,
        }
    }

    fn error_body(status: u16, code: &str, details: serde_json::Value) -> ClientError {
        let body = json!({"error": {"code": code, "message": "A sentence.", "details": details}})
            .to_string();
        decode::<Heartbeat>(response(status, Some("application/json"), body.as_bytes()))
            .unwrap_err()
    }

    #[test]
    fn a_success_is_parsed_into_its_type() {
        let answer: Heartbeat = decode(response(
            200,
            Some("application/json; charset=utf-8"),
            br#"{"refreshed":[1,2]}"#,
        ))
        .unwrap();
        assert_eq!(answer.refreshed.len(), 2);
    }

    #[test]
    fn an_html_page_is_unexpected_whatever_its_status() {
        for status in [200, 403, 502, 503] {
            let error = decode::<Heartbeat>(response(
                status,
                Some("text/html; charset=UTF-8"),
                b"<html><title>Just a moment...</title></html>",
            ))
            .unwrap_err();
            assert_eq!(
                error,
                ClientError::Unexpected {
                    server: SERVER.to_owned(),
                    status
                }
            );
            assert!(error.is_unavailable());
            assert!(!error.to_string().contains("Just a moment"));
        }
    }

    #[test]
    fn json_outside_the_shapes_is_unexpected() {
        for (status, body) in [
            (200, r#"{"refreshed": "soon"}"#),
            (403, r#"{"message": "denied"}"#),
            (500, "not json"),
        ] {
            let error =
                decode::<Heartbeat>(response(status, Some("application/json"), body.as_bytes()))
                    .unwrap_err();
            assert!(
                matches!(error, ClientError::Unexpected { .. }),
                "{status} {body}"
            );
        }
        let no_type = decode::<Heartbeat>(response(200, None, br#"{"refreshed":[]}"#));
        assert!(matches!(no_type, Err(ClientError::Unexpected { .. })));
    }

    #[test]
    fn refusals_keep_their_code_sentence_and_details() {
        let conflict = error_body(
            409,
            "area_held",
            json!({"area": "api:billing", "holder": "bob", "item": 4}),
        );
        let ClientError::Conflict {
            code,
            message,
            details,
        } = &conflict
        else {
            panic!("{conflict:?}");
        };
        assert_eq!(
            (code.as_str(), message.as_str()),
            ("area_held", "A sentence.")
        );
        assert_eq!(details.holder.as_ref().map(Handle::as_str), Some("bob"));
        assert_eq!(details.item, Some(ItemId(4)));
        assert!(!conflict.is_unavailable());

        let forbidden = error_body(403, "not_holder", json!({"holder": "bob"}));
        assert!(matches!(
            forbidden,
            ClientError::Refused { status: 403, .. }
        ));
        assert_eq!(forbidden.code(), "not_holder");
        assert!(!forbidden.is_unavailable());

        let unauthorized = error_body(401, "unauthorized", json!({}));
        assert_eq!(
            unauthorized,
            ClientError::Unauthorized {
                message: "A sentence.".to_owned()
            }
        );

        let failing = error_body(503, "timeout", json!({}));
        assert!(failing.is_unavailable());
    }

    #[test]
    fn a_server_that_serves_only_newer_clients_gives_no_decision_and_says_how_to_update() {
        let outdated = error_body(426, "client_too_old", json!({"min_client": "0.4.0"}));
        assert_eq!(
            outdated,
            ClientError::Outdated {
                message: "A sentence.".to_owned()
            }
        );
        assert!(outdated.is_unavailable());
        assert_eq!(outdated.code(), "client_too_old");
        let page = decode::<Heartbeat>(response(426, Some("text/html"), b"<html></html>"));
        assert!(matches!(
            page,
            Err(ClientError::Unexpected { status: 426, .. })
        ));
    }
}
