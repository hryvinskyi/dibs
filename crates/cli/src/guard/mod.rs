//! The guard's ports, implemented over the client: the checkouts it works in, what this machine
//! remembers, the project's server, the background upkeep, and the pool of worktrees a checkout
//! lends.

pub mod cache;
pub mod gateway;
pub mod ledger;
pub mod machine;
pub mod remote;
pub mod report;
pub mod timing;
pub mod upkeep;
pub mod views;
pub mod workspace;
pub mod worktrees;

use time::OffsetDateTime;

use dibs_agent_host::ports::Clock;

/// The machine's clock.
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> OffsetDateTime {
        OffsetDateTime::now_utc()
    }
}

/// The variable that switches the guard off on this machine.
pub const OFF_VAR: &str = "DIBS_OFF";

/// Whether the variable's value switches the guard off: `1`, `true`, `yes` or `on`.
pub fn switched_off(value: Option<&str>) -> bool {
    value.is_some_and(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_switch_is_on_for_the_usual_yes_words_only() {
        for on in ["1", "true", "YES", " on "] {
            assert!(switched_off(Some(on)), "{on}");
        }
        for off in ["0", "", "false", "off", "no"] {
            assert!(!switched_off(Some(off)), "{off}");
        }
        assert!(!switched_off(None));
    }
}
