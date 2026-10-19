//! Character rules shared by the validated text types of this crate.

/// Whether `value` is a slug: 1 to `max_len` characters, the first a lowercase ASCII letter or digit,
/// the rest lowercase ASCII letters, digits or the characters in `extra`.
pub(crate) fn is_slug(value: &str, max_len: usize, extra: &[char]) -> bool {
    let mut chars = value.chars();
    let first_ok = chars
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
    first_ok
        && value.len() <= max_len
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || extra.contains(&c))
}

/// Whether `value` contains a control character (NUL, newline, tab, escape and the like).
pub(crate) fn has_control(value: &str) -> bool {
    value.chars().any(char::is_control)
}

/// Implements the conversions every validated string type of this crate shares, given that the type
/// is a tuple struct over `String` with an associated `parse(&str) -> Result<Self, $error>`.
macro_rules! string_value {
    ($type:ident, $error:ty) => {
        impl $type {
            /// The validated text.
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl ::std::fmt::Display for $type {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl ::std::convert::TryFrom<String> for $type {
            type Error = $error;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                Self::parse(&value)
            }
        }

        impl ::std::convert::From<$type> for String {
            fn from(value: $type) -> String {
                value.0
            }
        }

        impl ::std::str::FromStr for $type {
            type Err = $error;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Self::parse(value)
            }
        }
    };
}

pub(crate) use string_value;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_rules() {
        let cases: &[(&str, bool)] = &[
            ("a", true),
            ("a-b", true),
            ("0a", true),
            ("", false),
            ("-a", false),
            ("A", false),
            ("a_b", false),
            ("abcd", true),
            ("abcde", false),
        ];
        for (value, expected) in cases {
            assert_eq!(is_slug(value, 4, &['-']), *expected, "{value:?}");
        }
    }

    #[test]
    fn control_characters_are_detected() {
        assert!(has_control("a\u{0}b"));
        assert!(has_control("a\nb"));
        assert!(!has_control("a b"));
    }
}
