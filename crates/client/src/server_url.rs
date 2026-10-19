//! The address of a dibs server, in one normalised form so that one server has one spelling in the
//! credentials file and in every cache key.

use std::fmt;
use std::net::IpAddr;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use url::{Host, Url};

/// A server's base URL: `https://host[:port][/prefix]`, scheme and host in lower case, no default
/// port, no user, query, fragment or trailing `/`. Plain `http` is accepted only for a loopback
/// host, so a token never crosses a network unencrypted.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ServerUrl(String);

/// Why a text is not a server URL.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ServerUrlError {
    /// The text is no absolute URL.
    #[error("'{text}' is not a server address; write it as https://dibs.example.com")]
    Malformed {
        /// The text as given.
        text: String,
    },
    /// The scheme is neither `https` nor `http`.
    #[error("the server address '{text}' must start with https://")]
    Scheme {
        /// The text as given.
        text: String,
    },
    /// Plain `http` to a host that is not this machine.
    #[error(
        "the server address '{text}' uses plain http to another machine; use https so the token is not sent unencrypted"
    )]
    Insecure {
        /// The text as given.
        text: String,
    },
    /// The URL carries a user, a query or a fragment.
    #[error("the server address '{text}' must not hold a user name, a query or a fragment")]
    Extra {
        /// The text as given.
        text: String,
    },
}

impl ServerUrl {
    /// Parses and normalises a server address.
    ///
    /// # Errors
    /// [`ServerUrlError`] naming what is wrong.
    pub fn parse(text: &str) -> Result<Self, ServerUrlError> {
        let trimmed = text.trim();
        let malformed = || ServerUrlError::Malformed {
            text: trimmed.to_owned(),
        };
        let url = Url::parse(trimmed).map_err(|_| malformed())?;
        let scheme = url.scheme();
        if scheme != "https" && scheme != "http" {
            return Err(ServerUrlError::Scheme {
                text: trimmed.to_owned(),
            });
        }
        if !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(ServerUrlError::Extra {
                text: trimmed.to_owned(),
            });
        }
        let host = url.host().ok_or_else(malformed)?;
        if scheme == "http" && !is_loopback(&host) {
            return Err(ServerUrlError::Insecure {
                text: trimmed.to_owned(),
            });
        }
        let mut normal = format!("{scheme}://{host}");
        if let Some(port) = url.port() {
            normal.push_str(&format!(":{port}"));
        }
        normal.push_str(url.path().trim_end_matches('/'));
        Ok(Self(normal))
    }

    /// The normalised text.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The URL of a route of the API (`/v1/me`) under this server.
    pub(crate) fn route(&self, path: &str) -> String {
        format!("{}{path}", self.0)
    }
}

fn is_loopback(host: &Host<&str>) -> bool {
    match host {
        Host::Domain(name) => name.eq_ignore_ascii_case("localhost"),
        Host::Ipv4(address) => IpAddr::V4(*address).is_loopback(),
        Host::Ipv6(address) => IpAddr::V6(*address).is_loopback(),
    }
}

impl fmt::Display for ServerUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<String> for ServerUrl {
    type Error = ServerUrlError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}

impl From<ServerUrl> for String {
    fn from(value: ServerUrl) -> Self {
        value.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_spelling_of_one_server_normalises_to_one_form() {
        for text in [
            "https://dibs.example.com",
            "https://dibs.example.com/",
            "HTTPS://Dibs.Example.COM",
            "https://dibs.example.com:443/",
            "  https://dibs.example.com  ",
        ] {
            assert_eq!(
                ServerUrl::parse(text).unwrap().as_str(),
                "https://dibs.example.com",
                "{text:?}"
            );
        }
        assert_eq!(
            ServerUrl::parse("https://example.com:8443/dibs/")
                .unwrap()
                .as_str(),
            "https://example.com:8443/dibs"
        );
        assert_eq!(
            ServerUrl::parse("https://example.com/dibs")
                .unwrap()
                .route("/v1/me"),
            "https://example.com/dibs/v1/me"
        );
    }

    #[test]
    fn plain_http_only_reaches_this_machine() {
        for text in [
            "http://127.0.0.1:8080",
            "http://localhost:8080",
            "http://[::1]:8080",
        ] {
            assert!(ServerUrl::parse(text).is_ok(), "{text}");
        }
        assert!(matches!(
            ServerUrl::parse("http://dibs.example.com"),
            Err(ServerUrlError::Insecure { .. })
        ));
    }

    #[test]
    fn anything_else_is_refused() {
        for (text, expected) in [
            ("dibs.example.com", "Malformed"),
            ("", "Malformed"),
            ("ftp://example.com", "Scheme"),
            ("https://alice@example.com", "Extra"),
            ("https://example.com/?x=1", "Extra"),
            ("https://example.com/#top", "Extra"),
        ] {
            let error = ServerUrl::parse(text).unwrap_err();
            assert!(
                format!("{error:?}").starts_with(expected),
                "{text}: {error:?}"
            );
        }
    }
}
