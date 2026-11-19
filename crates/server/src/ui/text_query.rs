//! The state a list's address keeps on a screen that a text narrows and that shows part of a long
//! list until asked for all of it: the text the rows match (`q`) and whether every row is shown
//! (`all=1`), and the addresses built from it.

use serde::Deserialize;

use crate::ui::links::query_value;
use crate::ui::matching::matches;
use crate::ui::shell::Screen;

/// The query of a list's address as it arrives.
#[derive(Debug, Default, Deserialize)]
pub struct RawQuery {
    q: Option<String>,
    all: Option<String>,
}

/// A list's state on its screen.
#[derive(Debug, Clone)]
pub struct TextQuery {
    /// The screen whose list it is.
    screen: Screen,
    /// The text the rows match; empty for any.
    pub q: String,
    /// Whether every row is shown rather than the first part of the list.
    pub all: bool,
}

impl TextQuery {
    /// The state a query gives the list of `screen`: only `all=1` shows every row.
    pub fn of(screen: Screen, raw: RawQuery) -> Self {
        Self {
            screen,
            q: raw.q.unwrap_or_default(),
            all: raw.all.as_deref() == Some("1"),
        }
    }

    /// Whether the text narrows the list.
    pub fn filtering(&self) -> bool {
        !self.q.trim().is_empty()
    }

    /// Whether the text leaves in the list a row whose words are `text`.
    pub fn leaves(&self, text: &str) -> bool {
        matches(&self.q, text)
    }

    /// Whether every row is shown: asked for, or narrowed by a text.
    pub fn shows_all(&self) -> bool {
        self.all || self.filtering()
    }

    /// The address of the list, or of the object written `selected` beside it, with this state;
    /// each value percent-encoded, and the defaults left out.
    pub fn address(&self, selected: Option<&str>) -> String {
        let path = match selected {
            Some(selected) => format!("{}/{selected}", self.screen.path()),
            None => self.screen.path().to_owned(),
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

    fn query(screen: Screen, q: Option<&str>, all: Option<&str>) -> TextQuery {
        TextQuery::of(
            screen,
            RawQuery {
                q: q.map(str::to_owned),
                all: all.map(str::to_owned),
            },
        )
    }

    #[test]
    fn only_one_shows_every_row() {
        assert!(query(Screen::Queue, None, Some("1")).all);
        for other in [None, Some(""), Some("0"), Some("true"), Some("2")] {
            assert!(!query(Screen::Queue, None, other).all, "{other:?}");
        }
    }

    #[test]
    fn every_row_shows_when_asked_for_or_when_a_text_narrows_the_list() {
        assert!(!query(Screen::Areas, None, None).shows_all());
        assert!(!query(Screen::Areas, Some("  "), None).shows_all());
        assert!(query(Screen::Areas, Some("rates"), None).shows_all());
        assert!(query(Screen::Areas, None, Some("1")).shows_all());
    }

    #[test]
    fn the_text_leaves_the_rows_whose_words_hold_every_word_of_it() {
        let state = query(Screen::Queue, Some("#12 Rates"), None);
        assert!(state.leaves("gh:acme/widgets#12 rates reach the ledger"));
        assert!(!state.leaves("gh:acme/widgets#12 refunds"));
        assert!(query(Screen::Queue, None, None).leaves("anything"));
    }

    #[test]
    fn an_address_keeps_what_differs_from_the_defaults_each_value_encoded() {
        assert_eq!(query(Screen::Queue, None, None).address(None), "/ui/queue");
        assert_eq!(
            query(Screen::Queue, Some(" "), Some("0")).address(Some("7")),
            "/ui/queue/7"
        );
        let state = query(Screen::Queue, Some("#12 a&b"), Some("1"));
        assert_eq!(
            state.address(Some("7")),
            "/ui/queue/7?q=%2312%20a%26b&all=1"
        );
        assert_eq!(
            query(Screen::Areas, Some("x"), None)
                .with_all()
                .address(Some("api:billing")),
            "/ui/areas/api:billing?q=x&all=1"
        );
        assert!(
            state
                .address(None)
                .bytes()
                .all(|byte| byte.is_ascii_graphic())
        );
    }
}
