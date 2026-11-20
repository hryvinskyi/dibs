//! Which entries a query finds and in what order: the entries whose text holds every word of the
//! query, grouped by kind in the kinds' order, each kind's in the order its entries were built and
//! cut at a number per kind. A query that is one number brings first the entries of that number,
//! and the kinds that have one before those that do not.

use crate::ui::matching::{matches, number};
use crate::ui::search::index::{Entry, Kind};

/// The hits of one kind.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Group<'a> {
    /// The kind.
    pub(crate) kind: Kind,
    /// Its hits shown, at most the cap.
    pub(crate) hits: Vec<&'a Entry>,
    /// How many more of its entries the query found than are shown.
    pub(crate) more: usize,
}

/// The hits of `query` among `entries`, at most `cap` per kind; none for a query that is empty or
/// only spaces.
pub(crate) fn rank<'a>(entries: &'a [Entry], query: &str, cap: usize) -> Vec<Group<'a>> {
    if query.trim().is_empty() {
        return Vec::new();
    }
    let wanted = number(query);
    let numbered = |entry: &Entry| wanted.is_some() && entry.number == wanted;
    let mut groups: Vec<(bool, Group<'a>)> = Kind::ALL
        .into_iter()
        .filter_map(|kind| {
            let mut hits: Vec<&Entry> = entries
                .iter()
                .filter(|entry| entry.kind == kind && matches(query, &entry.text))
                .collect();
            hits.sort_by_key(|entry| !numbered(entry));
            let leads = hits.first().is_some_and(|entry| numbered(entry));
            let more = hits.len().saturating_sub(cap);
            hits.truncate(cap);
            (!hits.is_empty()).then_some((leads, Group { kind, hits, more }))
        })
        .collect();
    groups.sort_by_key(|(leads, _)| !leads);
    groups.into_iter().map(|(_, group)| group).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(kind: Kind, title: &str, text: &str, number: Option<u64>) -> Entry {
        Entry {
            kind,
            title: title.to_owned(),
            sub: String::new(),
            text: text.to_owned(),
            number,
            here: format!("/ui/{title}"),
            out: None,
        }
    }

    /// A claim on #11 in review with pull request #12, its pull request, the ready #12 and #13,
    /// a branch, an area and a person.
    fn corpus() -> Vec<Entry> {
        vec![
            entry(Kind::Slots, "deploy", "slot deploy 12 minutes", None),
            entry(
                Kind::Claims,
                "claim-11",
                "gh:acme/widgets#11 refunds bob acme/widgets#12",
                Some(11),
            ),
            entry(
                Kind::PullRequests,
                "pr-12",
                "pr pull acme/widgets#12 gh:acme/widgets#11 refunds",
                Some(12),
            ),
            entry(
                Kind::QueueTickets,
                "queue-12",
                "gh:acme/widgets#12 refund rates",
                Some(12),
            ),
            entry(
                Kind::QueueTickets,
                "queue-120",
                "gh:acme/widgets#120 rates",
                Some(120),
            ),
            entry(
                Kind::Branches,
                "bob/11-refunds",
                "branch api bob/11-refunds",
                None,
            ),
            entry(
                Kind::Areas,
                "api:billing",
                "area api:billing src/billing/**",
                None,
            ),
            entry(Kind::People, "bob", "person bob", None),
        ]
    }

    fn titles<'a>(groups: &[Group<'a>]) -> Vec<(Kind, Vec<&'a str>)> {
        groups
            .iter()
            .map(|group| {
                (
                    group.kind,
                    group.hits.iter().map(|hit| hit.title.as_str()).collect(),
                )
            })
            .collect()
    }

    #[test]
    fn without_a_number_the_hits_come_in_kind_order_each_kind_in_build_order() {
        let entries = corpus();
        assert_eq!(
            titles(&rank(&entries, "refund", 50)),
            [
                (Kind::Claims, vec!["claim-11"]),
                (Kind::PullRequests, vec!["pr-12"]),
                (Kind::QueueTickets, vec!["queue-12"]),
                (Kind::Branches, vec!["bob/11-refunds"]),
            ]
        );
    }

    #[test]
    fn a_number_query_brings_the_entries_of_that_number_first_then_the_rest() {
        let entries = corpus();
        let expected = [
            (Kind::PullRequests, vec!["pr-12"]),
            (Kind::QueueTickets, vec!["queue-12", "queue-120"]),
            (Kind::Claims, vec!["claim-11"]),
            (Kind::Slots, vec!["deploy"]),
        ];
        assert_eq!(titles(&rank(&entries, "12", 50)), expected);
        assert_eq!(titles(&rank(&entries, " #12 ", 50)), expected);
    }

    #[test]
    fn within_a_kind_the_entries_of_the_number_come_before_the_others() {
        let entries = vec![
            entry(Kind::QueueTickets, "first", "widgets#120", Some(120)),
            entry(Kind::QueueTickets, "second", "widgets#12", Some(12)),
            entry(Kind::QueueTickets, "third", "widgets#312", Some(312)),
        ];
        assert_eq!(
            titles(&rank(&entries, "12", 50)),
            [(Kind::QueueTickets, vec!["second", "first", "third"])]
        );
        assert_eq!(
            titles(&rank(&entries, "widgets", 50)),
            [(Kind::QueueTickets, vec!["first", "second", "third"])]
        );
    }

    #[test]
    fn every_word_of_a_query_must_be_found() {
        let entries = corpus();
        assert_eq!(
            titles(&rank(&entries, "Rates REFUND", 50)),
            [(Kind::QueueTickets, vec!["queue-12"])]
        );
        assert!(rank(&entries, "refund nothing", 50).is_empty());
    }

    #[test]
    fn an_empty_query_finds_nothing() {
        let entries = corpus();
        assert!(rank(&entries, "", 50).is_empty());
        assert!(rank(&entries, "  \t", 50).is_empty());
    }

    #[test]
    fn a_kind_shows_at_most_the_cap_and_counts_the_rest() {
        let entries: Vec<Entry> = (1..=7)
            .map(|n| entry(Kind::People, &format!("p{n}"), "person chore", None))
            .chain(std::iter::once(entry(Kind::Slots, "s", "slot chore", None)))
            .collect();
        let groups = rank(&entries, "chore", 5);
        assert_eq!(
            titles(&groups),
            [
                (Kind::People, vec!["p1", "p2", "p3", "p4", "p5"]),
                (Kind::Slots, vec!["s"]),
            ]
        );
        assert_eq!(groups[0].more, 2);
        assert_eq!(groups[1].more, 0);
        assert_eq!(rank(&entries, "chore", 50)[0].more, 0);
    }
}
