//! Text the guard hands to a host: a brief, a note, a notice or the reason of a refusal.

use std::fmt;

use serde::{Deserialize, Serialize};

/// Text for the person or the agent on the other side of a host. Control characters other than
/// line breaks and tabs are dropped when it is made, so nothing a server or a teammate wrote can
/// steer a terminal or forge the structure of a host's reply.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(from = "String", into = "String")]
pub struct Text(String);

impl Text {
    /// The text, with control characters other than `\n` and `\t` removed.
    pub fn new(value: impl Into<String>) -> Self {
        let value: String = value.into();
        Self(
            value
                .chars()
                .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
                .collect(),
        )
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
