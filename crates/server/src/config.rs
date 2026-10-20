//! The server's configuration, read once from environment variables through an injected lookup, so
//! the parser is a pure function: the binary passes the process environment, tests pass a map.

use std::net::SocketAddr;
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::time::Duration;

use axum::http::HeaderName;
use thiserror::Error;
use tracing::level_filters::LevelFilter;

use semver::Version;

use crate::client_ip::{Cidr, ProxyTrust};
use crate::versions::ClientVersions;

/// The address the server listens on when `DIBS_LISTEN` is not set.
pub const DEFAULT_LISTEN: &str = "0.0.0.0:8080";

/// The header a trusted proxy names the client in when `DIBS_CLIENT_IP_HEADER` is not set.
pub const DEFAULT_CLIENT_IP_HEADER: &str = "X-Forwarded-For";

/// How often a snapshot is taken when `DIBS_SNAPSHOT_EVERY` is not set.
pub const DEFAULT_SNAPSHOT_EVERY: Duration = Duration::from_secs(6 * 60 * 60);

/// How many snapshots are kept when `DIBS_SNAPSHOT_KEEP` is not set.
pub const DEFAULT_SNAPSHOT_KEEP: NonZeroUsize = NonZeroUsize::MIN.saturating_add(27);

/// The shortest interval between snapshots: each one copies the whole database.
pub const MIN_SNAPSHOT_EVERY: Duration = Duration::from_secs(60);

/// The longest interval between snapshots.
pub const MAX_SNAPSHOT_EVERY: Duration = Duration::from_secs(30 * 24 * 60 * 60);

/// The most snapshots that may be kept.
pub const MAX_SNAPSHOT_KEEP: usize = 10_000;

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
    /// Where and how often snapshots are taken, when they are (`DIBS_SNAPSHOT_DIR`,
    /// `DIBS_SNAPSHOT_EVERY`, `DIBS_SNAPSHOT_KEEP`).
    pub snapshots: Option<SnapshotConfig>,
    /// The oldest client served and the client recommended (`DIBS_MIN_CLIENT`,
    /// `DIBS_LATEST_CLIENT`).
    pub versions: ClientVersions,
}

/// Where snapshots of the database are written, how often, and how many are kept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotConfig {
    /// The directory the snapshots are written into. It must exist.
    pub dir: PathBuf,
    /// The time between two scheduled snapshots.
    pub every: Duration,
    /// How many of the newest snapshots are kept.
    pub keep: NonZeroUsize,
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
        let snapshots = parse_snapshots(&get)?;
        let versions = parse_versions(&get)?;
        Ok(Self {
            database,
            listen,
            proxy,
            log,
            snapshots,
            versions,
        })
    }
}

fn parse_versions(get: &impl Fn(&str) -> Option<String>) -> Result<ClientVersions, ConfigError> {
    let min = get("DIBS_MIN_CLIENT")
        .map(|text| parse_version("DIBS_MIN_CLIENT", &text))
        .transpose()?;
    let latest = get("DIBS_LATEST_CLIENT")
        .map(|text| parse_version("DIBS_LATEST_CLIENT", &text))
        .transpose()?;
    ClientVersions::new(min, latest).map_err(|error| ConfigError::Invalid {
        var: "DIBS_LATEST_CLIENT",
        reason: format!("{error}; raise it to DIBS_MIN_CLIENT or above"),
    })
}

/// A release version such as `1.4.0`, written with or without a leading `v`.
fn parse_version(var: &'static str, text: &str) -> Result<Version, ConfigError> {
    let text = text.trim();
    Version::parse(text.strip_prefix('v').unwrap_or(text)).map_err(|_| ConfigError::Invalid {
        var,
        reason: "it is a release version, such as 1.4.0".to_owned(),
    })
}

fn parse_snapshots(
    get: &impl Fn(&str) -> Option<String>,
) -> Result<Option<SnapshotConfig>, ConfigError> {
    let every = get("DIBS_SNAPSHOT_EVERY")
        .map(|text| parse_every(&text))
        .transpose()?;
    let keep = get("DIBS_SNAPSHOT_KEEP")
        .map(|text| parse_keep(&text))
        .transpose()?;
    let Some(dir) = get("DIBS_SNAPSHOT_DIR") else {
        if every.is_some() || keep.is_some() {
            return Err(ConfigError::Missing {
                var: "DIBS_SNAPSHOT_DIR",
                hint: "DIBS_SNAPSHOT_EVERY and DIBS_SNAPSHOT_KEEP need the directory the snapshots are written into",
            });
        }
        return Ok(None);
    };
    Ok(Some(SnapshotConfig {
        dir: PathBuf::from(dir.trim()),
        every: every.unwrap_or(DEFAULT_SNAPSHOT_EVERY),
        keep: keep.unwrap_or(DEFAULT_SNAPSHOT_KEEP),
    }))
}

/// A whole number followed by `s`, `m`, `h` or `d`, such as `6h`, between
/// [`MIN_SNAPSHOT_EVERY`] and [`MAX_SNAPSHOT_EVERY`].
fn parse_every(text: &str) -> Result<Duration, ConfigError> {
    let invalid = |reason: &str| ConfigError::Invalid {
        var: "DIBS_SNAPSHOT_EVERY",
        reason: reason.to_owned(),
    };
    let text = text.trim();
    let split = text
        .find(|c: char| !c.is_ascii_digit())
        .ok_or_else(|| invalid("it is a number and a unit, such as 6h, 30m or 1d"))?;
    let (number, unit) = text.split_at(split);
    let number: u64 = number
        .parse()
        .map_err(|_| invalid("it is a number and a unit, such as 6h, 30m or 1d"))?;
    let unit_seconds: u64 = match unit {
        "s" => 1,
        "m" => 60,
        "h" => 60 * 60,
        "d" => 24 * 60 * 60,
        _ => return Err(invalid("its unit is one of s, m, h or d, such as 6h")),
    };
    let every = number
        .checked_mul(unit_seconds)
        .map(Duration::from_secs)
        .ok_or_else(|| invalid("it is at most 30d"))?;
    if every < MIN_SNAPSHOT_EVERY {
        return Err(invalid(
            "it is at least 1m; every snapshot copies the whole database",
        ));
    }
    if every > MAX_SNAPSHOT_EVERY {
        return Err(invalid("it is at most 30d"));
    }
    Ok(every)
}

/// A whole number from 1 to [`MAX_SNAPSHOT_KEEP`].
fn parse_keep(text: &str) -> Result<NonZeroUsize, ConfigError> {
    text.trim()
        .parse::<usize>()
        .ok()
        .filter(|keep| *keep <= MAX_SNAPSHOT_KEEP)
        .and_then(NonZeroUsize::new)
        .ok_or_else(|| ConfigError::Invalid {
            var: "DIBS_SNAPSHOT_KEEP",
            reason: "it is a whole number from 1 to 10000; the newest snapshot is always kept"
                .to_owned(),
        })
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
        assert_eq!(config.snapshots, None);
        assert_eq!(config.versions, ClientVersions::default());
    }

    #[test]
    fn the_client_versions_are_release_versions_in_order() {
        let config = parse(&[
            ("DIBS_DATABASE", "dibs.db"),
            ("DIBS_MIN_CLIENT", " v0.4.0 "),
            ("DIBS_LATEST_CLIENT", "0.5.1"),
        ])
        .unwrap();
        assert_eq!(
            config.versions.min().map(ToString::to_string).as_deref(),
            Some("0.4.0")
        );
        assert_eq!(
            config.versions.latest().map(ToString::to_string).as_deref(),
            Some("0.5.1")
        );
        let cases: &[(&[(&str, &str)], &str)] = &[
            (&[("DIBS_MIN_CLIENT", "four")], "DIBS_MIN_CLIENT"),
            (&[("DIBS_LATEST_CLIENT", "1.2")], "DIBS_LATEST_CLIENT"),
            (
                &[
                    ("DIBS_MIN_CLIENT", "0.5.0"),
                    ("DIBS_LATEST_CLIENT", "0.4.9"),
                ],
                "DIBS_LATEST_CLIENT",
            ),
        ];
        for (pairs, var) in cases {
            let mut all = vec![("DIBS_DATABASE", "dibs.db")];
            all.extend_from_slice(pairs);
            let error = parse(&all).unwrap_err();
            assert!(error.to_string().starts_with(var), "{pairs:?}: {error}");
        }
    }

    #[test]
    fn a_snapshot_directory_turns_snapshots_on_with_the_defaults() {
        let config = parse(&[
            ("DIBS_DATABASE", "dibs.db"),
            ("DIBS_SNAPSHOT_DIR", "/snapshots"),
        ])
        .unwrap();
        let snapshots = config.snapshots.unwrap();
        assert_eq!(snapshots.dir, PathBuf::from("/snapshots"));
        assert_eq!(snapshots.every, Duration::from_secs(6 * 60 * 60));
        assert_eq!(snapshots.keep.get(), 28);
    }

    #[test]
    fn the_interval_and_retention_are_read_in_their_units() {
        let cases: &[(&str, u64)] = &[("90s", 90), ("30m", 1800), ("6h", 21_600), ("1d", 86_400)];
        for (text, seconds) in cases {
            let config = parse(&[
                ("DIBS_DATABASE", "dibs.db"),
                ("DIBS_SNAPSHOT_DIR", "/snapshots"),
                ("DIBS_SNAPSHOT_EVERY", text),
                ("DIBS_SNAPSHOT_KEEP", " 3 "),
            ])
            .unwrap();
            let snapshots = config.snapshots.unwrap();
            assert_eq!(snapshots.every, Duration::from_secs(*seconds), "{text}");
            assert_eq!(snapshots.keep.get(), 3);
        }
    }

    #[test]
    fn a_nonsensical_interval_or_retention_is_refused() {
        let cases: &[(&str, &str, &str)] = &[
            ("DIBS_SNAPSHOT_EVERY", "0h", "at least 1m"),
            ("DIBS_SNAPSHOT_EVERY", "30s", "at least 1m"),
            ("DIBS_SNAPSHOT_EVERY", "31d", "at most 30d"),
            ("DIBS_SNAPSHOT_EVERY", "99999999999999999d", "at most 30d"),
            ("DIBS_SNAPSHOT_EVERY", "6", "a number and a unit"),
            ("DIBS_SNAPSHOT_EVERY", "h", "a number and a unit"),
            ("DIBS_SNAPSHOT_EVERY", "-6h", "a number and a unit"),
            ("DIBS_SNAPSHOT_EVERY", "6 hours", "its unit"),
            ("DIBS_SNAPSHOT_KEEP", "0", "from 1 to 10000"),
            ("DIBS_SNAPSHOT_KEEP", "-1", "from 1 to 10000"),
            ("DIBS_SNAPSHOT_KEEP", "10001", "from 1 to 10000"),
            ("DIBS_SNAPSHOT_KEEP", "many", "from 1 to 10000"),
        ];
        for (var, value, reason) in cases {
            let error = parse(&[
                ("DIBS_DATABASE", "dibs.db"),
                ("DIBS_SNAPSHOT_DIR", "/snapshots"),
                (var, value),
            ])
            .unwrap_err();
            let text = error.to_string();
            assert!(text.starts_with(var), "{var}={value}: {text}");
            assert!(text.contains(reason), "{var}={value}: {text}");
        }
    }

    #[test]
    fn snapshot_settings_without_a_directory_are_refused() {
        for var in ["DIBS_SNAPSHOT_EVERY", "DIBS_SNAPSHOT_KEEP"] {
            let value = if var == "DIBS_SNAPSHOT_EVERY" {
                "6h"
            } else {
                "3"
            };
            let error = parse(&[("DIBS_DATABASE", "dibs.db"), (var, value)]).unwrap_err();
            assert!(
                error
                    .to_string()
                    .starts_with("DIBS_SNAPSHOT_DIR is not set"),
                "{error}"
            );
        }
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
