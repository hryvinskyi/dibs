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
}
