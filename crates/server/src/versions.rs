//! Which `dibs` clients the server serves, and which release it recommends.
//!
//! A client names its version in its `User-Agent` (`dibs/<semver>`). A request of the API from a
//! client older than the oldest one accepted is answered `426 Upgrade Required`, with one sentence
//! saying how to update, before anything else reads it. A request that names no `dibs` version —
//! a browser, a probe, another tool — is not judged. `/v1/version` is never refused, so a client
//! too old for everything else can still learn what to update to.

use axum::extract::{Request, State};
use axum::http::{StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use semver::Version;
use std::sync::Arc;

use crate::error::ApiError;

/// Where the latest release can be installed from by hand.
pub const INSTALL_URL: &str = "https://github.com/hryvinskyi/dibs#install";

/// The route that tells clients which versions the server accepts and recommends.
pub const VERSION_ROUTE: &str = "/v1/version";

/// The oldest client version served and the version clients are told to update to, each when set.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClientVersions {
    min: Option<Version>,
    latest: Option<Version>,
}

/// The two versions contradict each other.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("the latest client {latest} is older than the oldest client accepted, {min}")]
pub struct VersionsContradict {
    /// The oldest client accepted.
    pub min: Version,
    /// The client recommended.
    pub latest: Version,
}

impl ClientVersions {
    /// The versions, the recommended one never older than the oldest one accepted.
    ///
    /// # Errors
    /// [`VersionsContradict`] when `latest` is older than `min`.
    pub fn new(min: Option<Version>, latest: Option<Version>) -> Result<Self, VersionsContradict> {
        if let (Some(min), Some(latest)) = (&min, &latest)
            && latest < min
        {
            return Err(VersionsContradict {
                min: min.clone(),
                latest: latest.clone(),
            });
        }
        Ok(Self { min, latest })
    }

    /// The oldest client version served, when one is set.
    pub fn min(&self) -> Option<&Version> {
        self.min.as_ref()
    }

    /// The client version clients are told to update to, when one is set.
    pub fn latest(&self) -> Option<&Version> {
        self.latest.as_ref()
    }

    /// The refusal for a request with this `User-Agent`, when it comes from a client older than the
    /// oldest one served.
    pub fn refusal(&self, user_agent: Option<&str>) -> Option<ApiError> {
        let min = self.min.as_ref()?;
        let client = client_version(user_agent?)?;
        if client >= *min {
            return None;
        }
        let target = self.latest.as_ref().unwrap_or(min);
        Some(
            ApiError::new(
                StatusCode::UPGRADE_REQUIRED,
                "client_too_old",
                &format!(
                    "this server serves dibs {min} or newer and this dibs is {client}; update it with dibs self-update, or install {target} or newer from {INSTALL_URL}"
                ),
            )
            .with_detail("min_client", min.to_string()),
        )
    }
}

/// The version a `User-Agent` of the form `dibs/<semver>` names, followed by nothing or by a space
/// and more; `None` for any other agent.
pub fn client_version(user_agent: &str) -> Option<Version> {
    let rest = user_agent.trim().strip_prefix("dibs/")?;
    let version = rest.split_whitespace().next()?;
    Version::parse(version).ok()
}

/// Answers an API request from a client older than the oldest one served with the refusal, and
/// passes every other request on.
pub async fn gate(
    State(versions): State<Arc<ClientVersions>>,
    request: Request,
    next: Next,
) -> Response {
    let path = request.uri().path();
    if path.starts_with("/v1/") && path != VERSION_ROUTE {
        let agent = request
            .headers()
            .get(header::USER_AGENT)
            .and_then(|value| value.to_str().ok());
        if let Some(refusal) = versions.refusal(agent) {
            return refusal.into_response();
        }
    }
    next.run(request).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn version(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    fn versions(min: Option<&str>, latest: Option<&str>) -> ClientVersions {
        ClientVersions::new(min.map(version), latest.map(version)).unwrap()
    }

    #[test]
    fn only_a_dibs_agent_names_a_version() {
        assert_eq!(client_version("dibs/0.3.1"), Some(version("0.3.1")));
        assert_eq!(
            client_version("dibs/1.2.0-rc.1 (x86_64-unknown-linux-gnu)"),
            Some(version("1.2.0-rc.1"))
        );
        for other in [
            "curl/8.5.0",
            "dibs/",
            "dibs/one",
            "Mozilla/5.0 dibs/0.3.1",
            "",
        ] {
            assert_eq!(client_version(other), None, "{other}");
        }
    }

    #[test]
    fn a_client_below_the_oldest_served_is_refused_with_how_to_update() {
        let gate = versions(Some("0.4.0"), Some("0.5.2"));
        let refusal = gate.refusal(Some("dibs/0.3.9")).unwrap();
        assert_eq!(refusal.status(), StatusCode::UPGRADE_REQUIRED);
        assert_eq!(refusal.code(), "client_too_old");
        let message = refusal.message();
        assert!(message.contains("0.4.0 or newer"), "{message}");
        assert!(message.contains("this dibs is 0.3.9"), "{message}");
        assert!(message.contains("dibs self-update"), "{message}");
        assert!(message.contains("install 0.5.2 or newer"), "{message}");
        assert!(message.ends_with('.'), "{message}");
        assert!(
            versions(Some("0.4.0"), None)
                .refusal(Some("dibs/0.4.0-rc.1"))
                .is_some(),
            "a pre-release of the oldest version served is older than it"
        );
    }

    #[test]
    fn a_client_at_or_above_the_oldest_served_or_no_dibs_at_all_passes() {
        let gate = versions(Some("0.4.0"), None);
        for agent in [
            Some("dibs/0.4.0"),
            Some("dibs/1.0.0"),
            Some("curl/8.5.0"),
            None,
        ] {
            assert!(gate.refusal(agent).is_none(), "{agent:?}");
        }
        assert!(
            ClientVersions::default()
                .refusal(Some("dibs/0.0.1"))
                .is_none(),
            "without a minimum every client is served"
        );
    }

    #[test]
    fn the_latest_client_is_never_older_than_the_oldest_served() {
        let error =
            ClientVersions::new(Some(version("0.4.0")), Some(version("0.3.0"))).unwrap_err();
        assert!(error.to_string().contains("0.3.0"), "{error}");
        assert!(ClientVersions::new(Some(version("0.4.0")), Some(version("0.4.0"))).is_ok());
    }
}
