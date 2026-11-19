//! The list's state as its address keeps it — which claims it shows (`filter`), whose (`who`) and
//! which text they match (`q`) — and the addresses built from it. A value that does not read as a
//! filter means all claims.

use serde::Deserialize;

use dibs_domain::ids::ItemId;

use crate::ui::claims::view::{Activity, ClaimView};
use crate::ui::links::query_value;
use crate::ui::matching::matches;

/// The address of the list.
const LIST: &str = "/ui/claims";

/// A filter of the list: the name its address gives it and the claims it keeps.
#[derive(Debug)]
pub struct Filter {
    /// Its name in the address and in the page.
    pub code: &'static str,
    keeps: fn(&ClaimView) -> bool,
}

/// The filters, in the order the page offers them; the first is the default.
pub static FILTERS: [Filter; 7] = [
    Filter {
        code: "all",
        keeps: |_| true,
    },
    Filter {
        code: "working",
        keeps: |claim| claim.activity == Activity::Working,
    },
    Filter {
        code: "review",
        keeps: |claim| claim.activity == Activity::Review,
    },
    Filter {
        code: "silent",
        keeps: |claim| claim.activity == Activity::Silent,
    },
    Filter {
        code: "held",
        keeps: |claim| claim.held_over_a_week,
    },
    Filter {
        code: "finished",
        keeps: |claim| claim.finished,
    },
    Filter {
        code: "branch",
        keeps: |claim| claim.other_ticket.is_some(),
    },
];

impl Filter {
    /// Whether the filter keeps the claim.
    pub fn keeps(&self, claim: &ClaimView) -> bool {
        (self.keeps)(claim)
    }

    /// Whether it is the default filter, which the address leaves out.
    fn is_default(&self) -> bool {
        std::ptr::eq(self, &FILTERS[0])
    }
}

/// The query of the list's address as it arrives.
#[derive(Debug, Default, Deserialize)]
pub struct RawQuery {
    filter: Option<String>,
    who: Option<String>,
    q: Option<String>,
}

/// The list's state.
#[derive(Debug, Clone)]
pub struct ListQuery {
    /// The filter.
    pub filter: &'static Filter,
    /// The handle whose claims it shows; `None` for everyone's.
    pub who: Option<String>,
    /// The text the claims match; empty for any.
    pub q: String,
}

impl ListQuery {
    /// The state a query gives: an unknown or missing filter is the default, an empty `who` is
    /// everyone.
    pub fn of(raw: RawQuery) -> Self {
        let filter = raw
            .filter
            .and_then(|code| FILTERS.iter().find(|filter| filter.code == code))
            .unwrap_or(&FILTERS[0]);
        Self {
            filter,
            who: raw.who.filter(|who| !who.trim().is_empty()),
            q: raw.q.unwrap_or_default(),
        }
    }

    /// Whether `who` and `q` leave the claim in the list, whatever the filter.
    pub fn leaves(&self, claim: &ClaimView) -> bool {
        self.who
            .as_ref()
            .is_none_or(|who| claim.holder.handle == *who)
            && matches(&self.q, &claim.text())
    }

    /// The address of the list, or of the claim on `item` beside it, with this state; each value
    /// percent-encoded, and the defaults left out.
    pub fn address(&self, item: Option<ItemId>) -> String {
        let path = match item {
            Some(item) => format!("{LIST}/{item}"),
            None => LIST.to_owned(),
        };
        let pairs: Vec<String> = [
            (
                "filter",
                (!self.filter.is_default()).then_some(self.filter.code),
            ),
            ("who", self.who.as_deref()),
            ("q", Some(self.q.as_str()).filter(|q| !q.is_empty())),
        ]
        .into_iter()
        .filter_map(|(name, value)| value.map(|value| format!("{name}={}", query_value(value))))
        .collect();
        if pairs.is_empty() {
            path
        } else {
            format!("{path}?{}", pairs.join("&"))
        }
    }

    /// The same state with another filter.
    pub fn with_filter(&self, filter: &'static Filter) -> Self {
        Self {
            filter,
            ..self.clone()
        }
    }

    /// The same state for everyone's claims.
    pub fn everyone(&self) -> Self {
        Self {
            who: None,
            ..self.clone()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn query(filter: Option<&str>, who: Option<&str>, q: Option<&str>) -> ListQuery {
        ListQuery::of(RawQuery {
            filter: filter.map(str::to_owned),
            who: who.map(str::to_owned),
            q: q.map(str::to_owned),
        })
    }

    #[test]
    fn every_filter_reads_back_from_its_name_and_anything_else_is_all() {
        for filter in &FILTERS {
            assert_eq!(
                query(Some(filter.code), None, None).filter.code,
                filter.code
            );
        }
        assert_eq!(query(Some("nonsense"), None, None).filter.code, "all");
        assert_eq!(query(None, None, None).filter.code, "all");
    }

    #[test]
    fn an_address_keeps_what_differs_from_the_defaults_each_value_encoded() {
        assert_eq!(query(None, None, None).address(None), "/ui/claims");
        assert_eq!(
            query(Some("all"), Some(" "), Some("")).address(Some(ItemId(7))),
            "/ui/claims/7"
        );
        let state = query(Some("silent"), Some("bob"), Some("#12 a&b"));
        assert_eq!(
            state.address(Some(ItemId(7))),
            "/ui/claims/7?filter=silent&who=bob&q=%2312%20a%26b"
        );
        assert_eq!(
            state.everyone().address(None),
            "/ui/claims?filter=silent&q=%2312%20a%26b"
        );
        assert_eq!(
            state.with_filter(&FILTERS[0]).address(None),
            "/ui/claims?who=bob&q=%2312%20a%26b"
        );
        assert!(
            state
                .address(None)
                .bytes()
                .all(|byte| byte.is_ascii_graphic())
        );
    }
}
