//! The index of the project's areas: for each repository, each area it declares with its paths, the
//! areas it depends on, the live claims holding it, and the tickets of the queue that will probably
//! touch it, and how the area stands because of its holders.

use std::collections::BTreeMap;

use dibs_domain::ids::ItemId;
use dibs_domain::repo::RepoId;
use dibs_domain::settings::RepoSpec;

use crate::ui::claims::view::{ClaimView, Scene};
use crate::ui::links::{AreaView, LinkContext, LinkedText, PersonView, TicketView};
use crate::ui::moments::Ago;
use crate::ui::queue::view::QueueItemView;
use crate::ui::shell::Loaded;

/// The areas of every repository of the project.
#[derive(Debug, Clone)]
pub(crate) struct AreaIndex {
    /// The repositories, in the settings' order.
    pub(crate) repos: Vec<RepoAreas>,
}

/// The areas one repository declares.
#[derive(Debug, Clone)]
pub(crate) struct RepoAreas {
    /// The repository.
    pub(crate) repo: RepoId,
    /// Its areas, in declaration order.
    pub(crate) areas: Vec<AreaEntry>,
}

/// One declared area.
#[derive(Debug, Clone)]
pub(crate) struct AreaEntry {
    /// The area, in its full form `repo:name`.
    pub(crate) area: AreaView,
    /// The paths it declares, as declared.
    pub(crate) paths: Vec<String>,
    /// The areas it depends on.
    pub(crate) depends_on: Vec<AreaView>,
    /// The live claims holding it.
    pub(crate) holders: Vec<ClaimSummary>,
    /// The tickets of the queue whose area hints name it.
    pub(crate) wanted_by: Vec<QueueItemView>,
    /// How its holders leave it.
    pub(crate) state: AreaState,
}

/// A claim that holds an area, as the area shows it.
#[derive(Debug, Clone)]
pub(crate) struct ClaimSummary {
    /// The item's id.
    pub(crate) id: ItemId,
    /// The item's ticket.
    pub(crate) ticket: TicketView,
    /// The item's title, with the references in it linked.
    pub(crate) title: LinkedText,
    /// The holder.
    pub(crate) holder: PersonView,
    /// Whether its ticket is finished.
    pub(crate) finished: bool,
    /// Whether it has been held over a week.
    pub(crate) held_over_a_week: bool,
    /// How long it has been held.
    pub(crate) held: Ago,
}

/// How an area stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AreaState {
    /// No live claim holds it.
    Free,
    /// A live claim holds it.
    Held,
    /// Every claim holding it is on a finished ticket.
    HeldForFinished,
    /// Every claim holding it has been held over a week or is on a finished ticket, and not all
    /// are on finished tickets.
    HeldOverAWeek,
}

impl AreaState {
    /// How an area stands with these holders.
    pub(crate) fn of(holders: &[ClaimSummary]) -> Self {
        if holders.is_empty() {
            Self::Free
        } else if holders.iter().all(|claim| claim.finished) {
            Self::HeldForFinished
        } else if holders
            .iter()
            .all(|claim| claim.finished || claim.held_over_a_week)
        {
            Self::HeldOverAWeek
        } else {
            Self::Held
        }
    }

    /// Whether a claim holds it.
    pub(crate) const fn is_held(self) -> bool {
        !matches!(self, Self::Free)
    }

    /// The style modifiers of its seat, each led by a space; a free seat has none.
    pub(crate) const fn modifiers(self) -> &'static str {
        match self {
            Self::Free => "",
            Self::Held => " seat--held",
            Self::HeldForFinished => " seat--held seat--fin",
            Self::HeldOverAWeek => " seat--held seat--parked",
        }
    }
}

impl ClaimSummary {
    /// The summary of a claim.
    fn of(claim: &ClaimView) -> Self {
        Self {
            id: claim.id,
            ticket: claim.ticket.clone(),
            title: claim.title.clone(),
            holder: claim.holder.clone(),
            finished: claim.finished,
            held_over_a_week: claim.held_over_a_week,
            held: claim.claimed.ago,
        }
    }
}

impl AreaIndex {
    /// The areas the settings declare, with the live claims the status shows holding them and the
    /// tickets of `queue` that name them.
    pub(crate) fn of(loaded: &Loaded, links: &LinkContext, queue: &[QueueItemView]) -> Self {
        let scene = Scene {
            status: &loaded.status,
            viewer: &loaded.me.member.handle,
            now: loaded.now,
            stale_after: loaded.me.settings.stale_after(),
            links,
        };
        let claims: BTreeMap<ItemId, ClaimView> = loaded
            .status
            .live
            .iter()
            .map(|held| (held.item.item.id, ClaimView::of(held, &scene)))
            .collect();
        let repos = loaded
            .me
            .settings
            .repos()
            .iter()
            .map(|repo| RepoAreas::of(repo, loaded, &claims, queue))
            .collect();
        Self { repos }
    }

    /// The entry of the area written `repo:name`, when the project declares it.
    pub(crate) fn entry(&self, full: &str) -> Option<&AreaEntry> {
        self.repos
            .iter()
            .flat_map(|repo| &repo.areas)
            .find(|entry| entry.area.full == full)
    }
}

impl RepoAreas {
    /// The areas of `repo`.
    fn of(
        repo: &RepoSpec,
        loaded: &Loaded,
        claims: &BTreeMap<ItemId, ClaimView>,
        queue: &[QueueItemView],
    ) -> Self {
        let areas = repo
            .areas
            .iter()
            .map(|declared| {
                let qualified = repo.qualify(&declared.name);
                let area = AreaView::of(&qualified);
                let holders: Vec<ClaimSummary> = loaded
                    .status
                    .holds
                    .iter()
                    .filter(|hold| hold.area == qualified)
                    .filter_map(|hold| claims.get(&hold.item))
                    .map(ClaimSummary::of)
                    .collect();
                AreaEntry {
                    paths: declared
                        .paths
                        .iter()
                        .map(|glob| glob.as_str().to_owned())
                        .collect(),
                    depends_on: repo
                        .area_deps
                        .get(&declared.name)
                        .into_iter()
                        .flatten()
                        .map(AreaView::of)
                        .collect(),
                    wanted_by: queue
                        .iter()
                        .filter(|item| item.areas.iter().any(|named| named.full == area.full))
                        .cloned()
                        .collect(),
                    state: AreaState::of(&holders),
                    holders,
                    area,
                }
            })
            .collect();
        Self {
            repo: repo.id.clone(),
            areas,
        }
    }
}

impl AreaEntry {
    /// The text a list's filter matches against: the area's full name and its paths.
    pub(crate) fn text(&self) -> String {
        std::iter::once(self.area.full.as_str())
            .chain(self.paths.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

#[cfg(test)]
mod tests {
    use dibs_domain::handle::Handle;

    use super::*;
    use crate::ui::links::{Link, TicketView};

    fn holder(finished: bool, held_over_a_week: bool) -> ClaimSummary {
        ClaimSummary {
            id: ItemId(1),
            ticket: TicketView {
                label: "widgets#1".to_owned(),
                title: "gh:acme/widgets#1".to_owned(),
                main: Link {
                    href: "/ui/claims/1".to_owned(),
                    outside: false,
                },
                out: None,
                out_name: "the tracker",
            },
            title: LinkedText { pieces: Vec::new() },
            holder: PersonView::of(&Handle::parse("bob").unwrap()),
            finished,
            held_over_a_week,
            held: Ago::Days(1),
        }
    }

    #[test]
    fn an_area_nobody_holds_is_free() {
        assert_eq!(AreaState::of(&[]), AreaState::Free);
        assert!(!AreaState::Free.is_held());
    }

    #[test]
    fn an_area_held_by_a_working_claim_is_held() {
        assert_eq!(AreaState::of(&[holder(false, false)]), AreaState::Held);
        assert_eq!(
            AreaState::of(&[holder(false, false), holder(true, false)]),
            AreaState::Held
        );
        assert_eq!(
            AreaState::of(&[holder(false, true), holder(false, false)]),
            AreaState::Held
        );
    }

    #[test]
    fn an_area_every_holder_finished_with_is_held_for_a_finished_ticket() {
        assert_eq!(
            AreaState::of(&[holder(true, false)]),
            AreaState::HeldForFinished
        );
        assert_eq!(
            AreaState::of(&[holder(true, false), holder(true, false)]),
            AreaState::HeldForFinished
        );
    }

    #[test]
    fn an_area_every_holder_has_held_over_a_week_or_finished_with_is_held_over_a_week() {
        assert_eq!(
            AreaState::of(&[holder(false, true)]),
            AreaState::HeldOverAWeek
        );
        assert_eq!(
            AreaState::of(&[holder(false, true), holder(true, false)]),
            AreaState::HeldOverAWeek
        );
    }

    #[test]
    fn a_seat_is_styled_by_how_its_area_stands() {
        assert_eq!(AreaState::Free.modifiers(), "");
        assert_eq!(AreaState::Held.modifiers(), " seat--held");
        assert_eq!(
            AreaState::HeldForFinished.modifiers(),
            " seat--held seat--fin"
        );
        assert_eq!(
            AreaState::HeldOverAWeek.modifiers(),
            " seat--held seat--parked"
        );
    }
}
