//! How a list's filter text and the search match an entry: by the words of the query, each found
//! somewhere in the entry's text, whatever their case and order.

/// Whether every word of `query` occurs in `text`, ignoring case; a word's leading `#` is dropped.
/// An empty query matches.
pub fn matches(query: &str, text: &str) -> bool {
    let text = text.to_lowercase();
    query
        .split_whitespace()
        .map(|word| word.strip_prefix('#').unwrap_or(word).to_lowercase())
        .all(|word| text.contains(&word))
}

/// The number a query is, when it is one number with or without a leading `#`.
pub fn number(query: &str) -> Option<u64> {
    let digits = query.trim();
    let digits = digits.strip_prefix('#').unwrap_or(digits);
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_word_must_occur_in_any_order_and_any_case() {
        let text = "gh:acme/widgets#874 Refund Rates alice api:billing";
        assert!(matches("rates refund", text));
        assert!(matches("ALICE Billing", text));
        assert!(!matches("rates bob", text));
        assert!(!matches("search", text));
    }

    #[test]
    fn a_words_leading_hash_is_dropped() {
        assert!(matches("#874", "widgets#874"));
        assert!(matches("#874", "pull request 874"));
        assert!(!matches("#875", "widgets#874"));
    }

    #[test]
    fn an_empty_query_and_one_of_spaces_match_everything() {
        assert!(matches("", "anything"));
        assert!(matches("   \t ", "anything"));
        assert!(matches("", ""));
    }

    #[test]
    fn a_query_of_one_number_with_or_without_a_hash_is_that_number() {
        assert_eq!(number("874"), Some(874));
        assert_eq!(number("#874"), Some(874));
        assert_eq!(number(" 874 "), Some(874));
        assert_eq!(number(" #0012\t"), Some(12));
    }

    #[test]
    fn anything_else_is_no_number() {
        for query in [
            "874a",
            "two words",
            "#",
            "",
            "   ",
            "12 13",
            "##12",
            "-3",
            "+3",
            "# 12",
        ] {
            assert_eq!(number(query), None, "{query:?}");
        }
        assert_eq!(
            number("99999999999999999999999"),
            None,
            "too large for a number"
        );
    }
}
