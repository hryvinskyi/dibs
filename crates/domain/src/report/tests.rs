use std::collections::BTreeMap;

use time::{Duration, Month};

use super::stats::{PilotStats, StatsRange, StatsRangeError};
use super::*;
use crate::event::{
    Claimed, Event, EventKind, FeedbackGiven, PrStateReported, SlotMember, SlotQueued, SlotTaken,
    TakenOver,
};
use crate::ids::{EventId, ItemId, ProjectId, SessionId};
use crate::review::PrRef;
use crate::slot::{Purpose, SlotName};
use crate::testing::{ALICE, BOB, area};

fn day(text: &str) -> ReportDay {
    ReportDay::parse(text).unwrap()
}

fn durations(agent: &str, event: &str, counts: [u64; BUCKETS]) -> HookDurations {
    HookDurations {
        agent: AgentLabel::parse(agent).unwrap(),
        event: HookEventName::parse(event).unwrap(),
        buckets: DurationBuckets::new(counts),
    }
}

#[test]
fn a_day_is_written_and_read_as_its_calendar_date() {
    let read = day("2026-03-09");
    assert_eq!(
        read.date(),
        Date::from_calendar_date(2026, Month::March, 9).unwrap()
    );
    assert_eq!(read.to_string(), "2026-03-09");
    assert_eq!(read.next().to_string(), "2026-03-10");
    assert_eq!(read.minus_days(9).to_string(), "2026-02-28");
    assert_eq!(read.start().to_string(), "2026-03-09 0:00:00.0 +00:00:00");
    for bad in [
        "2026-3-09",
        "2026-02-30",
        "26-03-09",
        "2026-03-09T00:00",
        "",
        "2026/03/09",
    ] {
        assert!(ReportDay::parse(bad).is_err(), "{bad}");
    }
    let at = OffsetDateTime::UNIX_EPOCH + Duration::days(3) + Duration::hours(23);
    assert_eq!(ReportDay::of(at).to_string(), "1970-01-04");
    assert_eq!(
        serde_json::to_value(read).unwrap(),
        serde_json::json!("2026-03-09")
    );
}

#[test]
fn every_kind_has_one_name() {
    for tally in Tally::ALL {
        assert_eq!(Tally::parse(tally.name()), Some(tally));
        assert_eq!(
            serde_json::to_value(tally).unwrap(),
            serde_json::json!(tally.name())
        );
    }
    assert_eq!(Tally::parse("held_by_someone"), None);
}

#[test]
fn a_refusal_id_is_r_and_a_few_hex_digits() {
    assert_eq!(RefusalId::from_bits(0x7f_3a51).as_str(), "r-7f3a51");
    assert_eq!(RefusalId::from_bits(0x0001).as_str(), "r-000001");
    assert_eq!(RefusalId::from_bits(0xab7f_3a51).as_str(), "r-7f3a51");
    assert_eq!(RefusalId::parse(" r-7f3a ").unwrap().as_str(), "r-7f3a");
    assert!(RefusalId::parse("r-deadbeef").is_ok());
    for bad in ["7f3a", "r-7F3A", "r-7f3", "r-123456789", "r-xyz1", "R-7f3a"] {
        assert!(RefusalId::parse(bad).is_err(), "{bad}");
    }
}

#[test]
fn a_hook_event_name_and_a_reason_are_checked() {
    assert!(HookEventName::parse("before_tool").is_ok());
    for bad in ["", "Before", "_x", "before-tool", &"a".repeat(33)] {
        assert!(HookEventName::parse(bad).is_err(), "{bad}");
    }
    assert_eq!(
        FeedbackReason::parse("  it was free\n").unwrap().as_str(),
        "it was free"
    );
    assert_eq!(
        FeedbackReason::parse("the area\nwas\r\nfree\tagain")
            .unwrap()
            .as_str(),
        "the area was  free again",
        "one line: breaks and tabs fold to spaces"
    );
    assert!(FeedbackReason::parse("   ").is_err());
    assert!(FeedbackReason::parse(&"x".repeat(280)).is_ok());
    assert!(FeedbackReason::parse(&"x".repeat(281)).is_err());
    assert!(FeedbackReason::parse("a\u{7}b").is_err());
}

#[test]
fn buckets_hold_each_run_in_the_first_that_fits_and_name_the_95th_percentile() {
    let mut buckets = DurationBuckets::default();
    for millis in [0, 10, 11, 30, 31, 300, 1_000, 2_000, 2_001, 60_000] {
        buckets.record(Duration::milliseconds(millis));
    }
    assert_eq!(buckets.counts(), [2, 2, 1, 1, 1, 1, 2]);
    assert_eq!(buckets.total(), 10);
    let mut fast = DurationBuckets::new([95, 0, 0, 5, 0, 0, 0]);
    assert_eq!(fast.p95_bound_ms(), Some(10));
    fast.add(&DurationBuckets::new([0, 0, 0, 1, 0, 0, 0]));
    assert_eq!(fast.p95_bound_ms(), Some(300));
    assert_eq!(DurationBuckets::default().p95_bound_ms(), None);
    assert_eq!(
        DurationBuckets::new([0, 0, 0, 0, 0, 0, 1]).p95_bound_ms(),
        None
    );
}

#[test]
fn counts_are_kept_in_one_form_whatever_order_they_came_in() {
    let made = DayCounts::new(
        BTreeMap::from([(Tally::HeldBy, 2), (Tally::Frozen, 0)]),
        vec![
            durations("neutral", "before_tool", [1, 0, 0, 0, 0, 0, 0]),
            durations("cli", "after_tool", [0, 0, 0, 0, 0, 0, 0]),
            durations("agent", "after_tool", [0, 2, 0, 0, 0, 0, 0]),
        ],
    )
    .unwrap();
    assert_eq!(made.counts, BTreeMap::from([(Tally::HeldBy, 2)]));
    let order: Vec<&str> = made.durations.iter().map(|d| d.agent.as_str()).collect();
    assert_eq!(
        order,
        ["agent", "neutral"],
        "ordered, and a hook that never ran left out"
    );
    assert_eq!(
        DayCounts::new(
            BTreeMap::new(),
            vec![
                durations("neutral", "before_tool", [1, 0, 0, 0, 0, 0, 0]),
                durations("neutral", "before_tool", [2, 0, 0, 0, 0, 0, 0]),
            ]
        ),
        Err(DayCountsError::RepeatedDurations)
    );
    let many: Vec<HookDurations> = (0..65)
        .map(|n| durations(&format!("agent-{n}"), "before_tool", [1, 0, 0, 0, 0, 0, 0]))
        .collect();
    assert_eq!(
        DayCounts::new(BTreeMap::new(), many),
        Err(DayCountsError::TooManyDurations)
    );
    assert!(DayCounts::default().is_empty());
}

#[test]
fn a_report_changes_what_is_kept_only_when_its_counts_differ() {
    let counts = DayCounts::new(BTreeMap::from([(Tally::Unreachable, 3)]), Vec::new()).unwrap();
    let report = HookReport {
        member: ALICE,
        machine: MachineId::from_bits(1),
        host: HostName::parse("laptop").unwrap(),
        report: DayReport {
            day: day("2026-03-02"),
            counts: counts.clone(),
        },
    };
    assert!(report.changes(None));
    assert!(!report.changes(Some(&counts)));
    assert!(report.changes(Some(&DayCounts::default())));
    let wire = serde_json::to_value(&report.report).unwrap();
    assert_eq!(
        wire,
        serde_json::json!({"day": "2026-03-02", "counts": {"unreachable": 3}, "durations": []})
    );
}

fn event(id: u64, minutes: i64, kind: EventKind) -> Event {
    Event {
        id: EventId(id),
        project: ProjectId(1),
        at: day("2026-03-02").start() + Duration::minutes(minutes),
        kind,
    }
}

fn slot(name: &str) -> SlotName {
    SlotName::parse(name).unwrap()
}

fn purpose() -> Purpose {
    Purpose::parse("release").unwrap()
}

fn report(member: crate::ids::MemberId, host: &str, on: &str, counts: DayCounts) -> HookReport {
    HookReport {
        member,
        machine: MachineId::from_bits(u128::try_from(host.len()).unwrap()),
        host: HostName::parse(host).unwrap(),
        report: DayReport {
            day: day(on),
            counts,
        },
    }
}

#[test]
fn the_figures_of_a_range_come_from_its_events_and_its_days_reports() {
    let range = StatsRange::new(day("2026-03-02"), day("2026-03-03")).unwrap();
    let claimed = |id, minutes| {
        event(
            id,
            minutes,
            EventKind::Claimed(Claimed {
                item: ItemId(id),
                member: ALICE,
                session: SessionId(1),
                host: HostName::parse("laptop").unwrap(),
                areas: std::collections::BTreeSet::new(),
            }),
        )
    };
    let queued = |id, minutes, member, name: &str| {
        event(
            id,
            minutes,
            EventKind::SlotQueued(SlotQueued {
                slot: slot(name),
                member,
                purpose: purpose(),
            }),
        )
    };
    let taken = |id, minutes, member, name: &str| {
        event(
            id,
            minutes,
            EventKind::SlotTaken(SlotTaken {
                slot: slot(name),
                member,
                purpose: purpose(),
            }),
        )
    };
    let events = vec![
        claimed(1, -10),
        claimed(2, 5),
        claimed(3, 60 * 47),
        claimed(4, 60 * 48),
        event(
            5,
            6,
            EventKind::TakenOver(TakenOver {
                item: ItemId(2),
                member: BOB,
                previous: ALICE,
                session: SessionId(2),
                host: HostName::parse("desk").unwrap(),
                areas: std::collections::BTreeSet::new(),
            }),
        ),
        queued(6, 10, BOB, "deploy"),
        queued(7, 12, ALICE, "deploy"),
        taken(8, 20, BOB, "deploy"),
        event(
            9,
            21,
            EventKind::SlotLeft(SlotMember {
                slot: slot("deploy"),
                member: ALICE,
            }),
        ),
        taken(10, 22, ALICE, "deploy"),
        queued(11, 30, ALICE, "deploy"),
        taken(12, 60, ALICE, "deploy"),
        queued(13, 61, BOB, "deploy"),
        taken(14, 65, BOB, "deploy"),
        event(
            15,
            70,
            EventKind::SlotStalled(SlotMember {
                slot: slot("deploy"),
                member: BOB,
            }),
        ),
        event(
            16,
            80,
            EventKind::FeedbackGiven(FeedbackGiven {
                member: ALICE,
                about: Feedback::FalseRefusal {
                    refusal: RefusalId::parse("r-0a0b").unwrap(),
                    tally: Some(Tally::HeldBy),
                },
                reason: FeedbackReason::parse("released already").unwrap(),
            }),
        ),
        event(
            17,
            90,
            EventKind::FeedbackGiven(FeedbackGiven {
                member: BOB,
                about: Feedback::Conflict {
                    repo: crate::repo::RepoId::implicit(),
                    area: Some(area("default:billing")),
                },
                reason: FeedbackReason::parse("both edited rounding").unwrap(),
            }),
        ),
        event(
            18,
            100,
            EventKind::PrStateReported(PrStateReported {
                pr: PrRef::parse("acme/widgets#7").unwrap(),
                mergeable: false,
            }),
        ),
        event(
            19,
            101,
            EventKind::PrStateReported(PrStateReported {
                pr: PrRef::parse("acme/widgets#7").unwrap(),
                mergeable: true,
            }),
        ),
        event(
            20,
            102,
            EventKind::PrStateReported(PrStateReported {
                pr: PrRef::parse("acme/widgets#7").unwrap(),
                mergeable: false,
            }),
        ),
    ];
    let counted = |pairs: &[(Tally, u64)], timing: Vec<HookDurations>| {
        DayCounts::new(pairs.iter().copied().collect(), timing).unwrap()
    };
    let reports = vec![
        report(
            ALICE,
            "laptop",
            "2026-03-02",
            counted(
                &[(Tally::HeldBy, 2), (Tally::Unreachable, 1)],
                vec![durations("neutral", "before_tool", [90, 5, 0, 0, 0, 0, 0])],
            ),
        ),
        report(
            ALICE,
            "desk",
            "2026-03-02",
            counted(
                &[(Tally::HeldBy, 1)],
                vec![durations("neutral", "before_tool", [0, 0, 5, 0, 0, 0, 0])],
            ),
        ),
        report(
            BOB,
            "box",
            "2026-03-03",
            counted(&[(Tally::SwitchedOff, 1)], Vec::new()),
        ),
        report(
            BOB,
            "box",
            "2026-03-04",
            counted(&[(Tally::NeedsSlot, 9)], Vec::new()),
        ),
    ];
    let stats = PilotStats::of(&range, &events, &reports);
    assert_eq!(
        stats.claims, 2,
        "one before the range and one after it are left out"
    );
    assert_eq!(
        (stats.takeovers, stats.stalls, stats.escalations),
        (1, 1, 0)
    );
    assert_eq!(
        stats.slot_waits.count, 3,
        "a wait given up and a direct take count nothing"
    );
    assert_eq!(stats.slot_waits.median, Some(Duration::minutes(10)));
    assert_eq!(stats.slot_waits.max, Some(Duration::minutes(30)));
    assert_eq!((stats.false_refusals, stats.conflicts), (1, 1));
    assert_eq!(stats.feedback.len(), 2);
    assert_eq!(stats.feedback[0].member, BOB, "newest first");
    assert_eq!(stats.conflicting_reports, 2);
    assert_eq!(stats.conflicting_prs.len(), 1);
    assert_eq!(
        stats.reported,
        BTreeMap::from([
            (Tally::HeldBy, 3),
            (Tally::Unreachable, 1),
            (Tally::SwitchedOff, 1)
        ]),
        "the day after the range is left out"
    );
    assert_eq!(stats.member_days.len(), 2);
    assert_eq!(stats.member_days[0].member, ALICE);
    assert_eq!(
        stats.member_days[0].counts[&Tally::HeldBy],
        3,
        "both machines added"
    );
    assert_eq!(stats.durations.len(), 1);
    assert_eq!(stats.durations[0].runs(), 100);
    assert_eq!(stats.durations[0].p95_ms(), Some(30));
    assert_eq!(stats.member_durations.len(), 1);
    assert_eq!(stats.member_durations[0].member, Some(ALICE));
    for event in &events {
        if !matches!(event.kind, EventKind::Claimed(_) | EventKind::TakenOver(_)) {
            continue;
        }
        assert!(PilotStats::EVENT_NAMES.contains(&event.kind.name()));
    }
}

#[test]
fn every_kind_the_figures_read_is_among_the_names_the_store_is_asked_for() {
    let range = StatsRange::week_to(day("2026-03-08"));
    let kinds = [
        EventKind::SlotStalled(SlotMember {
            slot: slot("deploy"),
            member: ALICE,
        }),
        EventKind::SlotLeft(SlotMember {
            slot: slot("deploy"),
            member: ALICE,
        }),
        EventKind::SlotOfferLapsed(SlotMember {
            slot: slot("deploy"),
            member: ALICE,
        }),
        EventKind::SlotQueued(SlotQueued {
            slot: slot("deploy"),
            member: ALICE,
            purpose: purpose(),
        }),
        EventKind::SlotTaken(SlotTaken {
            slot: slot("deploy"),
            member: ALICE,
            purpose: purpose(),
        }),
        EventKind::PrStateReported(PrStateReported {
            pr: PrRef::parse("acme/widgets#7").unwrap(),
            mergeable: false,
        }),
    ];
    for kind in kinds {
        assert!(
            PilotStats::EVENT_NAMES.contains(&kind.name()),
            "{}",
            kind.name()
        );
    }
    let unread = [
        EventKind::SlotRenewed(SlotMember {
            slot: slot("deploy"),
            member: ALICE,
        }),
        EventKind::SlotOffered(SlotMember {
            slot: slot("deploy"),
            member: ALICE,
        }),
    ];
    for kind in unread {
        assert!(!PilotStats::EVENT_NAMES.contains(&kind.name()));
        assert_eq!(
            PilotStats::of(&range, &[event(1, 0, kind)], &[]),
            PilotStats::default(),
            "a kind the figures do not read changes nothing"
        );
    }
    assert_eq!(range.from().to_string(), "2026-03-02");
    assert!(range.holds(day("2026-03-08").start() + Duration::hours(23)));
    assert!(!range.holds(day("2026-03-09").start()));
}

#[test]
fn a_range_runs_forwards_and_covers_at_most_92_days() {
    assert_eq!(
        StatsRange::new(day("2026-03-02"), day("2026-03-01")),
        Err(StatsRangeError::Backwards)
    );
    assert!(StatsRange::new(day("2026-01-01"), day("2026-04-02")).is_ok());
    assert_eq!(
        StatsRange::new(day("2026-01-01"), day("2026-04-03")),
        Err(StatsRangeError::TooLong)
    );
    let one = StatsRange::new(day("2026-03-02"), day("2026-03-02")).unwrap();
    assert_eq!(one.ends_at() - one.starts_at(), Duration::days(1));
}

#[test]
fn a_heartbeat_reports_each_day_of_a_week_once() {
    let on = |text: &str| DayReport {
        day: day(text),
        counts: DayCounts::default(),
    };
    let week: Vec<DayReport> = (2..=8).map(|n| on(&format!("2026-03-0{n}"))).collect();
    let machine = MachineId::from_bits(7);
    assert_eq!(
        HeartbeatReports::new(machine.clone(), week.clone())
            .unwrap()
            .days()
            .len(),
        7
    );
    let mut eight = week;
    eight.push(on("2026-03-09"));
    assert_eq!(
        HeartbeatReports::new(machine.clone(), eight),
        Err(HeartbeatReportsError::TooMany)
    );
    assert_eq!(
        HeartbeatReports::new(machine, vec![on("2026-03-02"), on("2026-03-02")]),
        Err(HeartbeatReportsError::RepeatedDay)
    );
}

#[test]
fn a_machine_id_is_32_hex_digits() {
    assert_eq!(
        MachineId::from_bits(0xab).as_str(),
        "000000000000000000000000000000ab"
    );
    assert!(MachineId::parse(&"f".repeat(32)).is_ok());
    for bad in ["", "abc", &"F".repeat(32), &"g".repeat(32), &"a".repeat(33)] {
        assert!(MachineId::parse(bad).is_err(), "{bad}");
    }
}

#[test]
fn a_heartbeat_may_report_the_week_before_its_day_and_the_day_after() {
    let now = day("2026-03-09").start() + Duration::hours(12);
    let on = |text: &str| DayReport {
        day: day(text),
        counts: DayCounts::default(),
    };
    for (text, recent) in [
        ("2026-03-01", false),
        ("2026-03-02", true),
        ("2026-03-09", true),
        ("2026-03-10", true),
        ("2026-03-11", false),
    ] {
        assert_eq!(on(text).is_recent(now), recent, "{text}");
    }
}
