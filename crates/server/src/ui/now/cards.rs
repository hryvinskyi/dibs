//! The cards of Now: what waits for a person's look, each read from what the frame loaded and shown
//! only when it has something. Every card is one entry of a table that says whether it is shown and
//! how it is built, so the number of cards shown — the heading's and the rail's — is the number of
//! entries whose card would be built.

use std::collections::{BTreeMap, BTreeSet};

use time::{Duration, OffsetDateTime};

use dibs_app::named::NamedSummary;
use dibs_app::use_cases::{
    HeldItem, InboxView, NamedOverlap, OpenEscalation, PendingGrantAsk, StatusView,
};
use dibs_domain::area::QualifiedArea;
use dibs_domain::handle::Handle;
use dibs_domain::ids::ThreadId;
use dibs_domain::radar::OverlapKind;

use crate::ui::claims::classify::{finished, held_over_a_week, other_ticket, silent};
use crate::ui::claims::view::{ClaimView, OtherTicket, Scene};
use crate::ui::inbox::summary::{ConversationSummary, unread_people, waiting_grants};
use crate::ui::links::{
    AreaView, BranchView, ConversationView, LinkContext, PersonView, TicketView,
};
use crate::ui::moments::{Ago, Moment, ago, moment};
use crate::ui::shell::Loaded;

/// The most lines a card lists before it says how many more there are.
const MOST_LINES: usize = 4;

/// How long a grant request asked of the viewer waits before its card shows: a session of theirs
/// would have decided it by then.
const DECIDE_AFTER: Duration = Duration::minutes(10);

/// What the cards are read from.
pub(crate) struct Facts<'a> {
    /// The project's claims, holds, escalations and overlaps.
    pub(crate) status: &'a StatusView,
    /// The viewer's unread conversations, those waiting for their acknowledgement and the grant
    /// requests waiting for them.
    pub(crate) inbox: &'a InboxView,
    /// The member signed in.
    pub(crate) viewer: &'a Handle,
    /// The time the page is rendered at.
    pub(crate) now: OffsetDateTime,
    /// How long a claim may go unseen before it is silent.
    pub(crate) stale_after: Duration,
}

impl<'a> Facts<'a> {
    /// What the frame loaded.
    pub(crate) fn of(loaded: &'a Loaded) -> Self {
        Self {
            status: &loaded.status,
            inbox: &loaded.unread,
            viewer: &loaded.me.member.handle,
            now: loaded.now,
            stale_after: loaded.me.settings.stale_after(),
        }
    }

    /// The live and stale claims.
    fn claims(&self) -> impl Iterator<Item = &'a HeldItem> + use<'a> {
        self.status.live.iter().chain(&self.status.stale)
    }

    /// The live and stale claims that `keeps` keeps.
    fn claims_where(&self, keeps: fn(&HeldItem, &Self) -> bool) -> Vec<&'a HeldItem> {
        self.claims().filter(|held| keeps(held, self)).collect()
    }

    /// What a claim's view is built against.
    fn scene<'s>(&'s self, links: &'s LinkContext) -> Scene<'s> {
        Scene {
            status: self.status,
            viewer: self.viewer,
            now: self.now,
            stale_after: self.stale_after,
            links,
        }
    }
}

/// A card of Now.
#[derive(Debug, Clone)]
pub(crate) enum Card {
    /// Conversations from people with messages the viewer has not read, and conversations with
    /// messages waiting for the viewer's acknowledgement, read or not; each conversation once.
    Answer {
        /// How many conversations from people are unread.
        total: usize,
        /// Those, the newest first.
        lines: Lines<Talk>,
        /// How many conversations wait for the viewer's acknowledgement and are not among the
        /// unread ones.
        acks_total: usize,
        /// Those, the newest first.
        acks: Lines<Talk>,
        /// Whether any conversation of the card, unread or not, waits for an acknowledgement.
        acknowledging: bool,
    },
    /// Grant requests asked of the viewer that have waited too long for their agent.
    Decide {
        /// How many.
        total: usize,
        /// The oldest first.
        lines: Lines<Ask>,
    },
    /// Escalations no arbiter has acknowledged.
    Arbitrate {
        /// How many.
        total: usize,
        /// The newest first.
        lines: Lines<Dispute>,
        /// The arbiters they wait for, by handle.
        arbiters: Vec<PersonView>,
    },
    /// Pairs of live claims that change the same files or meet at a contract.
    Overlap {
        /// How many.
        total: usize,
        /// In the order the radar keeps them.
        lines: Lines<Pair>,
    },
    /// Claims whose ticket is finished.
    Release {
        /// How many.
        total: usize,
        /// Those holding the most areas first.
        lines: Lines<ClaimView>,
        /// Their holders.
        holders: Vec<Holder>,
        /// How many areas they hold together.
        areas: usize,
    },
    /// Claims that went silent.
    TakeOver {
        /// How many.
        total: usize,
        /// The longest silent first.
        lines: Lines<ClaimView>,
        /// How long the longest silent has been.
        longest: Ago,
        /// Their holders.
        holders: Vec<Holder>,
    },
    /// Claims held for more than a week.
    Narrow {
        /// How many.
        total: usize,
        /// The longest held first.
        lines: Lines<ClaimView>,
        /// Their holders.
        holders: Vec<Holder>,
        /// How many areas they hold together.
        areas: usize,
    },
    /// Claims that recorded a branch named after another ticket.
    Check {
        /// How many.
        total: usize,
        /// In item order.
        lines: Lines<Misnamed>,
    },
    /// The pull request integration stopped calling.
    Integration {
        /// Since when.
        since: Moment,
    },
}

/// The first lines of a card, and how many it leaves out.
#[derive(Debug, Clone)]
pub(crate) struct Lines<T> {
    /// The lines shown.
    pub(crate) shown: Vec<T>,
    /// How many more there are.
    pub(crate) more: usize,
}

/// An unread conversation from a person, or a conversation waiting for the viewer's
/// acknowledgement.
#[derive(Debug, Clone)]
pub(crate) struct Talk {
    /// The conversation.
    pub(crate) summary: ConversationSummary,
    /// When its last message was written.
    pub(crate) last: Moment,
}

/// A grant request waiting for the viewer's agent.
#[derive(Debug, Clone)]
pub(crate) struct Ask {
    /// Who asks.
    pub(crate) asker: PersonView,
    /// The area asked for.
    pub(crate) area: AreaView,
    /// The asker's ticket.
    pub(crate) ticket: TicketView,
    /// The address of the request's conversation.
    pub(crate) href: String,
    /// When it was asked.
    pub(crate) asked: Moment,
}

/// An escalation waiting for an arbiter.
#[derive(Debug, Clone)]
pub(crate) struct Dispute {
    /// Who escalated.
    pub(crate) by: PersonView,
    /// What it is about, leading to its conversation.
    pub(crate) about: ConversationView,
    /// When.
    pub(crate) at: Moment,
}

/// Two claims that touch the same code.
#[derive(Debug, Clone)]
pub(crate) struct Pair {
    /// Both tickets, in item order.
    pub(crate) tickets: Vec<TicketView>,
    /// Whether they meet at a published contract rather than in the same files.
    pub(crate) contract: bool,
    /// How many files they both change.
    pub(crate) files: usize,
}

/// A claim on a branch named after another ticket.
#[derive(Debug, Clone)]
pub(crate) struct Misnamed {
    /// The claim's ticket.
    pub(crate) ticket: TicketView,
    /// The branch.
    pub(crate) branch: BranchView,
    /// The ticket the branch is named after.
    pub(crate) other: OtherTicket,
}

/// A holder of some of a card's claims.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Holder {
    /// The holder.
    pub(crate) person: PersonView,
    /// How many of the claims they hold.
    pub(crate) claims: usize,
}

/// A card as the table knows it: whether it is shown, and how it is built when it is.
struct Need {
    shown: fn(&Facts<'_>) -> bool,
    card: fn(&Facts<'_>, &LinkContext) -> Card,
}

/// The cards, in the order the page shows them.
static NEEDS: [Need; 9] = [
    Need {
        shown: |facts| {
            let unread = answers(facts);
            !unread.is_empty() || !acknowledgements(facts, &unread).is_empty()
        },
        card: answer,
    },
    Need {
        shown: |facts| !decisions(facts).is_empty(),
        card: decide,
    },
    Need {
        shown: |facts| !facts.status.escalations.is_empty(),
        card: arbitrate,
    },
    Need {
        shown: |facts| !collisions(facts).is_empty(),
        card: overlap,
    },
    Need {
        shown: |facts| !facts.claims_where(on_finished).is_empty(),
        card: release,
    },
    Need {
        shown: |facts| !facts.claims_where(went_silent).is_empty(),
        card: take_over,
    },
    Need {
        shown: |facts| !facts.claims_where(held_long).is_empty(),
        card: narrow,
    },
    Need {
        shown: |facts| !facts.claims_where(misnamed).is_empty(),
        card: check,
    },
    Need {
        shown: |facts| facts.status.integration_silent_since.is_some(),
        card: integration,
    },
];

/// How many cards are shown.
pub(crate) fn count(facts: &Facts<'_>) -> usize {
    NEEDS.iter().filter(|need| (need.shown)(facts)).count()
}

/// The cards shown, in order.
pub(crate) fn cards(facts: &Facts<'_>, links: &LinkContext) -> Vec<Card> {
    NEEDS
        .iter()
        .filter(|need| (need.shown)(facts))
        .map(|need| (need.card)(facts, links))
        .collect()
}

/// The first [`MOST_LINES`] of `all`, each viewed by `view`.
fn lines<S, T>(all: Vec<S>, view: impl FnMut(S) -> T) -> Lines<T> {
    let more = all.len().saturating_sub(MOST_LINES);
    Lines {
        shown: all.into_iter().take(MOST_LINES).map(view).collect(),
        more,
    }
}

/// The holders of the claims, the one holding the most first, then by handle.
fn holders(claims: &[&HeldItem]) -> Vec<Holder> {
    let mut counts: BTreeMap<&Handle, usize> = BTreeMap::new();
    for held in claims {
        *counts.entry(&held.claim.holder).or_default() += 1;
    }
    let mut all: Vec<(&Handle, usize)> = counts.into_iter().collect();
    all.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    all.into_iter()
        .map(|(handle, claims)| Holder {
            person: PersonView::of(handle),
            claims,
        })
        .collect()
}

/// The areas a claim holds: none while it is silent.
fn held_areas<'a>(
    held: &'a HeldItem,
    facts: &Facts<'_>,
) -> impl Iterator<Item = &'a QualifiedArea> + use<'a> {
    let claim = &held.claim.claim;
    let quiet = silent(claim, facts.now, facts.stale_after);
    claim.areas.iter().filter(move |_| !quiet)
}

/// How many distinct areas the claims hold together.
fn areas_together(claims: &[&HeldItem], facts: &Facts<'_>) -> usize {
    claims
        .iter()
        .flat_map(|held| held_areas(held, facts))
        .collect::<BTreeSet<_>>()
        .len()
}

/// The conversations of open escalations, which are the Arbitrate card's.
fn disputed(facts: &Facts<'_>) -> BTreeSet<ThreadId> {
    facts
        .status
        .escalations
        .iter()
        .map(|escalation| escalation.thread)
        .collect()
}

/// The conversations with unread messages from people that no other card shows: an open
/// escalation's conversation is the Arbitrate card's.
fn answers<'a>(facts: &Facts<'a>) -> Vec<&'a NamedSummary> {
    let waiting = waiting_grants(facts.inbox);
    let disputed = disputed(facts);
    facts
        .inbox
        .threads
        .iter()
        .filter(|summary| {
            unread_people(summary, &waiting) && !disputed.contains(&summary.thread.record.id)
        })
        .collect()
}

/// The conversations with messages waiting for the viewer's acknowledgement, read or not, that no
/// other card shows and that are not among the unread ones [`answers`] lists: an open escalation's
/// conversation is the Arbitrate card's, and an unread one is listed once, among the unread.
fn acknowledgements<'a>(facts: &Facts<'a>, unread: &[&NamedSummary]) -> Vec<&'a NamedSummary> {
    let disputed = disputed(facts);
    let unread: BTreeSet<ThreadId> = unread
        .iter()
        .map(|summary| summary.thread.record.id)
        .collect();
    facts
        .inbox
        .awaiting_ack
        .iter()
        .filter(|summary| {
            let id = summary.thread.record.id;
            summary.awaiting_ack > 0 && !disputed.contains(&id) && !unread.contains(&id)
        })
        .collect()
}

/// The conversations, each as a line of a card, the newest first.
fn talks(summaries: Vec<&NamedSummary>, facts: &Facts<'_>, links: &LinkContext) -> Lines<Talk> {
    let mut talks: Vec<ConversationSummary> = summaries
        .into_iter()
        .map(|summary| ConversationSummary::of(summary, links))
        .collect();
    talks.sort_by(|a, b| b.last_at.cmp(&a.last_at).then_with(|| b.id.cmp(&a.id)));
    lines(talks, |summary| Talk {
        last: moment(summary.last_at, facts.now),
        summary,
    })
}

/// The card of the unread conversations from people and of the conversations waiting for the
/// viewer's acknowledgement, each the newest first.
fn answer(facts: &Facts<'_>, links: &LinkContext) -> Card {
    let unread = answers(facts);
    let acks = acknowledgements(facts, &unread);
    let acknowledging = !acks.is_empty() || unread.iter().any(|summary| summary.awaiting_ack > 0);
    Card::Answer {
        total: unread.len(),
        acks_total: acks.len(),
        lines: talks(unread, facts, links),
        acks: talks(acks, facts, links),
        acknowledging,
    }
}

/// The grant requests asked of the viewer more than [`DECIDE_AFTER`] ago, among every one waiting
/// and not only those an agent is handed.
fn decisions<'a>(facts: &Facts<'a>) -> Vec<&'a PendingGrantAsk> {
    facts
        .inbox
        .every_grant_ask
        .iter()
        .filter(|ask| facts.now - ask.asked_at > DECIDE_AFTER)
        .collect()
}

/// The card of the grant requests left waiting, the oldest first.
fn decide(facts: &Facts<'_>, links: &LinkContext) -> Card {
    let mut asks = decisions(facts);
    asks.sort_by_key(|ask| (ask.asked_at, ask.thread));
    Card::Decide {
        total: asks.len(),
        lines: lines(asks, |ask| Ask {
            asker: PersonView::of(&ask.asker),
            area: AreaView::of(&ask.area),
            ticket: TicketView::of(ask.item.id, ask.item.external_ref.as_ref(), links),
            href: format!("/ui/inbox/{}", ask.thread),
            asked: moment(ask.asked_at, facts.now),
        }),
    }
}

/// The card of the escalations waiting for an arbiter.
fn arbitrate(facts: &Facts<'_>, links: &LinkContext) -> Card {
    let open: Vec<&OpenEscalation> = facts.status.escalations.iter().collect();
    let arbiters: BTreeSet<&Handle> = open
        .iter()
        .flat_map(|escalation| &escalation.arbiters)
        .collect();
    Card::Arbitrate {
        total: open.len(),
        lines: lines(open, |escalation| Dispute {
            by: PersonView::of(&escalation.by),
            about: ConversationView::of(escalation.thread, &escalation.about, links),
            at: moment(escalation.at, facts.now),
        }),
        arbiters: arbiters.into_iter().map(PersonView::of).collect(),
    }
}

/// The overlaps between two live claims: the same files, or a published contract.
fn collisions<'a>(facts: &Facts<'a>) -> Vec<&'a NamedOverlap> {
    facts
        .status
        .overlaps
        .iter()
        .filter(|named| {
            matches!(
                named.overlap.kind(),
                OverlapKind::SameFile | OverlapKind::Contract
            )
        })
        .collect()
}

/// The card of the pairs of claims that touch the same code.
fn overlap(facts: &Facts<'_>, links: &LinkContext) -> Card {
    let pairs = collisions(facts);
    Card::Overlap {
        total: pairs.len(),
        lines: lines(pairs, |named| Pair {
            tickets: named
                .items
                .iter()
                .map(|(item, _)| TicketView::of(item.id, item.external_ref.as_ref(), links))
                .collect(),
            contract: named.overlap.kind() == OverlapKind::Contract,
            files: named.overlap.paths().len(),
        }),
    }
}

/// Whether the claim's ticket is finished.
fn on_finished(held: &HeldItem, _: &Facts<'_>) -> bool {
    finished(&held.item.item)
}

/// The card of the claims on finished tickets, those holding the most areas first.
fn release(facts: &Facts<'_>, links: &LinkContext) -> Card {
    let mut claims = facts.claims_where(on_finished);
    claims.sort_by(|a, b| {
        let areas = |held: &HeldItem| held_areas(held, facts).count();
        areas(b)
            .cmp(&areas(a))
            .then_with(|| a.item.item.id.cmp(&b.item.item.id))
    });
    let scene = facts.scene(links);
    Card::Release {
        total: claims.len(),
        holders: holders(&claims),
        areas: areas_together(&claims, facts),
        lines: lines(claims, |held| ClaimView::of(held, &scene)),
    }
}

/// Whether the claim went silent.
fn went_silent(held: &HeldItem, facts: &Facts<'_>) -> bool {
    silent(&held.claim.claim, facts.now, facts.stale_after)
}

/// The card of the silent claims, the longest silent first.
fn take_over(facts: &Facts<'_>, links: &LinkContext) -> Card {
    let mut claims = facts.claims_where(went_silent);
    claims.sort_by_key(|held| (held.claim.claim.last_seen, held.item.item.id));
    let longest = claims.first().map_or(Ago::JustNow, |held| {
        ago(facts.now - held.claim.claim.last_seen)
    });
    let scene = facts.scene(links);
    Card::TakeOver {
        total: claims.len(),
        longest,
        holders: holders(&claims),
        lines: lines(claims, |held| ClaimView::of(held, &scene)),
    }
}

/// Whether the claim has been held over a week.
fn held_long(held: &HeldItem, facts: &Facts<'_>) -> bool {
    held_over_a_week(
        &held.claim.claim,
        &held.item.item,
        facts.now,
        facts.stale_after,
    )
}

/// The card of the claims held over a week, the longest held first.
fn narrow(facts: &Facts<'_>, links: &LinkContext) -> Card {
    let mut claims = facts.claims_where(held_long);
    claims.sort_by_key(|held| (held.claim.claim.claimed_at, held.item.item.id));
    let scene = facts.scene(links);
    Card::Narrow {
        total: claims.len(),
        holders: holders(&claims),
        areas: areas_together(&claims, facts),
        lines: lines(claims, |held| ClaimView::of(held, &scene)),
    }
}

/// Whether a branch the claim recorded is named after another ticket.
fn misnamed(held: &HeldItem, _: &Facts<'_>) -> bool {
    other_ticket(&held.claim.claim, &held.item.item).is_some()
}

/// The card of the claims on a branch named after another ticket, in item order.
fn check(facts: &Facts<'_>, links: &LinkContext) -> Card {
    let mut claims = facts.claims_where(misnamed);
    claims.sort_by_key(|held| held.item.item.id);
    let views: Vec<Misnamed> = claims
        .into_iter()
        .filter_map(|held| {
            let claim = &held.claim.claim;
            let item = &held.item.item;
            let (repo, number) = other_ticket(claim, item)?;
            let branch = claim.branch(repo)?;
            Some(Misnamed {
                ticket: TicketView::of(item.id, item.external_ref.as_ref(), links),
                branch: BranchView::of(repo, branch, links),
                other: OtherTicket {
                    number,
                    link: links.numbered(repo, number),
                },
            })
        })
        .collect();
    Card::Check {
        total: views.len(),
        lines: lines(views, |view| view),
    }
}

/// The card of the silent pull request integration.
fn integration(facts: &Facts<'_>, _: &LinkContext) -> Card {
    Card::Integration {
        since: moment(
            facts.status.integration_silent_since.unwrap_or(facts.now),
            facts.now,
        ),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use askama::Template;
    use time::macros::datetime;

    use dibs_app::named::{AuthorName, ItemName, NamedClaim, NamedItem, NamedThread};
    use dibs_app::records::{ItemRecord, ThreadRecord};
    use dibs_app::use_cases::{ReadyView, UnreadConversations};
    use dibs_domain::claim::Claim;
    use dibs_domain::escalation::EscalationSubject;
    use dibs_domain::forge::ForgeRegistry;
    use dibs_domain::handover::ClaimKind;
    use dibs_domain::hot_rule::HotRuleRegistry;
    use dibs_domain::ids::{ItemId, MemberId, MessageId, SessionId, ThreadId};
    use dibs_domain::item::{ExternalRef, ItemRepository, ItemStatus, Title};
    use dibs_domain::message::{Author, Importance, Recipients, Subject};
    use dibs_domain::paths::RepoPath;
    use dibs_domain::radar::{BranchName, BranchSource, Overlap, RecordedBranch};
    use dibs_domain::ref_scheme::RefSchemeRegistry;
    use dibs_domain::repo::RepoId;
    use dibs_domain::review::{ClaimState, PrRef, Review};
    use dibs_domain::session::HostName;
    use dibs_domain::settings::{ProjectSettings, SettingsDocument};

    use crate::ui::links::places;

    use super::*;

    const NOW: OffsetDateTime = datetime!(2026-03-10 12:00:00 UTC);

    /// The cards' markup, rendered by the page's own template.
    #[derive(Template)]
    #[template(path = "screens/now.html", block = "needs")]
    struct Needs {
        cards: Vec<Card>,
    }

    fn render(cards: Vec<Card>) -> String {
        Needs { cards }.render().unwrap()
    }

    fn handle(text: &str) -> Handle {
        Handle::parse(text).unwrap()
    }

    fn area(text: &str) -> QualifiedArea {
        QualifiedArea::parse(text).unwrap()
    }

    fn reference(number: u64) -> ExternalRef {
        ExternalRef::parse(&format!("gh:acme/widgets#{number}")).unwrap()
    }

    fn empty_status() -> StatusView {
        StatusView {
            live: Vec::new(),
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
        }
    }

    fn empty_inbox() -> InboxView {
        InboxView {
            threads: Vec::new(),
            unacked: None,
            offers: Vec::new(),
            deliverable: None,
            grant_asks: Vec::new(),
            every_grant_ask: Vec::new(),
            awaiting_ack: Vec::new(),
            unread_conversations: UnreadConversations::default(),
        }
    }

    /// What alice sees; a claim goes silent after a day unseen.
    struct World {
        status: StatusView,
        inbox: InboxView,
        viewer: Handle,
    }

    impl World {
        fn new() -> Self {
            Self {
                status: empty_status(),
                inbox: empty_inbox(),
                viewer: handle("alice"),
            }
        }

        fn facts(&self) -> Facts<'_> {
            Facts {
                status: &self.status,
                inbox: &self.inbox,
                viewer: &self.viewer,
                now: NOW,
                stale_after: Duration::days(1),
            }
        }

        fn links(&self) -> LinkContext {
            let document: SettingsDocument = serde_json::from_value(serde_json::json!({
                "repos": [{"id": "api", "remote": "https://github.com/acme/widgets",
                           "areas": [{"name": "billing", "paths": ["billing/**"]},
                                     {"name": "search", "paths": ["search/**"]}]}]
            }))
            .unwrap();
            LinkContext::new(
                ProjectSettings::parse(&document, &HotRuleRegistry::standard()).unwrap(),
                Arc::new(RefSchemeRegistry::standard()),
                Arc::new(ForgeRegistry::standard()),
                places(&self.status),
            )
        }

        fn cards(&self) -> Vec<Card> {
            cards(&self.facts(), &self.links())
        }

        /// The one card shown.
        fn card(&self) -> Card {
            let mut all = self.cards();
            assert_eq!(all.len(), 1, "{all:?}");
            assert_eq!(count(&self.facts()), 1);
            all.remove(0)
        }

        fn page(&self) -> String {
            render(self.cards())
        }
    }

    /// `holder`'s claim on item `id` (`gh:acme/widgets#{id}`), claimed `held` and last seen `seen`
    /// before now, holding `areas`.
    fn held(id: u64, holder: &str, held: Duration, seen: Duration, areas: &[&str]) -> HeldItem {
        let item = ItemRecord::new(
            ItemId(id),
            Some(reference(id)),
            Title::parse(&format!("Item {id}")).unwrap(),
            ItemRepository::Listed(RepoId::parse("api").unwrap()),
            MemberId(1),
            NOW - Duration::days(60),
        );
        HeldItem {
            claim: NamedClaim {
                claim: Claim {
                    item: ItemId(id),
                    member: MemberId(1),
                    session: SessionId(1),
                    host: HostName::parse("laptop").unwrap(),
                    areas: areas.iter().map(|text| area(text)).collect(),
                    wide: None,
                    claimed_at: NOW - held,
                    renewed_at: NOW - held,
                    last_seen: NOW - seen,
                    state: ClaimState::Working,
                    branches: BTreeMap::new(),
                    kind: ClaimKind::Feature,
                    pending: BTreeMap::new(),
                },
                holder: handle(holder),
            },
            item: NamedItem {
                item,
                created_by: handle("alice"),
            },
            link: None,
        }
    }

    fn working(id: u64, holder: &str) -> HeldItem {
        held(id, holder, Duration::hours(2), Duration::minutes(1), &[])
    }

    fn closed(mut held: HeldItem) -> HeldItem {
        held.item.item.closed = true;
        held
    }

    fn titles(page: &str) -> Vec<&str> {
        page.split("<h2>")
            .skip(1)
            .filter_map(|rest| rest.split("</h2>").next())
            .collect()
    }

    /// A conversation `id` started by `author` (`None`: dibs) with `unread` unread messages, every
    /// message written by whoever started it, its last message `ago` before now.
    fn talk(id: u64, author: Option<&str>, unread: u64, ago: Duration) -> NamedSummary {
        let (record_author, named) = match author {
            Some(name) => (
                Author::Member(MemberId(2)),
                AuthorName::Member(handle(name)),
            ),
            None => (Author::System, AuthorName::Dibs),
        };
        NamedSummary {
            thread: NamedThread {
                record: ThreadRecord {
                    id: ThreadId(id),
                    subject: Subject::parse(&format!("Subject {id}")).unwrap(),
                    item: None,
                    author: record_author,
                    recipients: Recipients::Project,
                    created_at: NOW - Duration::days(1),
                },
                author: named,
                recipients: Vec::new(),
            },
            messages: 2,
            unread,
            unread_from_people: if author.is_some() { unread } else { 0 },
            unread_from_dibs: if author.is_some() { 0 } else { unread },
            from_people: if author.is_some() { 2 } else { 0 },
            from_dibs: if author.is_some() { 0 } else { 2 },
            last_from: author.map(handle),
            last_unread_from: author.filter(|_| unread > 0).map(handle),
            last_message_at: Some(NOW - ago),
            importance: Importance::Normal,
            awaiting_ack: 0,
        }
    }

    fn ask(thread: u64, asked: Duration) -> PendingGrantAsk {
        let name = |id: u64| ItemName {
            id: ItemId(id),
            external_ref: Some(reference(id)),
            title: Title::parse("Asked").unwrap(),
            link: None,
        };
        PendingGrantAsk {
            thread: ThreadId(thread),
            message: MessageId(thread),
            asker: handle("bob"),
            item: name(thread + 100),
            area: area("api:billing"),
            holder_item: name(1),
            asked_at: NOW - asked,
        }
    }

    fn escalation(thread: u64, arbiters: &[&str]) -> OpenEscalation {
        OpenEscalation {
            subject: EscalationSubject::Item(ItemId(thread)),
            about: format!("Rates of gh:acme/widgets#{thread}"),
            by: handle("bob"),
            arbiters: arbiters.iter().map(|name| handle(name)).collect(),
            thread: ThreadId(thread),
            message: MessageId(thread),
            at: NOW - Duration::minutes(30),
        }
    }

    fn collision(kind: OverlapKind, items: [u64; 2], paths: &[&str]) -> NamedOverlap {
        let overlap = Overlap::new(
            kind,
            items.iter().map(|id| ItemId(*id)).collect(),
            None,
            paths
                .iter()
                .map(|path| RepoPath::parse(path).unwrap())
                .collect(),
        )
        .unwrap();
        NamedOverlap {
            overlap,
            items: items
                .iter()
                .map(|id| {
                    (
                        ItemName {
                            id: ItemId(*id),
                            external_ref: Some(reference(*id)),
                            title: Title::parse("Pair").unwrap(),
                            link: None,
                        },
                        handle("bob"),
                    )
                })
                .collect(),
        }
    }

    #[test]
    fn nothing_shows_no_card() {
        let world = World::new();
        assert!(world.cards().is_empty());
        assert_eq!(count(&world.facts()), 0);
        assert_eq!(world.page(), "");
    }

    #[test]
    fn answer_lists_unread_conversations_from_people_newest_first() {
        let mut world = World::new();
        world.inbox.threads = vec![
            talk(1, Some("bob"), 1, Duration::hours(3)),
            talk(2, Some("alice"), 2, Duration::minutes(5)),
            talk(3, None, 4, Duration::minutes(1)),
            talk(4, Some("bob"), 0, Duration::minutes(1)),
            talk(5, Some("bob"), 1, Duration::minutes(2)),
        ];
        world.inbox.every_grant_ask = vec![ask(5, Duration::minutes(1))];
        let Card::Answer { total, lines, .. } = world.card() else {
            panic!("an answer card");
        };
        assert_eq!(
            total, 2,
            "dibs's, the read one and the waiting request are not"
        );
        let ids: Vec<ThreadId> = lines.shown.iter().map(|line| line.summary.id).collect();
        assert_eq!(ids, [ThreadId(2), ThreadId(1)]);
        assert_eq!(
            titles(&world.page()),
            ["2 conversations from people are unread"]
        );
        world.inbox.threads.truncate(1);
        assert_eq!(
            titles(&world.page()),
            ["1 conversation from a person is unread"]
        );
    }

    #[test]
    fn an_open_escalations_conversation_is_the_arbitrate_cards_and_not_the_answer_cards() {
        let mut world = World::new();
        world.inbox.threads = vec![
            talk(3, Some("bob"), 1, Duration::minutes(2)),
            talk(4, Some("bob"), 1, Duration::minutes(1)),
        ];
        world.status.escalations = vec![escalation(3, &["alice"])];
        let labels: Vec<&str> = world
            .cards()
            .iter()
            .filter_map(|card| match card {
                Card::Answer { total, .. } => Some(if *total == 1 { "answer 1" } else { "answer" }),
                Card::Arbitrate { .. } => Some("arbitrate"),
                _ => None,
            })
            .collect();
        assert_eq!(labels, ["answer 1", "arbitrate"]);
        world.inbox.threads.truncate(1);
        assert_eq!(
            world
                .cards()
                .iter()
                .filter(|card| matches!(card, Card::Answer { .. }))
                .count(),
            0,
            "the escalation's own conversation raises no Answer card"
        );
    }

    #[test]
    fn a_card_lists_four_lines_and_says_how_many_more() {
        let mut world = World::new();
        world.inbox.threads = (1..=6)
            .map(|id| {
                talk(
                    id,
                    Some("bob"),
                    1,
                    Duration::minutes(i64::try_from(id).unwrap()),
                )
            })
            .collect();
        let Card::Answer { total, lines, .. } = world.card() else {
            panic!("an answer card");
        };
        assert_eq!((total, lines.shown.len(), lines.more), (6, 4, 2));
        let page = world.page();
        assert!(
            page.contains(r#"<li class="faint">and 2 more</li>"#),
            "{page}"
        );
        world.inbox.threads.truncate(4);
        assert!(!world.page().contains("more</li>"));
    }

    #[test]
    fn decide_shows_grant_requests_waiting_more_than_ten_minutes_oldest_first() {
        let mut world = World::new();
        world.inbox.every_grant_ask = vec![ask(7, DECIDE_AFTER)];
        assert!(world.cards().is_empty(), "ten minutes exactly is not more");
        world.inbox.every_grant_ask = vec![
            ask(7, DECIDE_AFTER + Duration::seconds(1)),
            ask(8, Duration::hours(2)),
            ask(9, Duration::minutes(3)),
        ];
        let Card::Decide { total, lines } = world.card() else {
            panic!("a decide card");
        };
        assert_eq!(total, 2);
        let threads: Vec<&str> = lines.shown.iter().map(|line| line.href.as_str()).collect();
        assert_eq!(threads, ["/ui/inbox/8", "/ui/inbox/7"]);
        assert_eq!(lines.shown[0].area.full, "api:billing");
        assert_eq!(lines.shown[0].ticket.title, "gh:acme/widgets#108");
        assert_eq!(
            titles(&world.page()),
            ["2 grant requests have waited for your agent for more than ten minutes"]
        );
        world.inbox.every_grant_ask.truncate(1);
        assert_eq!(
            titles(&world.page()),
            ["1 grant request has waited for your agent for more than ten minutes"]
        );
    }

    /// Twelve grant requests, newest first, as the use case answers them: the agent's list holds
    /// the newest ten; only the two oldest were asked more than ten minutes ago.
    fn twelve_asks(world: &mut World) {
        world.inbox.every_grant_ask = (1..=12)
            .map(|n| {
                let asked = if n > 10 {
                    Duration::hours(1) + Duration::minutes(i64::try_from(n).unwrap())
                } else {
                    Duration::minutes(i64::try_from(n).unwrap() / 2)
                };
                ask(20 + n, asked)
            })
            .collect();
        world.inbox.grant_asks = world.inbox.every_grant_ask[..10].to_vec();
    }

    #[test]
    fn decide_measures_from_every_waiting_request_and_not_only_the_agents_list() {
        let mut world = World::new();
        twelve_asks(&mut world);
        let Card::Decide { total, lines } = world.card() else {
            panic!("a decide card");
        };
        assert_eq!(total, 2);
        let threads: Vec<&str> = lines.shown.iter().map(|line| line.href.as_str()).collect();
        assert_eq!(threads, ["/ui/inbox/32", "/ui/inbox/31"]);
    }

    #[test]
    fn a_request_beyond_the_agents_list_is_still_no_conversation_to_answer() {
        let mut world = World::new();
        twelve_asks(&mut world);
        world.inbox.every_grant_ask.truncate(11);
        world.inbox.threads = vec![talk(31, Some("bob"), 1, Duration::hours(1))];
        assert!(
            !world
                .cards()
                .iter()
                .any(|card| matches!(card, Card::Answer { .. })),
            "the eleventh request is a decision"
        );
    }

    /// `summary`, with `count` messages waiting for the viewer's acknowledgement.
    fn awaiting(mut summary: NamedSummary, count: u64) -> NamedSummary {
        summary.awaiting_ack = count;
        summary
    }

    #[test]
    fn answer_names_the_conversations_waiting_for_an_acknowledgement_read_or_not() {
        let mut world = World::new();
        world.inbox.awaiting_ack = vec![
            awaiting(talk(1, Some("bob"), 0, Duration::minutes(3)), 2),
            awaiting(talk(2, None, 0, Duration::minutes(1)), 1),
            awaiting(talk(3, Some("bob"), 0, Duration::minutes(2)), 1),
        ];
        world.status.escalations = vec![escalation(3, &["alice"])];
        let cards = world.cards();
        let Some(Card::Answer {
            total,
            lines,
            acks_total,
            acks,
            ..
        }) = cards
            .iter()
            .find(|card| matches!(card, Card::Answer { .. }))
        else {
            panic!("an answer card in {cards:?}");
        };
        assert_eq!((*total, lines.shown.len()), (0, 0));
        assert_eq!(*acks_total, 2, "the escalation's is the Arbitrate card's");
        let ids: Vec<ThreadId> = acks.shown.iter().map(|line| line.summary.id).collect();
        assert_eq!(ids, [ThreadId(2), ThreadId(1)]);
        let page = world.page();
        assert!(
            titles(&page).contains(&"2 conversations wait for your acknowledgement"),
            "{page}"
        );
        assert!(
            page.contains(r#"<span class="tag tag--warn" title="Messages waiting for your acknowledgement">2 to acknowledge</span>"#),
            "{page}"
        );
        world.inbox.awaiting_ack.truncate(1);
        world.inbox.threads = vec![talk(4, Some("bob"), 1, Duration::minutes(1))];
        let page = world.page();
        assert_eq!(
            page.matches(r#"<article class="need">"#).count(),
            2,
            "one Answer card besides Arbitrate: {page}"
        );
        assert!(
            titles(&page).contains(&"1 conversation from a person is unread"),
            "{page}"
        );
        assert!(
            page.contains(r#"<p class="sub">1 conversation waits for your acknowledgement:</p>"#),
            "{page}"
        );
        world.inbox.awaiting_ack.clear();
        assert!(!world.page().contains("to acknowledge"));
    }

    #[test]
    fn an_unread_conversation_waiting_for_an_acknowledgement_is_listed_once_among_the_unread() {
        let mut world = World::new();
        world.inbox.threads = vec![awaiting(talk(1, Some("bob"), 1, Duration::minutes(3)), 1)];
        world.inbox.awaiting_ack = vec![
            awaiting(talk(1, Some("bob"), 1, Duration::minutes(3)), 1),
            awaiting(talk(2, Some("carol"), 0, Duration::minutes(1)), 2),
        ];
        let Card::Answer {
            total,
            lines,
            acks_total,
            acks,
            acknowledging,
        } = world.card()
        else {
            panic!("an answer card");
        };
        assert_eq!(total, 1);
        let unread: Vec<ThreadId> = lines.shown.iter().map(|line| line.summary.id).collect();
        assert_eq!(unread, [ThreadId(1)]);
        assert_eq!(acks_total, 1, "the unread one is not counted again");
        let acked: Vec<ThreadId> = acks.shown.iter().map(|line| line.summary.id).collect();
        assert_eq!(acked, [ThreadId(2)]);
        assert!(acknowledging);
        let page = world.page();
        assert!(
            page.contains(r#"<span class="tag tag--warn" title="Messages waiting for your acknowledgement">1 to acknowledge</span>"#),
            "the unread line says it waits: {page}"
        );
        assert!(
            page.contains(r#"<p class="sub">1 conversation waits for your acknowledgement:</p>"#),
            "{page}"
        );

        world.inbox.awaiting_ack.truncate(1);
        let Card::Answer {
            total,
            acks_total,
            acknowledging,
            ..
        } = world.card()
        else {
            panic!("an answer card");
        };
        assert_eq!((total, acks_total), (1, 0));
        assert!(acknowledging);
        let page = world.page();
        assert!(!page.contains("for your acknowledgement:"), "{page}");
        assert!(
            page.contains("reading it is not enough"),
            "the rule still explains the badge: {page}"
        );
    }

    #[test]
    fn arbitrate_lists_the_escalations_and_every_arbiter_they_wait_for() {
        let mut world = World::new();
        world.status.escalations = vec![
            escalation(3, &["carol", "alice"]),
            escalation(2, &["alice"]),
        ];
        let Card::Arbitrate {
            total,
            lines,
            arbiters,
        } = world.card()
        else {
            panic!("an arbitrate card");
        };
        assert_eq!(total, 2);
        assert_eq!(lines.shown[0].about.href, "/ui/inbox/3");
        let names: Vec<&str> = arbiters
            .iter()
            .map(|person| person.handle.as_str())
            .collect();
        assert_eq!(names, ["alice", "carol"]);
        assert_eq!(
            titles(&world.page()),
            ["2 disagreements wait for an arbiter"]
        );
        world.status.escalations.truncate(1);
        assert_eq!(
            titles(&world.page()),
            ["1 disagreement waits for an arbiter"]
        );
    }

    #[test]
    fn overlap_counts_collisions_between_claims_and_not_a_moved_base() {
        let mut world = World::new();
        world.status.overlaps = vec![
            collision(
                OverlapKind::SameFile,
                [1, 2],
                &["api:src/a.rs", "api:src/b.rs"],
            ),
            collision(OverlapKind::Contract, [3, 4], &["api:src/c.rs"]),
        ];
        let Card::Overlap { total, lines } = world.card() else {
            panic!("an overlap card");
        };
        assert_eq!(total, 2);
        assert_eq!((lines.shown[0].files, lines.shown[0].contract), (2, false));
        assert!(lines.shown[1].contract);
        let labels: Vec<&str> = lines.shown[0]
            .tickets
            .iter()
            .map(|ticket| ticket.label.as_str())
            .collect();
        assert_eq!(labels, ["widgets#1", "widgets#2"]);
        let page = world.page();
        assert_eq!(titles(&page), ["2 pairs of claims touch the same code"]);
        assert!(page.contains(r#"<span class="e">2 files</span>"#), "{page}");
        assert!(
            page.contains(r#"<span class="e">a contract</span>"#),
            "{page}"
        );
        world.status.overlaps.truncate(1);
        assert_eq!(
            titles(&world.page()),
            ["1 pair of claims touches the same code"]
        );

        let mut moved = World::new();
        let merged = Overlap::new(
            OverlapKind::MainMoved,
            BTreeSet::from([ItemId(1)]),
            Some(dibs_domain::radar::MergeMark {
                pr: PrRef::parse("acme/widgets#9").unwrap(),
                at: NOW,
            }),
            BTreeSet::from([RepoPath::parse("api:src/a.rs").unwrap()]),
        )
        .unwrap();
        moved.status.overlaps = vec![NamedOverlap {
            overlap: merged,
            items: Vec::new(),
        }];
        assert!(moved.cards().is_empty());
    }

    #[test]
    fn release_lists_claims_on_finished_tickets_the_most_areas_first() {
        let mut world = World::new();
        let one = closed(held(
            1,
            "bob",
            Duration::hours(2),
            Duration::minutes(1),
            &["api:billing"],
        ));
        let two = closed(held(
            2,
            "alice",
            Duration::hours(2),
            Duration::minutes(1),
            &["api:billing", "api:search"],
        ));
        let mut done = held(
            3,
            "bob",
            Duration::hours(2),
            Duration::minutes(1),
            &["api:search"],
        );
        done.item.item.status = ItemStatus::Done;
        let quiet = closed(held(
            4,
            "carol",
            Duration::days(3),
            Duration::days(2),
            &["api:search"],
        ));
        world.status.live = vec![one, two, done, working(5, "bob")];
        world.status.stale = vec![quiet];
        let cards = world.cards();
        let Some(Card::Release {
            total,
            lines,
            holders,
            areas,
        }) = cards
            .iter()
            .find(|card| matches!(card, Card::Release { .. }))
        else {
            panic!("a release card in {cards:?}");
        };
        assert_eq!(*total, 4);
        let ids: Vec<ItemId> = lines.shown.iter().map(|line| line.id).collect();
        assert_eq!(ids, [ItemId(2), ItemId(1), ItemId(3), ItemId(4)]);
        assert_eq!(*areas, 2, "a silent claim holds none of its areas");
        let counts: Vec<(&str, usize)> = holders
            .iter()
            .map(|holder| (holder.person.handle.as_str(), holder.claims))
            .collect();
        assert_eq!(counts, [("bob", 2), ("alice", 1), ("carol", 1)]);
        let page = world.page();
        assert!(
            titles(&page).contains(&"4 claims hold tickets that are already finished"),
            "{page}"
        );
        assert!(page.contains(". They hold 2 areas.</p>"), "{page}");
        assert!(page.contains(r#"<span class="e">2 areas</span>"#), "{page}");
        world.status.live.truncate(1);
        world.status.stale.clear();
        let page = world.page();
        assert_eq!(
            titles(&page),
            ["1 claim holds a ticket that is already finished"]
        );
        assert!(page.contains(". They hold 1 area.</p>"), "{page}");
    }

    #[test]
    fn take_over_lists_silent_claims_the_longest_silent_first() {
        let mut world = World::new();
        world.status.stale = vec![
            held(1, "bob", Duration::days(5), Duration::days(2), &[]),
            held(2, "carol", Duration::days(9), Duration::days(4), &[]),
        ];
        world.status.live = vec![working(3, "bob")];
        let Card::TakeOver {
            total,
            lines,
            longest,
            holders,
        } = world.card()
        else {
            panic!("a take-over card");
        };
        assert_eq!((total, longest), (2, Ago::Days(4)));
        let ids: Vec<ItemId> = lines.shown.iter().map(|line| line.id).collect();
        assert_eq!(ids, [ItemId(2), ItemId(1)]);
        assert_eq!(holders.len(), 2);
        assert_eq!(
            titles(&world.page()),
            ["2 claims went silent, the longest 4 d ago"]
        );
        world.status.stale.truncate(1);
        assert_eq!(titles(&world.page()), ["1 claim went silent 2 d ago"]);
    }

    #[test]
    fn narrow_lists_claims_held_over_a_week_the_longest_held_first() {
        let mut world = World::new();
        let seen = Duration::minutes(5);
        world.status.live = vec![
            held(1, "bob", Duration::days(8), seen, &["api:billing"]),
            held(
                2,
                "bob",
                Duration::days(12),
                seen,
                &["api:search", "api:billing"],
            ),
            held(3, "bob", Duration::days(7), seen, &["api:search"]),
            closed(held(4, "bob", Duration::days(20), seen, &[])),
        ];
        let cards = world.cards();
        let Some(Card::Narrow {
            total,
            lines,
            holders,
            areas,
        }) = cards
            .iter()
            .find(|card| matches!(card, Card::Narrow { .. }))
        else {
            panic!("a narrow card in {cards:?}");
        };
        assert_eq!((*total, *areas), (2, 2));
        let ids: Vec<ItemId> = lines.shown.iter().map(|line| line.id).collect();
        assert_eq!(ids, [ItemId(2), ItemId(1)]);
        assert_eq!(holders[0].claims, 2);
        let page = world.page();
        assert!(
            titles(&page).contains(&"2 claims have been held for more than a week"),
            "{page}"
        );
        assert!(page.contains(r#"<span class="e">12 d</span>"#), "{page}");
        assert!(page.contains(". Together they hold 2 areas.</p>"), "{page}");
        world.status.live.remove(1);
        assert!(titles(&world.page()).contains(&"1 claim has been held for more than a week"));
    }

    #[test]
    fn check_names_the_ticket_a_claims_branch_is_named_after() {
        let mut world = World::new();
        let mut on_other = working(5, "carol");
        on_other.claim.claim.branches = BTreeMap::from([(
            RepoId::parse("api").unwrap(),
            RecordedBranch::new(
                BranchName::parse("bob/77-other").unwrap(),
                BranchSource::Checkout,
            ),
        )]);
        let mut own = working(6, "bob");
        own.claim.claim.branches = BTreeMap::from([(
            RepoId::parse("api").unwrap(),
            RecordedBranch::new(
                BranchName::parse("bob/6-own").unwrap(),
                BranchSource::Checkout,
            ),
        )]);
        world.status.live = vec![on_other, own];
        let Card::Check { total, lines } = world.card() else {
            panic!("a check card");
        };
        assert_eq!(total, 1);
        assert_eq!(lines.shown[0].branch.label, "bob/77-other");
        assert_eq!(lines.shown[0].other.number, 77);
        assert_eq!(
            lines.shown[0].other.link.href,
            "https://github.com/acme/widgets/issues/77"
        );
        assert_eq!(
            titles(&world.page()),
            ["1 claim records a branch named after another ticket"]
        );
    }

    #[test]
    fn integration_shows_while_the_integration_is_silent() {
        let mut world = World::new();
        world.status.integration_silent_since = Some(NOW - Duration::minutes(20));
        let Card::Integration { since } = world.card() else {
            panic!("an integration card");
        };
        assert_eq!(since.clock, "11:40");
        assert!(
            world.page().contains(
                r#"<h2>The pull request integration has been silent since <time datetime="2026-03-10T11:40:00Z" data-clock>11:40</time>"#
            ),
            "{}",
            world.page()
        );
    }

    #[test]
    fn the_cards_come_in_their_order_and_the_count_is_theirs() {
        let mut world = World::new();
        let mut reviewing = held(9, "bob", Duration::days(1), Duration::days(1), &[]);
        let prs = BTreeSet::from([PrRef::parse("acme/widgets#12").unwrap()]);
        reviewing.claim.claim.state = ClaimState::InReview(Review::new(prs, NOW, false).unwrap());
        let mut misnamed = held(5, "carol", Duration::days(10), Duration::minutes(1), &[]);
        misnamed.claim.claim.branches = BTreeMap::from([(
            RepoId::parse("api").unwrap(),
            RecordedBranch::new(
                BranchName::parse("bob/77-other").unwrap(),
                BranchSource::Checkout,
            ),
        )]);
        world.status.live = vec![closed(working(1, "bob")), misnamed, reviewing];
        world.status.stale = vec![held(2, "bob", Duration::days(3), Duration::days(2), &[])];
        world.status.escalations = vec![escalation(3, &["alice"])];
        world.status.overlaps = vec![collision(OverlapKind::SameFile, [1, 5], &["api:a.rs"])];
        world.status.integration_silent_since = Some(NOW - Duration::hours(1));
        world.inbox.threads = vec![talk(1, Some("bob"), 1, Duration::minutes(1))];
        world.inbox.every_grant_ask = vec![ask(8, Duration::hours(1))];
        let labels: Vec<&str> = world
            .cards()
            .iter()
            .map(|card| match card {
                Card::Answer { .. } => "answer",
                Card::Decide { .. } => "decide",
                Card::Arbitrate { .. } => "arbitrate",
                Card::Overlap { .. } => "overlap",
                Card::Release { .. } => "release",
                Card::TakeOver { .. } => "take over",
                Card::Narrow { .. } => "narrow",
                Card::Check { .. } => "check",
                Card::Integration { .. } => "integration",
            })
            .collect();
        assert_eq!(
            labels,
            [
                "answer",
                "decide",
                "arbitrate",
                "overlap",
                "release",
                "take over",
                "narrow",
                "check",
                "integration"
            ]
        );
        assert_eq!(count(&world.facts()), 9);
        let page = world.page();
        assert_eq!(page.matches(r#"<article class="need">"#).count(), 9);
        assert!(page.starts_with(r#"<div class="needs">"#), "{page}");
    }

    #[test]
    fn what_people_wrote_is_escaped_on_a_card() {
        let mut world = World::new();
        let mut summary = talk(1, Some("bob"), 1, Duration::minutes(1));
        summary.thread.record.subject = Subject::parse("Rates <script>alert(1)</script>").unwrap();
        world.inbox.threads = vec![summary];
        let page = world.page();
        assert!(!page.contains("<script>"), "{page}");
        assert!(page.contains("&#60;script&#62;"), "{page}");
    }
}
