//! The error answer of every route: `{ "error": { "code": "...", "message": "..." } }`, with a
//! stable `snake_case` code for clients to act on, an English sentence for the person reading it,
//! and, where a refusal names something the caller can act on, a `details` object. The sentence
//! never carries an internal error's text; that goes to the log.

use axum::extract::rejection::{JsonRejection, PathRejection, QueryRejection};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use serde_json::{Map, Value, json};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use dibs_app::AppError;
use dibs_app::error::Duplicate;
use dibs_app::named::Refusal;
use dibs_domain::claim::{ClaimRefused, ForceReleaseRefused, HolderOnly, TakeoverRefused};
use dibs_domain::delivery::DeliveryRefused;
use dibs_domain::dependency::DependencyRefused;
use dibs_domain::edit::EditCheckRefused;
use dibs_domain::grant::{GrantRefused, RevokeRefused};
use dibs_domain::handle::Handle;
use dibs_domain::import::BatchRefused;
use dibs_domain::item::CreateRefused;
use dibs_domain::message::{AckRefused, RecipientsError};
use dibs_domain::settings::AreaLookupError;
use dibs_domain::slot::{BeatRefused, SlotRefused};
use dibs_domain::wide::WideScopeError;

/// A refusal or failure as the client receives it.
#[derive(Debug, Clone, PartialEq)]
pub struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: String,
    details: Map<String, Value>,
    retry_after: Option<u64>,
}

impl ApiError {
    /// An error of this status and code, with `message` made into a sentence.
    pub fn new(status: StatusCode, code: &'static str, message: &str) -> Self {
        Self {
            status,
            code,
            message: sentence(message),
            details: Map::new(),
            retry_after: None,
        }
    }

    /// Adds one entry to `details`. A value that does not serialise is left out.
    #[must_use]
    pub fn with_detail(mut self, key: &str, value: impl Serialize) -> Self {
        if let Ok(value) = serde_json::to_value(value) {
            self.details.insert(key.to_owned(), value);
        }
        self
    }

    /// The HTTP status.
    pub fn status(&self) -> StatusCode {
        self.status
    }

    /// The stable code.
    pub fn code(&self) -> &'static str {
        self.code
    }

    /// The sentence shown to the person.
    pub fn message(&self) -> &str {
        &self.message
    }

    /// The refusal's details.
    pub fn details(&self) -> &Map<String, Value> {
        &self.details
    }

    /// The token is missing, malformed, unknown or revoked. Every cause reads the same.
    pub fn unauthorized() -> Self {
        Self::from(AppError::Unauthorized)
    }

    /// Too many failed sign-ins from one address; the client may try again after `seconds`.
    pub fn too_many_attempts(seconds: u64) -> Self {
        let mut error = Self::new(
            StatusCode::TOO_MANY_REQUESTS,
            "too_many_attempts",
            "too many attempts with an invalid token from this address; wait before trying again",
        );
        error.retry_after = Some(seconds);
        error.with_detail("retry_after", seconds)
    }

    /// The service failed in a way the caller cannot fix.
    pub fn internal() -> Self {
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            "the service could not complete the request; try again",
        )
    }

    /// The request took longer than the service allows.
    pub fn timeout() -> Self {
        Self::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "timeout",
            "the service took too long to answer; try again",
        )
    }

    /// As many event streams are open as the service allows.
    pub fn too_many_streams() -> Self {
        Self::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "too_many_streams",
            "the service has as many event streams open as it allows; try again in a minute",
        )
    }

    /// The member holds as many event streams open as one member may.
    pub fn too_many_member_streams(max: usize) -> Self {
        Self::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "too_many_streams",
            &format!("you already have {max} event streams open; close one before opening another"),
        )
    }

    /// No route answers this path.
    pub fn no_route() -> Self {
        Self::new(StatusCode::NOT_FOUND, "not_found", "there is no such route")
    }

    /// The route does not answer this method.
    pub fn method_not_allowed() -> Self {
        Self::new(
            StatusCode::METHOD_NOT_ALLOWED,
            "method_not_allowed",
            "this route does not answer that method",
        )
    }

    /// A required field of the request is absent.
    pub fn missing_field(field: &str) -> Self {
        Self::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_request",
            &format!("the field '{field}' is required"),
        )
        .with_detail("field", field)
    }

    /// A field of the request holds a value its rules refuse; `reason` states the rule.
    pub fn invalid_field(field: &str, reason: &dyn std::fmt::Display) -> Self {
        Self::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_request",
            &format!("the field '{field}' is not valid: {reason}"),
        )
        .with_detail("field", field)
    }

    /// The use case's refusal or failure. A refusal that names a member shows the handle the use
    /// case read beside the id.
    pub fn from_app(error: AppError) -> Self {
        let status = status_of(&error);
        if status == StatusCode::INTERNAL_SERVER_ERROR {
            tracing::error!(code = error.code(), error = ?error, "a request failed");
            return Self::internal();
        }
        let mut answer = Self::new(status, error.code(), &error.to_string());
        answer.details = details_of(&error);
        answer
    }
}

impl From<AppError> for ApiError {
    fn from(error: AppError) -> Self {
        Self::from_app(error)
    }
}

impl From<JsonRejection> for ApiError {
    fn from(rejection: JsonRejection) -> Self {
        tracing::debug!(reason = %rejection.body_text(), "a request body was refused");
        match rejection.status() {
            StatusCode::PAYLOAD_TOO_LARGE => Self::new(
                StatusCode::PAYLOAD_TOO_LARGE,
                "body_too_large",
                "the request body is larger than this service accepts",
            ),
            StatusCode::UNSUPPORTED_MEDIA_TYPE => Self::new(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "unsupported_media_type",
                "send the request body as JSON, with the header 'Content-Type: application/json'",
            ),
            _ => Self::new(
                StatusCode::BAD_REQUEST,
                "invalid_json",
                "the request body is not JSON of the shape this route expects",
            ),
        }
    }
}

impl From<QueryRejection> for ApiError {
    fn from(rejection: QueryRejection) -> Self {
        tracing::debug!(reason = %rejection.body_text(), "a query string was refused");
        Self::new(
            StatusCode::BAD_REQUEST,
            "invalid_query",
            "the query string is not valid for this route",
        )
    }
}

impl From<PathRejection> for ApiError {
    fn from(rejection: PathRejection) -> Self {
        tracing::debug!(reason = %rejection.body_text(), "a path was refused");
        Self::no_route()
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut error = Map::new();
        error.insert("code".to_owned(), Value::from(self.code));
        error.insert("message".to_owned(), Value::from(self.message));
        if !self.details.is_empty() {
            error.insert("details".to_owned(), Value::Object(self.details));
        }
        let body = json!({ "error": Value::Object(error) }).to_string();
        let mut response = (
            self.status,
            [(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            )],
            body,
        )
            .into_response();
        if self.status == StatusCode::UNAUTHORIZED {
            response
                .headers_mut()
                .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
        }
        if let Some(seconds) = self.retry_after {
            response
                .headers_mut()
                .insert(header::RETRY_AFTER, HeaderValue::from(seconds));
        }
        response
    }
}

/// The text with its first letter in upper case and a full stop at its end.
pub(crate) fn sentence(text: &str) -> String {
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

/// The HTTP status of each refusal and failure.
fn status_of(error: &AppError) -> StatusCode {
    match error {
        AppError::Unauthorized => StatusCode::UNAUTHORIZED,
        AppError::Forbidden
        | AppError::HolderOnly(Refusal {
            reason: HolderOnly::NotHolder { .. },
            ..
        })
        | AppError::ForceRelease(ForceReleaseRefused::NotAdmin)
        | AppError::Grant(GrantRefused::NotHolder { .. })
        | AppError::Revoke(RevokeRefused::NotGrantor)
        | AppError::NotAuthor
        | AppError::Ack(AckRefused::NotRecipient)
        | AppError::Slot(Refusal {
            reason: SlotRefused::NotHolder { .. },
            ..
        }) => StatusCode::FORBIDDEN,
        AppError::NotFound(_)
        | AppError::Slot(Refusal {
            reason: SlotRefused::UnknownSlot { .. },
            ..
        }) => StatusCode::NOT_FOUND,
        AppError::Duplicate(Duplicate::Project(_) | Duplicate::Handle(_))
        | AppError::ItemClosed { .. }
        | AppError::Claim(Refusal {
            reason:
                ClaimRefused::HeldBy { .. }
                | ClaimRefused::Stale { .. }
                | ClaimRefused::AreaHeld { .. }
                | ClaimRefused::WideOverlap { .. },
            ..
        })
        | AppError::Takeover(Refusal {
            reason:
                TakeoverRefused::NothingToTake
                | TakeoverRefused::OwnClaim
                | TakeoverRefused::NotStale { .. }
                | TakeoverRefused::AreaHeld { .. }
                | TakeoverRefused::WideOverlap { .. },
            ..
        })
        | AppError::HolderOnly(Refusal {
            reason: HolderOnly::NoClaim,
            ..
        })
        | AppError::ForceRelease(ForceReleaseRefused::NoClaim)
        | AppError::Grant(GrantRefused::GranteeHasNoClaim)
        | AppError::Dependency(DependencyRefused::Cycle)
        | AppError::Ack(AckRefused::NotRequired)
        | AppError::Slot(Refusal {
            reason:
                SlotRefused::HoldsOther { .. }
                | SlotRefused::NotHeld
                | SlotRefused::RunEnded
                | SlotRefused::NotWaiting
                | SlotRefused::OwnHold
                | SlotRefused::NotStalled { .. },
            ..
        })
        | AppError::Beat(Refusal {
            reason: BeatRefused::TakenOver { .. } | BeatRefused::Ended,
            ..
        }) => StatusCode::CONFLICT,
        AppError::NotARecipient(_)
        | AppError::Claim(Refusal {
            reason: ClaimRefused::UnknownArea { .. } | ClaimRefused::WideAndHeld { .. },
            ..
        })
        | AppError::Takeover(Refusal {
            reason: TakeoverRefused::UnknownArea { .. } | TakeoverRefused::WideAndHeld { .. },
            ..
        })
        | AppError::Wide(WideScopeError::NoAreas)
        | AppError::Grant(GrantRefused::SelfGrant | GrantRefused::UnknownArea { .. })
        | AppError::Create(CreateRefused::UnlistedRepository { .. })
        | AppError::Area(
            AreaLookupError::Malformed { .. }
            | AreaLookupError::NotFound { .. }
            | AreaLookupError::Ambiguous { .. },
        )
        | AppError::Settings(_)
        | AppError::Dependency(DependencyRefused::SelfDependency)
        | AppError::Batch(
            BatchRefused::TooMany { .. }
            | BatchRefused::CompleteWithoutSource
            | BatchRefused::Duplicate { .. },
        )
        | AppError::EditCheck(EditCheckRefused::UnknownRepo { .. })
        | AppError::Recipients(RecipientsError::Empty) => StatusCode::UNPROCESSABLE_ENTITY,
        AppError::Delivery(DeliveryRefused::NotRecipient) => StatusCode::FORBIDDEN,
        AppError::Delivery(DeliveryRefused::Delivered { .. } | DeliveryRefused::OfferGone) => {
            StatusCode::CONFLICT
        }
        AppError::ImportanceLimited(_) => StatusCode::TOO_MANY_REQUESTS,
        AppError::StoredSettings(_) | AppError::Store(_) => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

/// What a refusal names that the caller can act on.
fn details_of(error: &AppError) -> Map<String, Value> {
    let mut details = Details::default();
    match error {
        AppError::Claim(Refusal {
            reason: ClaimRefused::HeldBy {
                since, last_seen, ..
            },
            member,
            ..
        }) => {
            details.holder(member.as_ref());
            details.time("since", *since);
            details.time("last_seen", *last_seen);
        }
        AppError::Claim(Refusal {
            reason: ClaimRefused::Stale { last_seen, .. },
            member,
            ..
        })
        | AppError::Takeover(Refusal {
            reason: TakeoverRefused::NotStale { last_seen, .. },
            member,
            ..
        }) => {
            details.holder(member.as_ref());
            details.time("last_seen", *last_seen);
        }
        AppError::Claim(Refusal {
            reason:
                ClaimRefused::AreaHeld { area, item, .. } | ClaimRefused::WideOverlap { area, item, .. },
            member,
            item_ref,
        })
        | AppError::Takeover(Refusal {
            reason:
                TakeoverRefused::AreaHeld { area, item, .. }
                | TakeoverRefused::WideOverlap { area, item, .. },
            member,
            item_ref,
        }) => {
            details.put("area", area);
            details.holder(member.as_ref());
            details.put("item", item);
            if let Some(reference) = item_ref {
                details.put("item_ref", reference);
            }
        }
        AppError::Delivery(DeliveryRefused::Delivered { session, at }) => {
            details.put("session_id", session);
            details.time("at", *at);
        }
        AppError::ImportanceLimited(limited) => {
            details.put("limit", limited.limit);
            details.time("frees_at", limited.frees_at);
        }
        AppError::Batch(BatchRefused::Duplicate { reference }) => details.put("ref", reference),
        AppError::Batch(BatchRefused::TooMany { max }) => details.put("max", max),
        AppError::HolderOnly(Refusal {
            reason: HolderOnly::NotHolder { .. },
            member,
            ..
        }) => details.holder(member.as_ref()),
        AppError::Claim(Refusal {
            reason: ClaimRefused::UnknownArea { area } | ClaimRefused::WideAndHeld { area },
            ..
        })
        | AppError::Takeover(Refusal {
            reason: TakeoverRefused::UnknownArea { area } | TakeoverRefused::WideAndHeld { area },
            ..
        })
        | AppError::Grant(GrantRefused::NotHolder { area } | GrantRefused::UnknownArea { area }) => {
            details.put("area", area);
        }
        AppError::ItemClosed { status } => details.put("status", status),
        AppError::Area(AreaLookupError::Ambiguous { candidates, .. }) => {
            details.put("candidates", candidates);
        }
        AppError::Create(CreateRefused::UnlistedRepository { remote }) => {
            details.put("remote", remote);
        }
        AppError::EditCheck(EditCheckRefused::UnknownRepo { repo }) => details.put("repo", repo),
        AppError::Slot(Refusal {
            reason: SlotRefused::NotHolder { .. },
            member,
            ..
        })
        | AppError::Beat(Refusal {
            reason: BeatRefused::TakenOver { .. },
            member,
            ..
        }) => details.holder(member.as_ref()),
        AppError::Slot(Refusal {
            reason: SlotRefused::NotStalled { last_seen, .. },
            member,
            ..
        }) => {
            details.holder(member.as_ref());
            details.time("last_seen", *last_seen);
        }
        AppError::Slot(Refusal {
            reason: SlotRefused::HoldsOther { held },
            ..
        }) => details.put("held", held),
        AppError::Slot(Refusal {
            reason: SlotRefused::UnknownSlot { slot },
            ..
        }) => details.put("slot", slot),
        _ => {}
    }
    details.0
}

/// A `details` object being filled.
#[derive(Default)]
struct Details(Map<String, Value>);

impl Details {
    fn put(&mut self, key: &str, value: impl Serialize) {
        if let Ok(value) = serde_json::to_value(value) {
            self.0.insert(key.to_owned(), value);
        }
    }

    fn holder(&mut self, member: Option<&Handle>) {
        if let Some(handle) = member {
            self.put("holder", handle);
        }
    }

    fn time(&mut self, key: &str, at: OffsetDateTime) {
        if let Ok(text) = at.format(&Rfc3339) {
            self.put(key, text);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use dibs_app::error::Missing;
    use dibs_app::ports::StoreError;
    use dibs_domain::area::QualifiedArea;
    use dibs_domain::ids::{ItemId, SessionId};

    use dibs_domain::ids::MemberId;

    use super::*;

    fn unnamed<R>(reason: R) -> Refusal<R> {
        Refusal {
            reason,
            member: None,
            item_ref: None,
        }
    }

    fn area(text: &str) -> QualifiedArea {
        QualifiedArea::parse(text).unwrap()
    }

    async fn body_of(error: ApiError) -> (StatusCode, Value, axum::http::HeaderMap) {
        let response = error.into_response();
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = http_body_util::BodyExt::collect(response.into_body())
            .await
            .unwrap()
            .to_bytes();
        (status, serde_json::from_slice(&bytes).unwrap(), headers)
    }

    #[tokio::test]
    async fn the_shape_is_code_and_message_under_error() {
        let (status, body, headers) =
            body_of(ApiError::from(AppError::NotFound(Missing::Item(ItemId(7))))).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(
            body,
            json!({"error": {"code": "not_found", "message": "There is no work item 7."}})
        );
        assert_eq!(headers[header::CONTENT_TYPE], "application/json");
    }

    #[tokio::test]
    async fn an_internal_failure_tells_nothing_of_its_cause() {
        let error = AppError::Store(StoreError::Corrupt {
            what: "claims row 7: areas column".to_owned(),
        });
        let (status, body, _) = body_of(ApiError::from(error)).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body["error"]["code"], "internal");
        assert!(!body.to_string().contains("claims row"));
    }

    #[tokio::test]
    async fn unauthorized_asks_for_a_bearer_token() {
        let (status, body, headers) = body_of(ApiError::unauthorized()).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body["error"]["code"], "unauthorized");
        assert_eq!(headers[header::WWW_AUTHENTICATE], "Bearer");
    }

    #[tokio::test]
    async fn too_many_attempts_says_when_to_retry() {
        let (status, body, headers) = body_of(ApiError::too_many_attempts(6)).await;
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(headers[header::RETRY_AFTER], "6");
        assert_eq!(body["error"]["details"]["retry_after"], 6);
    }

    #[test]
    fn refusals_map_to_their_status() {
        let held = AppError::Claim(unnamed(ClaimRefused::HeldBy {
            member: MemberId(1),
            since: OffsetDateTime::UNIX_EPOCH,
            last_seen: OffsetDateTime::UNIX_EPOCH,
        }));
        let cases: Vec<(AppError, StatusCode, &str)> = vec![
            (AppError::Forbidden, StatusCode::FORBIDDEN, "forbidden"),
            (held, StatusCode::CONFLICT, "held"),
            (
                AppError::HolderOnly(unnamed(HolderOnly::NotHolder {
                    member: MemberId(1),
                })),
                StatusCode::FORBIDDEN,
                "not_holder",
            ),
            (
                AppError::HolderOnly(unnamed(HolderOnly::NoClaim)),
                StatusCode::CONFLICT,
                "no_claim",
            ),
            (
                AppError::Takeover(unnamed(TakeoverRefused::NotStale {
                    member: MemberId(1),
                    last_seen: OffsetDateTime::UNIX_EPOCH,
                })),
                StatusCode::CONFLICT,
                "not_stale",
            ),
            (
                AppError::Grant(GrantRefused::SelfGrant),
                StatusCode::UNPROCESSABLE_ENTITY,
                "self_grant",
            ),
            (
                AppError::Revoke(RevokeRefused::NotGrantor),
                StatusCode::FORBIDDEN,
                "not_grantor",
            ),
            (
                AppError::NotFound(Missing::Session(SessionId(3))),
                StatusCode::NOT_FOUND,
                "not_found",
            ),
            (
                AppError::Dependency(DependencyRefused::Cycle),
                StatusCode::CONFLICT,
                "dependency_cycle",
            ),
            (
                AppError::Dependency(DependencyRefused::SelfDependency),
                StatusCode::UNPROCESSABLE_ENTITY,
                "self_dependency",
            ),
            (
                AppError::Batch(BatchRefused::TooMany { max: 500 }),
                StatusCode::UNPROCESSABLE_ENTITY,
                "batch_too_large",
            ),
        ];
        for (error, status, code) in cases {
            let answer = ApiError::from(error);
            assert_eq!((answer.status(), answer.code()), (status, code));
        }
    }

    #[test]
    fn a_refusal_names_the_holder_by_the_handle_it_carries() {
        let error = AppError::Claim(Refusal {
            reason: ClaimRefused::AreaHeld {
                area: area("api:billing"),
                member: MemberId(4),
                item: ItemId(9),
            },
            member: Some(Handle::parse("alice").unwrap()),
            item_ref: Some(Box::new(
                dibs_domain::item::ExternalRef::parse("gh:acme/web#12").unwrap(),
            )),
        });
        let answer = ApiError::from(error);
        assert_eq!(answer.code(), "area_held");
        assert_eq!(
            Value::Object(answer.details().clone()),
            json!({"area": "api:billing", "holder": "alice", "item": 9, "item_ref": "gh:acme/web#12"})
        );
    }

    #[test]
    fn an_ambiguous_area_lists_its_candidates() {
        let error = AppError::Area(AreaLookupError::Ambiguous {
            text: "billing".to_owned(),
            candidates: BTreeSet::from([area("api:billing"), area("web:billing")])
                .into_iter()
                .collect(),
        });
        let answer = ApiError::from(error);
        assert_eq!(answer.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(
            answer.details()["candidates"],
            json!(["api:billing", "web:billing"])
        );
    }

    #[test]
    fn messages_are_sentences() {
        assert_eq!(
            sentence("nobody holds this item"),
            "Nobody holds this item."
        );
        assert_eq!(sentence("done."), "Done.");
        assert_eq!(
            ApiError::missing_field("title").message(),
            "The field 'title' is required."
        );
    }
}
