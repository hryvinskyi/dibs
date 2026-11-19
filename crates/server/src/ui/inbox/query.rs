//! The list's state as its address keeps it — which tab it shows (`tab`) and which text the
//! conversations match (`q`) — and the addresses built from it. A value that does not read as a tab
//! names none.

use serde::Deserialize;

use dibs_domain::ids::ThreadId;

use crate::ui::inbox::list::{TABS, Tab};
use crate::ui::links::query_value;

/// The address of the list.
const LIST: &str = "/ui/inbox";

/// The query of the list's address as it arrives.
#[derive(Debug, Default, Deserialize)]
pub(crate) struct RawQuery {
    tab: Option<String>,
    q: Option<String>,
}

/// The list's state.
#[derive(Debug, Clone, Default)]
pub(crate) struct ListQuery {
    /// The tab the address names, when it names one.
    pub(crate) tab: Option<&'static Tab>,
    /// The text the conversations match; empty for any.
    pub(crate) q: String,
}

impl ListQuery {
    /// The state a query gives: an unknown or missing tab names none.
    pub(crate) fn of(raw: RawQuery) -> Self {
        Self {
            tab: raw
                .tab
                .and_then(|code| TABS.iter().find(|tab| tab.code == code)),
            q: raw.q.unwrap_or_default(),
        }
    }

    /// The same state showing `tab`.
    pub(crate) fn showing(&self, tab: &'static Tab) -> Self {
        Self {
            tab: Some(tab),
            ..self.clone()
        }
    }

    /// The address of the list, or of the conversation `thread` beside it, with this state; each
    /// value percent-encoded, and the default tab and an empty text left out.
    pub(crate) fn address(&self, thread: Option<ThreadId>) -> String {
        let path = match thread {
            Some(thread) => format!("{LIST}/{thread}"),
            None => LIST.to_owned(),
        };
        let pairs: Vec<String> = [
            (
                "tab",
                self.tab.filter(|tab| !tab.is_default()).map(|tab| tab.code),
            ),
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
}

#[cfg(test)]
mod tests {
    use super::*;

    fn query(tab: Option<&str>, q: Option<&str>) -> ListQuery {
        ListQuery::of(RawQuery {
            tab: tab.map(str::to_owned),
            q: q.map(str::to_owned),
        })
    }

    #[test]
    fn every_tab_reads_back_from_its_name_and_anything_else_names_none() {
        for tab in &TABS {
            assert_eq!(
                query(Some(tab.code), None).tab.map(|t| t.code),
                Some(tab.code)
            );
        }
        assert!(query(Some("nonsense"), None).tab.is_none());
        assert!(query(None, None).tab.is_none());
    }

    #[test]
    fn an_address_keeps_what_differs_from_the_defaults_each_value_encoded() {
        assert_eq!(query(None, None).address(None), "/ui/inbox");
        assert_eq!(
            query(Some("people"), Some("")).address(Some(ThreadId(7))),
            "/ui/inbox/7"
        );
        let state = query(Some("grants"), Some("#12 a&b"));
        assert_eq!(
            state.address(Some(ThreadId(7))),
            "/ui/inbox/7?tab=grants&q=%2312%20a%26b"
        );
        assert_eq!(
            state.showing(&TABS[2]).address(None),
            "/ui/inbox?tab=dibs&q=%2312%20a%26b"
        );
    }
}
