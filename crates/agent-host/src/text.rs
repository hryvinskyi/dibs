//! Text the guard hands to a host: a brief, a note, a notice or the reason of a refusal.

use std::fmt;

use serde::{Deserialize, Serialize};
use unicode_general_category::{GeneralCategory, get_general_category};

/// Whether a character shows as itself: a line break or a tab, or any character that is not a
/// control character (general category Cc), a format character (Cf: the bidirectional overrides
/// and isolates, the zero-width characters, the byte order mark, the tag characters), a
/// private-use character (Co), a surrogate (Cs) or a code point Unicode has not assigned (Cn).
/// Line and paragraph separators are dropped too: a member's text keeps its own line breaks only.
pub fn shows(c: char) -> bool {
    if c == '\n' || c == '\t' {
        return true;
    }
    if ('\u{E0000}'..='\u{E007F}').contains(&c) {
        return false;
    }
    !matches!(
        get_general_category(c),
        GeneralCategory::Control
            | GeneralCategory::Format
            | GeneralCategory::PrivateUse
            | GeneralCategory::Surrogate
            | GeneralCategory::Unassigned
            | GeneralCategory::LineSeparator
            | GeneralCategory::ParagraphSeparator
    )
}

/// Text for the person or the agent on the other side of a host. Only characters that show as
/// themselves are kept when it is made — line breaks, tabs, and no control, format, private-use or
/// unassigned character — so nothing a server or a teammate wrote can steer a terminal, hide text
/// from the person reading it, reorder it on screen or forge the structure of a host's reply.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(from = "String", into = "String")]
pub struct Text(String);

impl Text {
    /// The text, with every character [`shows`] refuses removed.
    pub fn new(value: impl Into<String>) -> Self {
        let value: String = value.into();
        Self(value.chars().filter(|c| shows(*c)).collect())
    }

    /// The text.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The text on one line: line breaks and tabs become spaces, runs of spaces one space.
    pub fn one_line(&self) -> String {
        self.0.split_whitespace().collect::<Vec<_>>().join(" ")
    }
}

impl fmt::Display for Text {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<String> for Text {
    fn from(value: String) -> Self {
        Self::new(value)
    }
}

impl From<&str> for Text {
    fn from(value: &str) -> Self {
        Self::new(value)
    }
}

impl From<Text> for String {
    fn from(value: Text) -> Self {
        value.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_does_not_show_as_itself_is_dropped_class_by_class() {
        for (class, hidden) in [
            (
                "bidirectional overrides",
                "\u{202A}\u{202B}\u{202C}\u{202D}\u{202E}",
            ),
            ("bidirectional isolates", "\u{2066}\u{2067}\u{2068}\u{2069}"),
            (
                "zero-width characters",
                "\u{200B}\u{200C}\u{200D}\u{2060}\u{FEFF}",
            ),
            ("tag characters", "\u{E0000}\u{E0001}\u{E0041}\u{E007F}"),
            ("private use", "\u{E000}\u{F8FF}\u{F0000}\u{10FFFD}"),
            ("unassigned", "\u{0378}\u{FFFF}\u{2FFFE}"),
            ("C1 controls", "\u{0085}\u{009B}"),
            ("separators", "\u{2028}\u{2029}"),
        ] {
            let text = Text::new(format!("a{hidden}b"));
            assert_eq!(text.as_str(), "ab", "{class}");
        }
        assert_eq!(
            Text::new("é ü 日本 ✓").as_str(),
            "é ü 日本 ✓",
            "letters stay"
        );
    }

    #[test]
    fn control_characters_are_dropped_but_lines_stay() {
        let text = Text::new("a\u{1b}[31mred\u{7}\nnext\tcell");
        assert_eq!(text.as_str(), "a[31mred\nnext\tcell");
        assert_eq!(text.one_line(), "a[31mred next cell");
    }

    #[test]
    fn text_read_from_json_is_cleaned_too() {
        let text: Text = serde_json::from_str("\"x\\u0000y\"").unwrap();
        assert_eq!(text.as_str(), "xy");
    }
}
