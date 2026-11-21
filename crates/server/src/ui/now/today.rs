//! What happened in the viewer's day: the claims taken and the items finished since it started,
//! newest first, and the day's date as the viewer reads it.

use time::{OffsetDateTime, UtcOffset};

use dibs_app::use_cases::CompletedItem;
use dibs_domain::item::ItemRepository;

use crate::ui::claims::view::ClaimView;
use crate::ui::links::{LinkContext, LinkedText, PersonView, TicketView};
use crate::ui::moments::{Moment, moment};

/// The most a day's list shows.
const MOST: usize = 16;

/// What was done to a ticket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Done {
    /// Someone claimed it.
    Claimed,
    /// Someone finished it.
    Finished,
}

/// One thing that happened.
#[derive(Debug, Clone)]
pub(crate) struct Happening {
    /// When.
    pub(crate) at: Moment,
    /// Who.
    pub(crate) person: PersonView,
    /// What.
    pub(crate) done: Done,
    /// To which ticket.
    pub(crate) ticket: TicketView,
    /// The ticket's title, with the references in it linked.
    pub(crate) title: LinkedText,
}

/// The claims among `claims` taken at or after `since`, and the items of `completed` finished at or
/// after it, newest first, at most [`MOST`] of them.
pub(crate) fn happenings(
    claims: &[ClaimView],
    completed: &[CompletedItem],
    since: OffsetDateTime,
    now: OffsetDateTime,
    links: &LinkContext,
) -> Vec<Happening> {
    let claimed = claims
        .iter()
        .filter(|claim| claim.claimed_at >= since)
        .map(|claim| {
            (
                claim.claimed_at,
                Happening {
                    at: claim.claimed.clone(),
                    person: claim.holder.clone(),
                    done: Done::Claimed,
                    ticket: claim.ticket.clone(),
                    title: claim.title.clone(),
                },
            )
        });
    let finished = completed
        .iter()
        .filter(|done| done.at >= since)
        .map(|done| {
            let item = &done.item.item;
            let home = match &item.repository {
                ItemRepository::Listed(repo) => Some(repo),
                ItemRepository::Unlisted(_) | ItemRepository::Unnamed => None,
            };
            (
                done.at,
                Happening {
                    at: moment(done.at, now),
                    person: PersonView::of(&done.member),
                    done: Done::Finished,
                    ticket: TicketView::of(item.id, item.external_ref.as_ref(), links),
                    title: LinkedText::of(item.title.as_str(), home, links),
                },
            )
        });
    let mut all: Vec<(OffsetDateTime, Happening)> = claimed.chain(finished).collect();
    all.sort_by_key(|(at, _)| std::cmp::Reverse(*at));
    all.into_iter()
        .take(MOST)
        .map(|(_, happening)| happening)
        .collect()
}

/// The date `now` falls on at the viewer's offset, as `YYYY-MM-DD`.
pub(crate) fn date(now: OffsetDateTime, offset: UtcOffset) -> String {
    let day = now.to_offset(offset).date();
    format!(
        "{:04}-{:02}-{:02}",
        day.year(),
        u8::from(day.month()),
        day.day()
    )
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use time::Duration;
    use time::macros::{datetime, offset};

    use dibs_app::named::{NamedClaim, NamedItem};
    use dibs_app::records::ItemRecord;
    use dibs_app::use_cases::{HeldItem, ReadyView, StatusView};
    use dibs_domain::claim::Claim;
    use dibs_domain::forge::ForgeRegistry;
    use dibs_domain::handle::Handle;
    use dibs_domain::handover::ClaimKind;
    use dibs_domain::hot_rule::HotRuleRegistry;
    use dibs_domain::ids::{ItemId, MemberId, SessionId};
    use dibs_domain::item::{ExternalRef, Title};
    use dibs_domain::ref_scheme::RefSchemeRegistry;
    use dibs_domain::review::ClaimState;
    use dibs_domain::session::HostName;
    use dibs_domain::settings::{ProjectSettings, SettingsDocument};

    use crate::ui::claims::view::Scene;

    use super::*;

    const NOW: OffsetDateTime = datetime!(2026-03-10 15:00:00 UTC);
    const SINCE: OffsetDateTime = datetime!(2026-03-10 00:00:00 UTC);

    fn handle(text: &str) -> Handle {
        Handle::parse(text).unwrap()
    }

    fn record(id: u64) -> ItemRecord {
        ItemRecord::new(
            ItemId(id),
            Some(ExternalRef::parse(&format!("gh:acme/widgets#{id}")).unwrap()),
            Title::parse(&format!("Item {id}")).unwrap(),
            ItemRepository::Unnamed,
            MemberId(1),
            NOW - Duration::days(30),
        )
    }

    /// `holder`'s claim on item `id`, taken at `at`.
    fn held(id: u64, holder: &str, at: OffsetDateTime) -> HeldItem {
        HeldItem {
            claim: NamedClaim {
                claim: Claim {
                    item: ItemId(id),
                    member: MemberId(1),
                    session: SessionId(1),
                    host: HostName::parse("laptop").unwrap(),
                    areas: Default::default(),
                    wide: None,
                    claimed_at: at,
                    renewed_at: at,
                    last_seen: NOW,
                    state: ClaimState::Working,
                    branches: BTreeMap::new(),
                    kind: ClaimKind::Feature,
                    pending: BTreeMap::new(),
                },
                holder: handle(holder),
            },
            item: NamedItem {
                item: record(id),
                created_by: handle("alice"),
            },
            link: None,
        }
    }

    fn completed(id: u64, member: &str, at: OffsetDateTime) -> CompletedItem {
        CompletedItem {
            item: NamedItem {
                item: record(id),
                created_by: handle("alice"),
            },
            member: handle(member),
            at,
            link: None,
        }
    }

    fn links() -> LinkContext {
        let document: SettingsDocument =
            serde_json::from_value(serde_json::json!({"repos": []})).unwrap();
        LinkContext::new(
            ProjectSettings::parse(&document, &HotRuleRegistry::standard()).unwrap(),
            Arc::new(RefSchemeRegistry::standard()),
            Arc::new(ForgeRegistry::standard()),
            Vec::new(),
        )
    }

    fn views(held: &[HeldItem], links: &LinkContext) -> Vec<ClaimView> {
        let status = StatusView {
            live: held.to_vec(),
            stale: Vec::new(),
            holds: Vec::new(),
            wide: Vec::new(),
            ready: ReadyView {
                ready: Vec::new(),
                excluded: Vec::new(),
            },
            slots: Vec::new(),
            worktrees: Vec::new(),
            escalations: Vec::new(),
            integration_silent_since: None,
            overlaps: Vec::new(),
        };
        let viewer = handle("alice");
        let scene = Scene {
            status: &status,
            viewer: &viewer,
            now: NOW,
            stale_after: Duration::days(1),
            links,
        };
        held.iter().map(|one| ClaimView::of(one, &scene)).collect()
    }

    #[test]
    fn the_day_lists_claims_taken_and_items_finished_since_it_began_newest_first() {
        let links = links();
        let claims = views(
            &[
                held(1, "bob", SINCE + Duration::hours(9)),
                held(2, "carol", SINCE - Duration::seconds(1)),
                held(3, "alice", SINCE),
            ],
            &links,
        );
        let finished = [
            completed(4, "bob", SINCE + Duration::hours(10)),
            completed(5, "bob", SINCE - Duration::hours(2)),
        ];
        let day = happenings(&claims, &finished, SINCE, NOW, &links);
        let shown: Vec<(&str, Done, &str)> = day
            .iter()
            .map(|happening| {
                (
                    happening.person.handle.as_str(),
                    happening.done,
                    happening.ticket.title.as_str(),
                )
            })
            .collect();
        assert_eq!(
            shown,
            [
                ("bob", Done::Finished, "gh:acme/widgets#4"),
                ("bob", Done::Claimed, "gh:acme/widgets#1"),
                ("alice", Done::Claimed, "gh:acme/widgets#3"),
            ]
        );
        assert_eq!(day[0].at.clock, "10:00");
        assert_eq!(day[0].title.pieces[0].text, "Item 4");
    }

    #[test]
    fn the_day_lists_at_most_sixteen() {
        let links = links();
        let finished: Vec<CompletedItem> = (1..=20)
            .map(|id| {
                completed(
                    id,
                    "bob",
                    SINCE + Duration::minutes(i64::try_from(id).unwrap()),
                )
            })
            .collect();
        let day = happenings(&[], &finished, SINCE, NOW, &links);
        assert_eq!(day.len(), MOST);
        assert_eq!(day[0].ticket.title, "gh:acme/widgets#20");
    }

    #[test]
    fn the_date_is_the_viewers() {
        let late = datetime!(2026-03-10 22:30:00 UTC);
        assert_eq!(date(late, UtcOffset::UTC), "2026-03-10");
        assert_eq!(date(late, offset!(+3)), "2026-03-11");
        assert_eq!(
            date(datetime!(2026-03-10 02:00:00 UTC), offset!(-5)),
            "2026-03-09"
        );
    }
}
