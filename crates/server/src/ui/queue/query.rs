//! The list's state as its address keeps it — the text the tickets match (`q`) and whether every row
//! is shown (`all=1`) — and the addresses built from it.

use serde::Deserialize;

use dibs_domain::ids::ItemId;

use crate::ui::links::query_value;
use crate::ui::matching::matches;
use crate::ui::queue::view::QueueItemView;
use crate::ui::shell::Screen;

/// The query of the list's address as it arrives.
#[derive(Debug, Default, Deserialize)]
pub struct RawQuery {
    q: Option<String>,
    all: Option<String>,
}

/// The list's state.
#[derive(Debug, Clone)]
pub struct ListQuery {
    /// The text the tickets match; empty for any.
    pub q: String,
    /// Whether every row is shown rather than the first of each group.
    pub all: bool,
}

impl ListQuery {
    /// The state a query gives: only `all=1` shows every row.
    pub fn of(raw: RawQuery) -> Self {
        Self {
            q: raw.q.unwrap_or_default(),
            all: raw.all.as_deref() == Some("1"),
        }
    }

    /// Whether the text narrows the list.
    pub fn filtering(&self) -> bool {
        !self.q.trim().is_empty()
    }

    /// Whether the text leaves the ticket in the list.
    pub fn leaves(&self, item: &QueueItemView) -> bool {
        matches(&self.q, &item.text())
    }

    /// Whether a group shows all its rows: asked for, or narrowed by a text.
    pub fn shows_every_row(&self) -> bool {
        self.all || self.filtering()
    }

    /// The address of the list, or of the ticket `item` beside it, with this state; each value
    /// percent-encoded, and the defaults left out.
    pub fn address(&self, item: Option<ItemId>) -> String {
        let path = match item {
            Some(item) => format!("{}/{item}", Screen::Queue.path()),
            None => Screen::Queue.path().to_owned(),
        };
        let mut pairs = Vec::new();
        if self.filtering() {
            pairs.push(format!("q={}", query_value(&self.q)));
        }
        if self.all {
            pairs.push("all=1".to_owned());
        }
        if pairs.is_empty() {
            path
        } else {
            format!("{path}?{}", pairs.join("&"))
        }
    }

    /// The same state showing every row.
    pub fn with_all(&self) -> Self {
        Self {
            all: true,
            ..self.clone()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn query(q: Option<&str>, all: Option<&str>) -> ListQuery {
        ListQuery::of(RawQuery {
            q: q.map(str::to_owned),
            all: all.map(str::to_owned),
        })
    }

    #[test]
    fn only_one_shows_every_row() {
        assert!(query(None, Some("1")).all);
        for other in [None, Some(""), Some("0"), Some("true"), Some("2")] {
            assert!(!query(None, other).all, "{other:?}");
        }
    }

    #[test]
    fn a_group_shows_every_row_when_asked_to_or_when_a_text_narrows_it() {
        assert!(!query(None, None).shows_every_row());
        assert!(!query(Some("  "), None).shows_every_row());
        assert!(query(Some("rates"), None).shows_every_row());
        assert!(query(None, Some("1")).shows_every_row());
    }

    #[test]
    fn an_address_keeps_what_differs_from_the_defaults_each_value_encoded() {
        assert_eq!(query(None, None).address(None), "/ui/queue");
        assert_eq!(
            query(Some(" "), Some("0")).address(Some(ItemId(7))),
            "/ui/queue/7"
        );
        let state = query(Some("#12 a&b"), Some("1"));
        assert_eq!(
            state.address(Some(ItemId(7))),
            "/ui/queue/7?q=%2312%20a%26b&all=1"
        );
        assert_eq!(
            query(Some("x"), None).with_all().address(None),
            "/ui/queue?q=x&all=1"
        );
        assert!(
            state
                .address(None)
                .bytes()
                .all(|byte| byte.is_ascii_graphic())
        );
    }
}
