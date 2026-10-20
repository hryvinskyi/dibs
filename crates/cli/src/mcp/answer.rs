//! How an operation's outcome becomes a tool result: the client's JSON when it worked, and a tool
//! error carrying a sentence for the agent when it did not. Nothing but those sentences leaves
//! the server: no internal error text, path or type name.

use rmcp::model::{CallToolResult, ContentBlock};
use serde::Serialize;
use serde_json::json;

use crate::failure::{Failure, Kind};

/// The code of every failure to get an answer from the server.
const UNREACHABLE_CODE: &str = "unreachable";

/// What an agent is told when the server gave no decision, whatever the cause was.
const UNREACHABLE_SENTENCE: &str = "The dibs server cannot be reached right now, so this request may not have gone through. Carry on with your work, try again in a moment, and tell your person that coordination is off until it answers.";

/// The tool result of an operation: its answer as JSON, or its failure as a tool error.
pub fn result<T: Serialize>(outcome: Result<T, Failure>) -> CallToolResult {
    match outcome.and_then(|answer| serialise(&answer)) {
        Ok(value) => CallToolResult::structured(value),
        Err(failure) => failed(&failure),
    }
}

/// A tool error: the sentence as text, and the stable code and the sentence as structured content.
pub fn failed(failure: &Failure) -> CallToolResult {
    let (code, message) = match failure.kind {
        Kind::Unreachable => (UNREACHABLE_CODE, UNREACHABLE_SENTENCE),
        Kind::Refused | Kind::Config | Kind::Passed(_) => {
            (failure.code.as_str(), failure.message.as_str())
        }
    };
    let mut result = CallToolResult::error(vec![ContentBlock::text(message)]);
    result.structured_content = Some(match failure.kind {
        Kind::Unreachable => json!({"error": {"code": code, "message": message}}),
        Kind::Refused | Kind::Config | Kind::Passed(_) => crate::output::failure_json(failure),
    });
    result
}

fn serialise<T: Serialize>(answer: &T) -> Result<serde_json::Value, Failure> {
    match serde_json::to_value(answer) {
        Ok(value) if value.is_object() => Ok(value),
        Ok(_) | Err(_) => Err(Failure::config(
            "unreadable_answer",
            "The answer of the dibs server could not be put into words.",
        )),
    }
}

#[cfg(test)]
mod tests {
    use dibs_client::{ClientError, ErrorDetails};
    use serde_json::json;

    use super::*;

    fn text(result: &CallToolResult) -> String {
        result
            .content
            .first()
            .and_then(ContentBlock::as_text)
            .map(|text| text.text.clone())
            .unwrap_or_default()
    }

    #[test]
    fn an_answer_is_the_clients_json_as_structured_content_and_as_text() {
        let result = result(Ok(json!({"items": []})));
        assert_eq!(result.is_error, Some(false));
        assert_eq!(result.structured_content, Some(json!({"items": []})));
        assert_eq!(text(&result), r#"{"items":[]}"#);
    }

    #[test]
    fn a_refusal_is_a_tool_error_with_the_servers_sentence_and_the_holder() {
        let failure = Failure::from(ClientError::Conflict {
            code: "held".to_owned(),
            message: "Another member holds this item.".to_owned(),
            details: Box::new(ErrorDetails {
                holder: Some("alice".parse().unwrap()),
                item: Some(dibs_domain::ids::ItemId(4)),
                ..ErrorDetails::default()
            }),
        });
        let result = failed(&failure);
        assert_eq!(result.is_error, Some(true));
        assert_eq!(
            text(&result),
            "Another member holds this item. It is held by alice on item 4."
        );
        let structured = result.structured_content.clone().unwrap();
        assert_eq!(structured["error"]["code"], "held");
        assert_eq!(structured["error"]["message"], json!(text(&result)));
        assert_eq!(structured["error"]["details"]["holder"], "alice");
        assert_eq!(structured["error"]["details"]["item"], 4);
    }

    #[test]
    fn a_server_that_gave_no_decision_is_one_plain_sentence_whatever_the_cause() {
        let server = "https://dibs.example.com".to_owned();
        let causes = [
            ClientError::Network {
                server: server.clone(),
            },
            ClientError::Timeout {
                server: server.clone(),
            },
            ClientError::Unexpected {
                server: server.clone(),
                status: 502,
            },
            ClientError::Refused {
                status: 503,
                code: "overloaded".to_owned(),
                message: "Try later.".to_owned(),
                details: Box::default(),
            },
        ];
        for cause in causes {
            let result = failed(&Failure::from(cause.clone()));
            assert_eq!(result.is_error, Some(true), "{cause:?}");
            assert_eq!(text(&result), UNREACHABLE_SENTENCE, "{cause:?}");
            assert!(!text(&result).contains("example.com"), "{cause:?}");
            assert_eq!(
                result.structured_content.unwrap()["error"]["code"],
                UNREACHABLE_CODE
            );
        }
    }

    #[test]
    fn a_configuration_failure_keeps_its_sentence_and_code() {
        let result = failed(&Failure::config(
            "no_token",
            "No token is stored; sign in with dibs login.",
        ));
        assert_eq!(
            text(&result),
            "No token is stored; sign in with dibs login."
        );
        assert_eq!(
            result.structured_content.unwrap()["error"]["code"],
            "no_token"
        );
    }

    #[test]
    fn an_answer_that_is_not_an_object_is_a_tool_error_not_a_panic() {
        let result = result(Ok(vec![1, 2]));
        assert_eq!(result.is_error, Some(true));
        assert!(text(&result).ends_with('.'));
    }
}
