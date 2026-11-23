use std::collections::{BTreeMap, BTreeSet};

use time::Duration;

use super::{PilotStats, StatsRange, StatsReading, Waits};
use crate::escalation::EscalationSubject;
use crate::event::{
    Escalated, Event, EventKind, GrantEnd, GrantRevoked, Granted, OverlapOrdered, PrStateReported,
    WideGrantedByAck,
};
use crate::grant::GrantDecision;
use crate::ids::{EventId, GrantId, ItemId, ProjectId};
use crate::radar::{OrderRule, OverlapKind, OverlapOrder};
use crate::report::ReportDay;
use crate::review::PrRef;
use crate::testing::{ALICE, BOB, area};

fn day(text: &str) -> ReportDay {
    ReportDay::parse(text).unwrap()
}

/// The two days the readings below cover: 2026-03-02 and 2026-03-03.
fn range() -> StatsRange {
    StatsRange::new(day("2026-03-02"), day("2026-03-03")).unwrap()
}

/// An event `minutes` after the range starts; a negative count falls before it.
fn event(id: u64, minutes: i64, kind: EventKind) -> Event {
    Event {
        id: EventId(id),
        project: ProjectId(1),
        at: day("2026-03-02").start() + Duration::minutes(minutes),
        kind,
    }
}

/// A grant decided so at `minutes`, asked for `waited` minutes before it when `waited` is some.
fn granted(id: u64, minutes: i64, decided: GrantDecision, waited: Option<i64>) -> Event {
    let at = day("2026-03-02").start() + Duration::minutes(minutes);
    event(
        id,
        minutes,
        EventKind::Granted(Granted {
            grant: GrantId(id),
            item: ItemId(1),
            area: area("default:billing"),
            from: ALICE,
            to: BOB,
            decided,
            asked_at: waited.map(|waited| at - Duration::minutes(waited)),
        }),
    )
}

fn reported(id: u64, minutes: i64, pr: &str, mergeable: bool) -> Event {
    event(
        id,
        minutes,
        EventKind::PrStateReported(PrStateReported {
            pr: PrRef::parse(pr).unwrap(),
            mergeable,
        }),
    )
}

fn acknowledged(id: u64, decided: GrantDecision) -> Event {
    event(
        id,
        10,
        EventKind::WideGrantedByAck(WideGrantedByAck {
            grant: GrantId(id),
            item: ItemId(1),
            area: area("default:billing"),
            by: ALICE,
            to: BOB,
            decided,
        }),
    )
}

fn ended(id: u64, cause: GrantEnd) -> Event {
    event(
        id,
        20,
        EventKind::GrantRevoked(GrantRevoked {
            grant: GrantId(id),
            item: ItemId(1),
            area: area("default:billing"),
            from: ALICE,
            to: BOB,
            cause,
            by: None,
        }),
    )
}

fn ordered(id: u64, order: OverlapOrder, changed: bool) -> Event {
    event(
        id,
        40,
        EventKind::OverlapOrdered(OverlapOrdered {
            kind: OverlapKind::SameFile,
            items: BTreeSet::from([ItemId(1), ItemId(2)]),
            order,
            changed,
        }),
    )
}

fn escalated(id: u64, by_dibs: bool) -> Event {
    event(
        id,
        50,
        EventKind::Escalated(Escalated {
            subject: EscalationSubject::Item(ItemId(1)),
            by: ALICE,
            by_dibs,
        }),
    )
}

fn of(events: &[Event]) -> PilotStats {
    PilotStats::of(&range(), events, &[])
}

fn waits(count: u64, median: i64, max: i64) -> Waits {
    Waits {
        count,
        median: Some(Duration::minutes(median)),
        max: Some(Duration::minutes(max)),
    }
}

#[test]
fn grants_are_counted_by_who_decided_and_only_the_decisions_seen_are_keys() {
    let stats = of(&[
        granted(1, 1, GrantDecision::Holder, None),
        granted(2, 2, GrantDecision::Holder, Some(5)),
        granted(3, 3, GrantDecision::InReview, Some(0)),
        granted(4, 4, GrantDecision::Waited, Some(10)),
        granted(5, -1, GrantDecision::Idle, None),
    ]);
    assert_eq!(
        stats.grants,
        BTreeMap::from([
            (GrantDecision::Holder, 2),
            (GrantDecision::InReview, 1),
            (GrantDecision::Waited, 1),
        ]),
        "a grant before the range is left out"
    );
}

#[test]
fn acknowledgements_of_wide_claims_are_counted_by_who_decided() {
    let stats = of(&[
        acknowledged(1, GrantDecision::Holder),
        acknowledged(2, GrantDecision::Idle),
        acknowledged(3, GrantDecision::Idle),
    ]);
    assert_eq!(
        stats.acknowledgements,
        BTreeMap::from([(GrantDecision::Holder, 1), (GrantDecision::Idle, 2)])
    );
    assert!(stats.grants.is_empty(), "an acknowledgement is not a grant");
}

#[test]
fn a_grant_whose_request_was_asked_before_the_range_counts_but_is_not_timed() {
    let stats = of(&[
        granted(1, 30, GrantDecision::Waited, Some(10)),
        granted(2, 40, GrantDecision::Waited, Some(3 * 24 * 60)),
        granted(3, 50, GrantDecision::Holder, Some(51)),
    ]);
    assert_eq!(
        stats.grant_waits,
        BTreeMap::from([(GrantDecision::Waited, waits(1, 10, 10))]),
        "a wait is timed only when it began inside the range, as a slot wait or a conflict is"
    );
    assert_eq!(stats.grants[&GrantDecision::Waited], 2);
    assert_eq!(stats.grants[&GrantDecision::Holder], 1);
}

#[test]
fn only_a_grant_its_grantor_or_an_admin_revoked_counts_as_revoked() {
    let stats = of(&[
        ended(1, GrantEnd::Revoked),
        ended(2, GrantEnd::GrantorLeft),
        ended(3, GrantEnd::GranteeLeft),
        ended(4, GrantEnd::Revoked),
    ]);
    assert_eq!(stats.revoked, 2);
}

#[test]
fn a_grant_that_answered_a_request_is_timed_from_the_request_under_its_own_decision() {
    let stats = of(&[
        granted(1, 30, GrantDecision::Waited, Some(10)),
        granted(2, 31, GrantDecision::Waited, Some(30)),
        granted(3, 32, GrantDecision::Waited, Some(20)),
        granted(4, 33, GrantDecision::InReview, Some(0)),
        granted(5, 34, GrantDecision::Holder, Some(4)),
        granted(6, 35, GrantDecision::Holder, Some(8)),
        granted(7, 36, GrantDecision::Holder, None),
    ]);
    assert_eq!(
        stats.grant_waits,
        BTreeMap::from([
            (GrantDecision::Waited, waits(3, 20, 30)),
            (GrantDecision::InReview, waits(1, 0, 0),),
            (GrantDecision::Holder, waits(2, 6, 8)),
        ]),
        "a grant at the ask waits nothing and is not pooled with the waited ones; a grant nobody \
         asked for is not timed"
    );
    assert_eq!(stats.grants[&GrantDecision::Holder], 3);
}

#[test]
fn orders_are_counted_by_the_rule_that_decided_and_a_replaced_one_as_changed() {
    let first = |because| OverlapOrder::First {
        item: ItemId(1),
        because,
    };
    let stats = of(&[
        ordered(1, OverlapOrder::Tied, false),
        ordered(2, first(OrderRule::InReview), true),
        ordered(3, first(OrderRule::Older), false),
        ordered(4, first(OrderRule::Older), false),
        ordered(5, OverlapOrder::Tied, true),
    ]);
    assert_eq!(
        stats.orders,
        BTreeMap::from([("tied", 2), ("in_review", 1), ("older", 2)])
    );
    assert_eq!(stats.orders_changed, 2);
}

#[test]
fn escalations_dibs_raised_on_its_own_are_counted_apart_and_among_all() {
    let stats = of(&[escalated(1, true), escalated(2, false), escalated(3, true)]);
    assert_eq!((stats.escalations, stats.escalations_by_dibs), (3, 2));
}

#[test]
fn a_conflict_reported_and_resolved_in_the_range_is_timed() {
    let stats = of(&[
        reported(1, 60, "acme/widgets#7", false),
        reported(2, 90, "acme/widgets#7", true),
    ]);
    assert_eq!(stats.conflicts_resolved, waits(1, 30, 30));
    assert_eq!(stats.conflicting_reports, 1);
}

#[test]
fn a_conflict_reported_before_the_range_is_not_counted() {
    let stats = of(&[
        reported(1, -30, "acme/widgets#7", false),
        reported(2, 30, "acme/widgets#7", true),
    ]);
    assert_eq!(stats.conflicts_resolved, Waits::default());
}

#[test]
fn a_conflict_still_open_at_the_end_of_the_range_is_not_counted() {
    let stats = of(&[
        reported(1, 60, "acme/widgets#7", false),
        reported(2, 60 * 48 + 5, "acme/widgets#7", true),
    ]);
    assert_eq!(stats.conflicts_resolved, Waits::default());
    assert_eq!(stats.conflicting_reports, 1);
}

#[test]
fn interleaved_conflicts_of_two_pull_requests_are_each_ended_by_their_own_report() {
    let stats = of(&[
        reported(1, 10, "acme/widgets#7", false),
        reported(2, 20, "acme/widgets#8", false),
        reported(3, 25, "acme/widgets#8", false),
        reported(4, 30, "acme/widgets#7", true),
        reported(5, 80, "acme/widgets#8", true),
        reported(6, 85, "acme/widgets#8", true),
    ]);
    assert_eq!(
        stats.conflicts_resolved,
        Waits {
            count: 2,
            median: Some(Duration::minutes(40)),
            max: Some(Duration::minutes(60)),
        },
        "#7 lasts 20 minutes and #8 60, from its first report; a second clean report ends nothing"
    );
}

#[test]
fn the_conflicts_under_way_carry_over_from_one_page_of_events_to_the_next() {
    let mut reading = StatsReading::new(range());
    reading.events(&[reported(1, 10, "acme/widgets#7", false)]);
    reading.events(&[reported(2, 25, "acme/widgets#7", true)]);
    assert_eq!(reading.finish(&[]).conflicts_resolved, waits(1, 15, 15));
}

#[test]
fn an_empty_range_reads_as_no_figures_at_all() {
    assert_eq!(of(&[]), PilotStats::default());
    let outside = of(&[
        granted(1, -5, GrantDecision::Waited, Some(10)),
        reported(2, -5, "acme/widgets#7", false),
    ]);
    assert_eq!(outside, PilotStats::default());
}

#[test]
fn every_kind_these_figures_read_is_among_the_names_the_store_is_asked_for() {
    let read = [
        granted(1, 0, GrantDecision::Holder, None),
        acknowledged(2, GrantDecision::Holder),
        ended(3, GrantEnd::Revoked),
        ordered(4, OverlapOrder::Tied, false),
        escalated(5, true),
        reported(6, 0, "acme/widgets#7", false),
    ];
    for event in &read {
        assert!(
            PilotStats::EVENT_NAMES.contains(&event.kind.name()),
            "{}",
            event.kind.name()
        );
    }
    let names: BTreeSet<&str> = PilotStats::EVENT_NAMES.into_iter().collect();
    assert_eq!(names.len(), PilotStats::EVENT_NAMES.len(), "no name twice");
}
