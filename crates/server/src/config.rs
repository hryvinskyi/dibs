//! The server's configuration, read once from environment variables through an injected lookup, so
//! the parser is a pure function: the binary passes the process environment, tests pass a map.

use std::net::SocketAddr;
use std::path::PathBuf;

use axum::http::HeaderName;
use thiserror::Error;
use tracing::level_filters::LevelFilter;

use crate::client_ip::{Cidr, ProxyTrust};

/// The address the server listens on when `DIBS_LISTEN` is not set.
pub const DEFAULT_LISTEN: &str = "0.0.0.0:8080";

/// The header a trusted proxy names the client in when `DIBS_CLIENT_IP_HEADER` is not set.
pub const DEFAULT_CLIENT_IP_HEADER: &str = "X-Forwarded-For";

/// The server's configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// The SQLite database file (`DIBS_DATABASE`).
    pub database: PathBuf,
    /// The address to listen on (`DIBS_LISTEN`).
    pub listen: SocketAddr,
    /// The reverse proxies whose word on the client's address is taken, when there are any
    /// (`DIBS_TRUST_PROXY`, `DIBS_TRUSTED_PROXIES`, `DIBS_CLIENT_IP_HEADER`).
    pub proxy: Option<ProxyTrust>,
    /// The most detailed level written to the log (`DIBS_LOG`).
    pub log: LevelFilter,
}

/// Why the configuration is refused. Each message names the variable at fault.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ConfigError {
    /// A required variable is not set.
    #[error("{var} is not set; {hint}")]
    Missing {
        /// The variable.
        var: &'static str,
        /// What to set it to.
        hint: &'static str,
    },
    /// A variable holds a value its rules refuse.
    #[error("{var} is not valid: {reason}")]
    Invalid {
        /// The variable.
        var: &'static str,
        /// The rule broken.
        reason: String,
    },
}

impl Config {
    /// Reads the configuration through `lookup`, which answers a variable's value or `None` when it
    /// is not set. An empty value counts as not set.
    ///
    /// # Errors
    /// [`ConfigError`] naming the first variable at fault.
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let get = |var: &str| lookup(var).filter(|value| !value.trim().is_empty());
        let database = get("DIBS_DATABASE")
            .map(PathBuf::from)
            .ok_or(ConfigError::Missing {
                var: "DIBS_DATABASE",
                hint: "set it to the path of the database file",
            })?;
        let listen_text = get("DIBS_LISTEN").unwrap_or_else(|| DEFAULT_LISTEN.to_owned());
        let listen =
            listen_text
                .trim()
                .parse::<SocketAddr>()
                .map_err(|_| ConfigError::Invalid {
                    var: "DIBS_LISTEN",
                    reason: "it is an address and a port, such as 0.0.0.0:8080".to_owned(),
                })?;
        let trust = match get("DIBS_TRUST_PROXY").as_deref().map(str::trim) {
            None | Some("0") => false,
            Some("1") => true,
            Some(_) => {
                return Err(ConfigError::Invalid {
                    var: "DIBS_TRUST_PROXY",
                    reason: "it is 0 or 1".to_owned(),
                });
            }
        };
        let trusted = get("DIBS_TRUSTED_PROXIES")
            .map(|text| parse_cidrs(&text))
            .transpose()?;
        let header = parse_header(get("DIBS_CLIENT_IP_HEADER"))?;
        let proxy = if trust {
            let trusted = trusted.ok_or(ConfigError::Missing {
                var: "DIBS_TRUSTED_PROXIES",
                hint: "with DIBS_TRUST_PROXY=1 it lists the proxies' addresses as CIDRs, such as 172.18.0.0/16",
            })?;
            Some(ProxyTrust::new(trusted, header))
        } else {
            None
        };
        let log = match get("DIBS_LOG") {
            None => LevelFilter::INFO,
            Some(text) => text
                .trim()
                .to_ascii_lowercase()
                .parse::<LevelFilter>()
                .map_err(|_| ConfigError::Invalid {
                    var: "DIBS_LOG",
                    reason: "it is one of error, warn, info, debug, trace or off".to_owned(),
                })?,
        };
        Ok(Self {
            database,
            listen,
            proxy,
            log,
        })
    }
}

fn parse_cidrs(text: &str) -> Result<Vec<Cidr>, ConfigError> {
    let cidrs: Vec<Cidr> = text
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(|part| {
            Cidr::parse(part).map_err(|reason| ConfigError::Invalid {
                var: "DIBS_TRUSTED_PROXIES",
                reason: format!("'{part}': {reason}"),
            })
        })
        .collect::<Result<_, _>>()?;
    if cidrs.is_empty() {
        return Err(ConfigError::Invalid {
            var: "DIBS_TRUSTED_PROXIES",
            reason: "it names no address".to_owned(),
        });
    }
    Ok(cidrs)
}

fn parse_header(text: Option<String>) -> Result<HeaderName, ConfigError> {
    let text = text.unwrap_or_else(|| DEFAULT_CLIENT_IP_HEADER.to_owned());
    HeaderName::from_bytes(text.trim().as_bytes()).map_err(|_| ConfigError::Invalid {
        var: "DIBS_CLIENT_IP_HEADER",
        reason: "it is an HTTP header name, such as X-Forwarded-For".to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn parse(pairs: &[(&str, &str)]) -> Result<Config, ConfigError> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        Config::from_lookup(|var| map.get(var).cloned())
    }

    #[test]
    fn only_the_database_is_required() {
        let config = parse(&[("DIBS_DATABASE", "/var/lib/dibs/dibs.db")]).unwrap();
        assert_eq!(config.database, PathBuf::from("/var/lib/dibs/dibs.db"));
        assert_eq!(config.listen, "0.0.0.0:8080".parse().unwrap());
        assert_eq!(config.proxy, None);
        assert_eq!(config.log, LevelFilter::INFO);
    }

    #[test]
    fn a_missing_database_names_the_variable() {
        let error = parse(&[]).unwrap_err();
        assert!(error.to_string().starts_with("DIBS_DATABASE is not set"));
        let error = parse(&[("DIBS_DATABASE", "  ")]).unwrap_err();
        assert!(matches!(
            error,
            ConfigError::Missing {
                var: "DIBS_DATABASE",
                ..
            }
        ));
    }

    #[test]
    fn every_invalid_value_names_its_variable() {
        let cases: &[(&[(&str, &str)], &str)] = &[
            (&[("DIBS_LISTEN", "localhost")], "DIBS_LISTEN"),
            (&[("DIBS_TRUST_PROXY", "yes")], "DIBS_TRUST_PROXY"),
            (&[("DIBS_TRUST_PROXY", "1")], "DIBS_TRUSTED_PROXIES"),
            (
                &[
                    ("DIBS_TRUST_PROXY", "1"),
                    ("DIBS_TRUSTED_PROXIES", "10.0.0.0/33"),
                ],
                "DIBS_TRUSTED_PROXIES",
            ),
            (&[("DIBS_TRUSTED_PROXIES", " , ")], "DIBS_TRUSTED_PROXIES"),
            (
                &[("DIBS_CLIENT_IP_HEADER", "bad header")],
                "DIBS_CLIENT_IP_HEADER",
            ),
            (&[("DIBS_LOG", "loud")], "DIBS_LOG"),
        ];
        for (pairs, var) in cases {
            let mut all = vec![("DIBS_DATABASE", "dibs.db")];
            all.extend_from_slice(pairs);
            let error = parse(&all).unwrap_err();
            assert!(error.to_string().starts_with(var), "{pairs:?}: {error}");
        }
    }

    #[test]
    fn a_trusted_proxy_comes_with_its_header() {
        let config = parse(&[
            ("DIBS_DATABASE", "dibs.db"),
            ("DIBS_LISTEN", "127.0.0.1:9000"),
            ("DIBS_TRUST_PROXY", "1"),
            ("DIBS_TRUSTED_PROXIES", "172.18.0.0/16, 10.0.0.1"),
            ("DIBS_CLIENT_IP_HEADER", "CF-Connecting-IP"),
            ("DIBS_LOG", "DEBUG"),
        ])
        .unwrap();
        assert_eq!(config.listen, "127.0.0.1:9000".parse().unwrap());
        let proxy = config.proxy.unwrap();
        assert_eq!(proxy.header().as_str(), "cf-connecting-ip");
        assert_eq!(proxy.trusted().len(), 2);
        assert_eq!(config.log, LevelFilter::DEBUG);
    }

    #[test]
    fn proxies_listed_without_trust_are_checked_but_not_used() {
        let config = parse(&[
            ("DIBS_DATABASE", "dibs.db"),
            ("DIBS_TRUST_PROXY", "0"),
            ("DIBS_TRUSTED_PROXIES", "10.0.0.0/8"),
        ])
        .unwrap();
        assert_eq!(config.proxy, None);
    }
}
