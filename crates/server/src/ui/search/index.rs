//! What the search finds: one entry for each thing of the project a person may look for — a claim,
//! a pull request, a ticket of the queue, a branch, an area, a member, a conversation, a slot — with
//! the words it is shown with, the text a query is matched against, and where it leads, built from
//! what the use cases answered for the request.

use std::collections::{BTreeMap, BTreeSet};

use askama::Template;
use time::OffsetDateTime;

use dibs_app::named::NamedSummary;
use dibs_app::records::MemberRecord;
use dibs_app::use_cases::StatusView;
use dibs_domain::forge::ForgeRegistry;
use dibs_domain::handle::Handle;
use dibs_domain::ids::ItemId;
use dibs_domain::radar::{BranchName, OverlapKind};
use dibs_domain::repo::RepoId;
use dibs_domain::review::PrRef;
use dibs_domain::settings::ProjectSettings;

use crate::ui::claims::view::{Activity, ClaimView, Scene};
use crate::ui::inbox::summary::ConversationSummary;
use crate::ui::links::{
    AreaView, BranchView, LinkContext, LinkedText, PersonView, PullRequestView, TicketView,
    query_value,
};
use crate::ui::matching::number;
use crate::ui::moments::{Ago, moment};
use crate::ui::queue::view::queue_items;
use crate::ui::slots::SlotRow;

/// What kind of thing an entry is; the order of the kinds is the order the search lists them in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Kind {
    /// A live or stale claim.
    Claims,
    /// A pull request of a claim in review, or a merged one that moved the base under claims.
    PullRequests,
    /// A ticket waiting in the queue.
    QueueTickets,
    /// A branch claims recorded.
    Branches,
    /// A declared area.
    Areas,
    /// A member.
    People,
    /// A conversation.
    Conversations,
    /// A declared slot.
    Slots,
}

impl Kind {
    /// Every kind, in the order the search lists them in.
    pub(crate) const ALL: [Self; 8] = [
        Self::Claims,
        Self::PullRequests,
        Self::QueueTickets,
        Self::Branches,
        Self::Areas,
        Self::People,
        Self::Conversations,
        Self::Slots,
    ];

    /// The heading the kind's hits are listed under.
    pub(crate) const fn heading(self) -> &'static str {
        match self {
            Self::Claims => "Claims",
            Self::PullRequests => "Pull requests",
            Self::QueueTickets => "Tickets in the queue",
            Self::Branches => "Branches",
            Self::Areas => "Areas",
            Self::People => "People",
            Self::Conversations => "Conversations",
            Self::Slots => "Slots",
        }
    }

    /// The name the kind's heading is identified by in the markup.
    pub(crate) const fn code(self) -> &'static str {
        match self {
            Self::Claims => "claims",
            Self::PullRequests => "pull-requests",
            Self::QueueTickets => "queue",
            Self::Branches => "branches",
            Self::Areas => "areas",
            Self::People => "people",
            Self::Conversations => "conversations",
            Self::Slots => "slots",
        }
    }
}

/// One thing the search can find.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Entry {
    /// What kind of thing it is.
    pub(crate) kind: Kind,
    /// How a hit names it.
    pub(crate) title: String,
    /// The line under its name.
    pub(crate) sub: String,
    /// What a query is matched against.
    pub(crate) text: String,
    /// The number of its ticket or pull request, which a query of one number ranks first.
    pub(crate) number: Option<u64>,
    /// Its page in this service.
    pub(crate) here: String,
    /// Its page outside this service, and what that page's owner is called.
    pub(crate) out: Option<(String, &'static str)>,
}

/// What the entries are built from: the answers of the use cases for one request.
pub(crate) struct Sources<'a> {
    /// The project's claims, holds, queue, slots and overlaps.
    pub(crate) status: &'a StatusView,
    /// The project's settings.
    pub(crate) settings: &'a ProjectSettings,
    /// The member signed in.
    pub(crate) viewer: &'a Handle,
    /// The members of the project.
    pub(crate) members: &'a [MemberRecord],
    /// The conversations the viewer can read.
    pub(crate) threads: &'a [NamedSummary],
    /// The time the answer is rendered at.
    pub(crate) now: OffsetDateTime,
    /// How the references link.
    pub(crate) links: &'a LinkContext,
    /// The forges the deployment knows.
    pub(crate) forges: &'a ForgeRegistry,
}

/// The sources with the views of the claims, which several kinds read.
struct Corpus<'a> {
    sources: &'a Sources<'a>,
    claims: Vec<ClaimView>,
}

/// Builds the entries of one or more kinds, each kind's in the order it lists them.
type Builder = fn(&Corpus<'_>) -> Vec<Entry>;

/// The builders of every kind.
const BUILDERS: [Builder; 9] = [
    claims,
    pull_requests_in_review,
    merged_pull_requests,
    queue_tickets,
    branches,
    areas,
    people,
    conversations,
    slots,
];

/// Every entry of the sources.
pub(crate) fn entries(sources: &Sources<'_>) -> Vec<Entry> {
    let scene = Scene {
        status: sources.status,
        viewer: sources.viewer,
        now: sources.now,
        stale_after: sources.settings.stale_after(),
        links: sources.links,
    };
    let corpus = Corpus {
        sources,
        claims: sources
            .status
            .live
            .iter()
            .chain(&sources.status.stale)
            .map(|held| ClaimView::of(held, &scene))
            .collect(),
    };
    BUILDERS.iter().flat_map(|build| build(&corpus)).collect()
}

/// How long ago something was, in the words the pages use for it.
#[derive(Template)]
#[template(
    source = r#"{% import "moments.html" as moments %}{% call moments::ago(ago) %}{% endcall %}"#,
    ext = "txt"
)]
struct AgoText {
    ago: Ago,
}

/// `ago` in the words the pages use for it; empty in the unlikely case the words cannot be made.
fn ago_text(ago: Ago) -> String {
    AgoText { ago }.render().unwrap_or_default()
}

/// `1 {one}`, or `{n} {one}s`.
fn counted(n: usize, one: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {one}s")
    }
}

/// The text of a linked text.
fn plain(text: &LinkedText) -> String {
    text.pieces
        .iter()
        .map(|piece| piece.text.as_str())
        .collect()
}

/// The number a ticket's full name ends in, after its last `#`.
fn ticket_number(ticket: &TicketView) -> Option<u64> {
    ticket
        .title
        .rsplit_once('#')
        .and_then(|(_, tail)| number(tail))
}

/// The tracker's page of a ticket whose name leads into this service.
fn tracker(ticket: &TicketView) -> Option<(String, &'static str)> {
    ticket
        .out
        .as_ref()
        .map(|link| (link.href.clone(), ticket.out_name))
}

/// The forge's page of a pull request whose name leads into this service.
fn forge_page(pr: &PullRequestView) -> Option<(String, &'static str)> {
    pr.out
        .as_ref()
        .map(|beside| (beside.link.href.clone(), beside.name))
}

/// The words of what a claim's holder is doing.
const fn doing(activity: Activity) -> &'static str {
    match activity {
        Activity::Working => "working",
        Activity::Review => "in review",
        Activity::Silent => "silent",
    }
}

/// One entry per live or stale claim.
fn claims(corpus: &Corpus<'_>) -> Vec<Entry> {
    corpus
        .claims
        .iter()
        .map(|claim| Entry {
            kind: Kind::Claims,
            title: format!("{}  {}", claim.ticket.title, plain(&claim.title)),
            sub: format!(
                "{} · {} · held {}",
                claim.holder.handle,
                doing(claim.activity),
                ago_text(claim.claimed.ago)
            ),
            text: claim.text(),
            number: ticket_number(&claim.ticket),
            here: format!("/ui/claims/{}", claim.id),
            out: tracker(&claim.ticket),
        })
        .collect()
}

/// One entry per pull request of a claim in review.
fn pull_requests_in_review(corpus: &Corpus<'_>) -> Vec<Entry> {
    corpus
        .claims
        .iter()
        .flat_map(|claim| {
            claim.prs.iter().map(move |pr| Entry {
                kind: Kind::PullRequests,
                title: format!("PR {}", pr.title),
                sub: format!(
                    "in review for {} · {}",
                    claim.ticket.title, claim.holder.handle
                ),
                text: format!(
                    "pr pull {} {} {}",
                    pr.title,
                    claim.ticket.title,
                    plain(&claim.title)
                ),
                number: pr.title.rsplit_once('#').and_then(|(_, tail)| number(tail)),
                here: format!("/ui/claims/{}", claim.id),
                out: forge_page(pr),
            })
        })
        .collect()
}

/// One entry per merged pull request that moved the base under live claims, with how many.
fn merged_pull_requests(corpus: &Corpus<'_>) -> Vec<Entry> {
    let sources = corpus.sources;
    let mut merged: BTreeMap<&PrRef, (BTreeSet<ItemId>, Option<PullRequestView>)> = BTreeMap::new();
    for named in &sources.status.overlaps {
        let overlap = &named.overlap;
        let Some(pr) = overlap.pr() else { continue };
        if overlap.kind() != OverlapKind::MainMoved {
            continue;
        }
        let item_repo = named
            .items
            .iter()
            .find_map(|(item, _)| item.external_ref.as_ref())
            .and_then(|external| sources.links.repository_of(external));
        let here = format!("/ui/claims?q={}", query_value(&pr.to_string()));
        let entry = merged.entry(pr).or_insert((BTreeSet::new(), None));
        entry.0.extend(overlap.items().iter().copied());
        if entry.1.is_none() {
            entry.1 = Some(PullRequestView::of(
                pr,
                Some(here),
                item_repo.as_ref(),
                sources.links,
            ));
        }
    }
    merged
        .into_iter()
        .filter_map(|(pr, (claims, view))| {
            let view = view?;
            Some(Entry {
                kind: Kind::PullRequests,
                title: format!("PR {}", view.title),
                sub: format!(
                    "merged · moved the base under {}",
                    counted(claims.len(), "claim")
                ),
                text: format!("pr pull merged {}", view.title),
                number: Some(pr.number()),
                out: forge_page(&view),
                here: view.main.href,
            })
        })
        .collect()
}

/// One entry per ticket of the queue: the ready ones, then the others it lists.
fn queue_tickets(corpus: &Corpus<'_>) -> Vec<Entry> {
    let sources = corpus.sources;
    queue_items(sources.status, sources.links)
        .iter()
        .map(|item| {
            let title = plain(&item.title);
            Entry {
                kind: Kind::QueueTickets,
                title: format!("{}  {title}", item.ticket.title),
                sub: format!(
                    "{} · P{}",
                    if item.is_ready() {
                        "ready"
                    } else {
                        "not ready"
                    },
                    item.priority
                ),
                text: std::iter::once(item.ticket.title.as_str())
                    .chain(std::iter::once(title.as_str()))
                    .chain(item.areas.iter().map(|area| area.full.as_str()))
                    .collect::<Vec<_>>()
                    .join(" "),
                number: ticket_number(&item.ticket),
                here: format!("/ui/queue/{}", item.id),
                out: tracker(&item.ticket),
            }
        })
        .collect()
}

/// One entry per branch a live or stale claim recorded in a repository, with the tickets of the
/// claims that recorded it.
fn branches(corpus: &Corpus<'_>) -> Vec<Entry> {
    let sources = corpus.sources;
    let mut seen: Vec<((&RepoId, &BranchName), Vec<&str>)> = Vec::new();
    for (held, claim) in sources
        .status
        .live
        .iter()
        .chain(&sources.status.stale)
        .zip(&corpus.claims)
    {
        for recorded in &held.claim.claim.branches {
            match seen.iter_mut().find(|(key, _)| *key == recorded) {
                Some((_, tickets)) => tickets.push(&claim.ticket.title),
                None => seen.push((recorded, vec![&claim.ticket.title])),
            }
        }
    }
    seen.into_iter()
        .map(|((repo, branch), tickets)| {
            let view = BranchView::of(repo, branch, sources.links);
            let forge = sources
                .settings
                .repo(repo)
                .and_then(|spec| spec.remote.as_ref())
                .and_then(|remote| sources.forges.of(remote))
                .map(|forge| forge.name());
            Entry {
                kind: Kind::Branches,
                sub: format!("recorded on {}", tickets.join(", ")),
                text: format!("branch {repo} {}", view.label),
                number: None,
                here: format!("/ui/claims?q={}", query_value(&view.label)),
                out: forge
                    .filter(|_| view.main.outside)
                    .map(|name| (view.main.href.clone(), name)),
                title: view.label,
            }
        })
        .collect()
}

/// One entry per declared area, with who holds it for which tickets.
fn areas(corpus: &Corpus<'_>) -> Vec<Entry> {
    let sources = corpus.sources;
    sources
        .settings
        .repos()
        .iter()
        .flat_map(|repo| {
            repo.areas.iter().map(move |declared| {
                let qualified = repo.qualify(&declared.name);
                let view = AreaView::of(&qualified);
                let holds: Vec<_> = sources
                    .status
                    .holds
                    .iter()
                    .filter(|hold| hold.area == qualified)
                    .collect();
                let sub = match holds.first() {
                    Some(first) => format!(
                        "held by {} for {}",
                        first.holder,
                        holds
                            .iter()
                            .map(|hold| TicketView::of_item(hold.item, sources.links).title)
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                    None => "free".to_owned(),
                };
                Entry {
                    kind: Kind::Areas,
                    sub,
                    text: std::iter::once(format!("area {}", view.full))
                        .chain(declared.paths.iter().map(|glob| glob.as_str().to_owned()))
                        .collect::<Vec<_>>()
                        .join(" "),
                    number: None,
                    here: view.href,
                    out: None,
                    title: view.full,
                }
            })
        })
        .collect()
}

/// One entry per member, with how many live and stale claims they hold.
fn people(corpus: &Corpus<'_>) -> Vec<Entry> {
    corpus
        .sources
        .members
        .iter()
        .map(|member| {
            let person = PersonView::of(&member.handle);
            let held = corpus
                .claims
                .iter()
                .filter(|claim| claim.holder.handle == person.handle)
                .count();
            Entry {
                kind: Kind::People,
                sub: counted(held, "claim"),
                text: format!("person {}", person.handle),
                number: None,
                here: person.href,
                out: None,
                title: person.handle,
            }
        })
        .collect()
}

/// One entry per conversation the viewer can read, newest activity first.
fn conversations(corpus: &Corpus<'_>) -> Vec<Entry> {
    let sources = corpus.sources;
    sources
        .threads
        .iter()
        .map(|summary| {
            let conversation = ConversationSummary::of(summary, sources.links);
            let author = conversation
                .author
                .as_ref()
                .map_or(dibs_app::named::AuthorName::DIBS, |person| {
                    person.handle.as_str()
                });
            let to = conversation.recipients.as_ref().map_or_else(
                || "everyone".to_owned(),
                |people| {
                    people
                        .iter()
                        .map(|person| person.handle.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                },
            );
            Entry {
                kind: Kind::Conversations,
                title: plain(&conversation.subject.subject),
                sub: format!(
                    "{author} to {to} · {}",
                    moment(conversation.last_at, sources.now).clock
                ),
                text: conversation.text(),
                number: None,
                here: conversation.subject.href.clone(),
                out: None,
            }
        })
        .collect()
}

/// One entry per declared slot, with who holds it.
fn slots(corpus: &Corpus<'_>) -> Vec<Entry> {
    let sources = corpus.sources;
    sources
        .status
        .slots
        .iter()
        .map(|line| {
            let row = SlotRow::of(line);
            Entry {
                kind: Kind::Slots,
                sub: row
                    .holder
                    .map_or_else(|| "free".to_owned(), |holder| format!("held by {holder}")),
                text: format!("slot {} {}", row.name, row.note.unwrap_or_default()),
                number: None,
                here: format!("/ui/?slot={}", query_value(&row.name)),
                out: None,
                title: row.name,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_kinds_are_listed_in_their_fixed_order_each_with_its_own_name() {
        let mut sorted = Kind::ALL;
        sorted.sort();
        assert_eq!(sorted, Kind::ALL);
        let headings: Vec<&str> = Kind::ALL.iter().map(|kind| kind.heading()).collect();
        assert_eq!(
            headings,
            [
                "Claims",
                "Pull requests",
                "Tickets in the queue",
                "Branches",
                "Areas",
                "People",
                "Conversations",
                "Slots"
            ]
        );
        let codes: std::collections::BTreeSet<&str> =
            Kind::ALL.iter().map(|kind| kind.code()).collect();
        assert_eq!(codes.len(), Kind::ALL.len());
    }

    #[test]
    fn a_count_reads_one_or_many() {
        assert_eq!(counted(0, "claim"), "0 claims");
        assert_eq!(counted(1, "claim"), "1 claim");
        assert_eq!(counted(3, "claim"), "3 claims");
    }

    #[test]
    fn an_age_reads_as_the_pages_write_it() {
        assert_eq!(ago_text(Ago::JustNow), "just now");
        assert_eq!(ago_text(Ago::Minutes(4)), "4 min");
        assert_eq!(ago_text(Ago::Hours(2)), "2 h");
        assert_eq!(ago_text(Ago::Days(9)), "9 d");
    }
}
