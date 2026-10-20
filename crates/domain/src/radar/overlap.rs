//! Overlaps: two live claims of different members colliding on a path or a published contract, or a
//! claim whose branch a merge into its base went past.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use time::OffsetDateTime;

use crate::area::QualifiedArea;
use crate::claim::Claim;
use crate::ids::ItemId;
use crate::message::Importance;
use crate::paths::{RelPath, RepoPath};
use crate::radar::{BranchName, Consumer, Footprint, FootprintEntry};
use crate::repo::RepoId;
use crate::review::PrRef;
use crate::settings::ProjectSettings;

/// What kind of collision an overlap is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OverlapKind {
    /// Two claims' branches change one path.
    SameFile,
    /// One claim's branch changes a published contract that the other's branch consumes.
    Contract,
    /// A merge into the base changed a path the claim's branch changes, after the branch took its
    /// base.
    MainMoved,
}

impl OverlapKind {
    /// Every kind.
    pub const ALL: [Self; 3] = [Self::SameFile, Self::Contract, Self::MainMoved];

    /// The stable name: `same_file`, `contract` or `main_moved`.
    pub const fn code(self) -> &'static str {
        match self {
            Self::SameFile => "same_file",
            Self::Contract => "contract",
            Self::MainMoved => "main_moved",
        }
    }

    /// The kind of this name.
    pub fn parse(code: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.code() == code)
    }

    /// How much its message asks for attention: a collision with another member's work is high;
    /// a moved base, which every merge brings, is normal.
    pub const fn importance(self) -> Importance {
        match self {
            Self::SameFile | Self::Contract => Importance::High,
            Self::MainMoved => Importance::Normal,
        }
    }

    /// How many items an overlap of the kind names, and whether it names the merged pull request.
    const fn shape(self) -> (usize, bool) {
        match self {
            Self::SameFile | Self::Contract => (2, false),
            Self::MainMoved => (1, true),
        }
    }
}

/// The merge a moved base comes from: its pull request and when it was merged.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MergeMark {
    /// The pull request.
    pub pr: PrRef,
    /// When it was merged.
    pub at: OffsetDateTime,
}

/// An overlap the radar found: of a kind, between the items it names, on these paths. A collision
/// between two claims names both items; a moved base names the claim's item and the merge.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Overlap {
    kind: OverlapKind,
    items: BTreeSet<ItemId>,
    merge: Option<MergeMark>,
    paths: BTreeSet<RepoPath>,
}

/// Why an overlap cannot be made.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum OverlapError {
    /// It names the wrong number of items, a pull request its kind has none of (or lacks the one
    /// it has), or no path.
    #[error("an overlap names two items, or one item and a pull request, and at least one path")]
    Shape,
}

impl Overlap {
    /// An overlap of `kind`: two items and no merge for a collision, one item and the merge for a
    /// moved base, and at least one path.
    ///
    /// # Errors
    /// [`OverlapError::Shape`] when the parts do not fit the kind.
    pub fn new(
        kind: OverlapKind,
        items: BTreeSet<ItemId>,
        merge: Option<MergeMark>,
        paths: BTreeSet<RepoPath>,
    ) -> Result<Self, OverlapError> {
        let (count, names_merge) = kind.shape();
        if items.len() != count || merge.is_some() != names_merge || paths.is_empty() {
            return Err(OverlapError::Shape);
        }
        Ok(Self {
            kind,
            items,
            merge,
            paths,
        })
    }

    /// Its kind.
    pub fn kind(&self) -> OverlapKind {
        self.kind
    }

    /// The items it names, in order.
    pub fn items(&self) -> &BTreeSet<ItemId> {
        &self.items
    }

    /// The merge of a moved base.
    pub fn merge(&self) -> Option<&MergeMark> {
        self.merge.as_ref()
    }

    /// The merged pull request of a moved base.
    pub fn pr(&self) -> Option<&PrRef> {
        self.merge.as_ref().map(|merge| &merge.pr)
    }

    /// Whether it is a moved base of the footprint's claim in the footprint's repository that
    /// the footprint has caught up with: the footprint's base is no older than the merge.
    pub fn caught_up_by(&self, footprint: &Footprint) -> bool {
        self.kind == OverlapKind::MainMoved
            && self.involves(footprint.item)
            && self.paths.iter().any(|path| path.repo == footprint.repo)
            && self
                .merge
                .as_ref()
                .is_some_and(|merge| merge.at <= footprint.base.at)
    }

    /// Whether it is on a path of the repository.
    pub fn in_repo(&self, repo: &RepoId) -> bool {
        self.paths.iter().any(|path| path.repo == *repo)
    }

    /// The paths it is on, in order.
    pub fn paths(&self) -> &BTreeSet<RepoPath> {
        &self.paths
    }

    /// Whether it names the item.
    pub fn involves(&self, item: ItemId) -> bool {
        self.items.contains(&item)
    }

    /// What makes it the same overlap as another, whatever its paths: one is announced once, and
    /// paths it gains later do not make it new.
    pub fn key(&self) -> OverlapKey {
        OverlapKey {
            kind: self.kind,
            items: self.items.clone(),
            pr: self.pr().cloned(),
        }
    }

    /// The same overlap on other paths.
    fn with_paths(&self, paths: BTreeSet<RepoPath>) -> Self {
        Self {
            paths,
            ..self.clone()
        }
    }
}

/// What makes an overlap the one it is: its kind, its items and, for a moved base, the merge.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OverlapKey {
    /// The kind.
    pub kind: OverlapKind,
    /// The items.
    pub items: BTreeSet<ItemId>,
    /// The merged pull request of a moved base.
    pub pr: Option<PrRef>,
}

impl fmt::Display for OverlapKey {
    /// `same_file 3 7`, `main_moved 3 acme/api#12`: one text per key, which stores keep it under.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.kind.code())?;
        for item in &self.items {
            write!(f, " {item}")?;
        }
        if let Some(pr) = &self.pr {
            write!(f, " {pr}")?;
        }
        Ok(())
    }
}

/// A merge into a repository's base branch, as the radar reads it.
#[derive(Debug, Clone, Copy)]
pub struct Merge<'a> {
    /// The pull request.
    pub pr: &'a PrRef,
    /// Its repository.
    pub repo: &'a RepoId,
    /// The branch it was merged into.
    pub base: &'a BranchName,
    /// When it was merged.
    pub merged_at: OffsetDateTime,
    /// The paths it changed.
    pub files: &'a BTreeSet<RelPath>,
    /// The branch it was merged from, when the integration said.
    pub head: Option<&'a BranchName>,
    /// The item of the claim that recorded it, when one did. Without `head`, that claim's
    /// footprint in the repository stands for the merged branch.
    pub author: Option<ItemId>,
}

impl Merge<'_> {
    /// Whether `footprint` is of the branch that was merged: its work is in the base now.
    pub fn merged(&self, footprint: &Footprint) -> bool {
        footprint.repo == *self.repo
            && match self.head {
                Some(head) => footprint.branch == *head,
                None => self.author == Some(footprint.item),
            }
    }
}

/// The claims and footprints overlaps are derived from, at one moment.
#[derive(Debug, Clone, Copy)]
pub struct RadarScene<'a> {
    /// Every claim of the project; only live ones overlap.
    pub claims: &'a [Claim],
    /// Every footprint of the project.
    pub footprints: &'a [Footprint],
    /// The settings: areas, contracts and area dependencies.
    pub settings: &'a ProjectSettings,
    /// Now, which tells live claims from stale ones.
    pub now: OffsetDateTime,
}

impl RadarScene<'_> {
    /// The claims, live or stale, by item.
    fn by_item(&self) -> BTreeMap<ItemId, &Claim> {
        self.claims
            .iter()
            .map(|claim| (claim.item, claim))
            .collect()
    }

    /// Whether every claim the overlap names is live. Only such an overlap is announced; one
    /// whose claim went stale stays as it is, so going stale ends nothing and coming back
    /// announces nothing again.
    pub fn all_live(&self, overlap: &Overlap) -> bool {
        let stale_after = self.settings.stale_after();
        overlap.items().iter().all(|item| {
            self.claims
                .iter()
                .any(|claim| claim.item == *item && claim.is_live(self.now, stale_after))
        })
    }

    /// The same-file overlaps of the claim on `subject`: every claim of another member whose
    /// footprint shares a path with `paths`, the subject's own footprints. `others` are footprint
    /// paths of other claims; those not among `paths` are ignored, so a store may hand over only
    /// the ones that could match. Stale claims count here, so that an overlap holds while a claim
    /// is stale; [`RadarScene::all_live`] decides what is announced.
    pub fn same_file(
        &self,
        subject: ItemId,
        paths: &BTreeSet<RepoPath>,
        others: &[FootprintEntry],
    ) -> BTreeSet<Overlap> {
        let claims = self.by_item();
        let Some(mine) = claims.get(&subject) else {
            return BTreeSet::new();
        };
        let mut shared: BTreeMap<ItemId, BTreeSet<RepoPath>> = BTreeMap::new();
        for entry in others {
            let theirs = claims
                .get(&entry.item)
                .filter(|claim| claim.item != subject && claim.member != mine.member);
            if theirs.is_some() && paths.contains(&entry.path) {
                shared
                    .entry(entry.item)
                    .or_default()
                    .insert(entry.path.clone());
            }
        }
        shared
            .into_iter()
            .filter_map(|(other, paths)| {
                Overlap::new(OverlapKind::SameFile, [subject, other].into(), None, paths).ok()
            })
            .collect()
    }

    /// Every contract overlap of the project. A claim's footprint changes a path a contract of its
    /// repository covers; a claim of another member has a footprint that touches a path of another
    /// area that consumes it — an area declared as depending on the contract path's area, or a
    /// consumer the contract names (a whole repository, whose paths in no area count too). Each
    /// pair of claims is one overlap, on the contract paths either changes that the other
    /// consumes. Stale claims count, as for [`RadarScene::same_file`].
    ///
    /// The contract paths of a footprint are grouped by the area they lie in and the contracts that
    /// cover them before any other footprint is looked at, so the work grows with the number of
    /// such groups times the number of footprints, never with paths times paths.
    pub fn contracts(&self) -> BTreeSet<Overlap> {
        let claims = self.by_item();
        let mut pairs: BTreeMap<(ItemId, ItemId), BTreeSet<RepoPath>> = BTreeMap::new();
        for producer in self.footprints {
            let Some(made_by) = claims.get(&producer.item) else {
                continue;
            };
            for (scope, paths) in self.contract_scopes(producer) {
                for consumer in self.footprints {
                    let other = claims.get(&consumer.item).filter(|claim| {
                        claim.item != producer.item && claim.member != made_by.member
                    });
                    if other.is_some() && self.consumes(consumer, &scope) {
                        let pair = ordered(producer.item, consumer.item);
                        pairs
                            .entry(pair)
                            .or_default()
                            .extend(paths.iter().map(|path| RepoPath {
                                repo: producer.repo.clone(),
                                path: (*path).clone(),
                            }));
                    }
                }
            }
        }
        pairs
            .into_iter()
            .filter_map(|((a, b), paths)| {
                Overlap::new(OverlapKind::Contract, [a, b].into(), None, paths).ok()
            })
            .collect()
    }

    /// The contract paths of a footprint, grouped by the area each lies in and who consumes the
    /// contracts that cover it. A path no contract of the current settings covers any more is left
    /// out.
    fn contract_scopes<'f>(&self, footprint: &'f Footprint) -> Vec<(Scope, Vec<&'f RelPath>)> {
        let Some(spec) = self.settings.repo(&footprint.repo) else {
            return Vec::new();
        };
        let mut groups: BTreeMap<(Option<QualifiedArea>, Vec<usize>), Vec<&RelPath>> =
            BTreeMap::new();
        for path in &footprint.digest.contracts {
            let covering: Vec<usize> = spec
                .contracts
                .iter()
                .enumerate()
                .filter(|(_, contract)| contract.covers(path))
                .map(|(index, _)| index)
                .collect();
            if covering.is_empty() {
                continue;
            }
            let area = spec.area_for(path).map(|area| spec.qualify(&area.name));
            groups.entry((area, covering)).or_default().push(path);
        }
        groups
            .into_iter()
            .map(|((area, covering), paths)| {
                let mut consumers: Vec<Consumer> = Vec::new();
                for index in covering {
                    if let Some(contract) = spec.contracts.get(index) {
                        consumers.extend(contract.consumers.iter().cloned());
                    }
                }
                (Scope { area, consumers }, paths)
            })
            .collect()
    }

    /// Whether `footprint` touches a path of another area than the scope's that consumes it.
    fn consumes(&self, footprint: &Footprint, scope: &Scope) -> bool {
        let digest = &footprint.digest;
        let by_area = digest
            .areas
            .iter()
            .filter(|area| scope.area.as_ref() != Some(*area))
            .any(|area| {
                scope
                    .area
                    .as_ref()
                    .is_some_and(|own| self.settings.depends(area, own))
                    || scope
                        .consumers
                        .iter()
                        .any(|consumer| consumer.covers_area(area))
            });
        by_area
            || (digest.unzoned
                && scope
                    .consumers
                    .iter()
                    .any(|consumer| consumer.covers_unzoned(&footprint.repo)))
    }

    /// The moved bases a merge brings: each live claim whose footprint in the merge's repository
    /// is measured against the branch the merge went into, from a base older than the merge, and
    /// holds a path the merge changed — whoever holds it, the merge's own author included, since
    /// their other branches are as far behind as anyone's. The footprint of the branch that was
    /// merged is left out: its work is in the base. `hits` are footprint paths of the project;
    /// those the merge did not change are ignored, so a store may hand over only the ones that
    /// could match.
    pub fn main_moved(&self, merge: &Merge<'_>, hits: &[FootprintEntry]) -> BTreeSet<Overlap> {
        let stale_after = self.settings.stale_after();
        let behind: BTreeSet<ItemId> = self
            .footprints
            .iter()
            .filter(|footprint| {
                footprint.repo == *merge.repo
                    && footprint.base.behind(merge.base, merge.merged_at)
                    && !merge.merged(footprint)
            })
            .filter(|footprint| {
                self.claims.iter().any(|claim| {
                    claim.item == footprint.item && claim.is_live(self.now, stale_after)
                })
            })
            .map(|footprint| footprint.item)
            .collect();
        let mut moved: BTreeMap<ItemId, BTreeSet<RepoPath>> = BTreeMap::new();
        for hit in hits {
            let changed = hit.path.repo == *merge.repo && merge.files.contains(&hit.path.path);
            if changed && behind.contains(&hit.item) {
                moved.entry(hit.item).or_default().insert(hit.path.clone());
            }
        }
        let mark = MergeMark {
            pr: merge.pr.clone(),
            at: merge.merged_at,
        };
        moved
            .into_iter()
            .filter_map(|(item, paths)| {
                Overlap::new(
                    OverlapKind::MainMoved,
                    [item].into(),
                    Some(mark.clone()),
                    paths,
                )
                .ok()
            })
            .collect()
    }
}

/// Who consumes a group of contract paths: the area they lie in, whose dependents consume it, and
/// the consumers their contracts name.
struct Scope {
    area: Option<QualifiedArea>,
    consumers: Vec<Consumer>,
}

fn ordered(a: ItemId, b: ItemId) -> (ItemId, ItemId) {
    if a <= b { (a, b) } else { (b, a) }
}

/// How the stored overlaps change for what was derived now.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OverlapChanges {
    /// Overlaps not stored before: each is announced once.
    pub found: Vec<Overlap>,
    /// Stored overlaps that are still there on other paths: stored again, not announced.
    pub moved: Vec<Overlap>,
    /// Stored overlaps the derivation covers that are gone.
    pub ended: Vec<Overlap>,
}

impl OverlapChanges {
    /// Whether nothing changes.
    pub fn is_empty(&self) -> bool {
        self.found.is_empty() && self.moved.is_empty() && self.ended.is_empty()
    }
}

/// Compares what was derived now with what is stored. `covered` says which stored overlaps the
/// derivation speaks for: one of those that was not derived again has ended; a stored overlap it
/// does not cover stays as it is. A derived overlap already stored is not new, whatever its paths
/// became, so each is announced once for as long as it lasts; one that ends and comes back later is
/// new again. A derived overlap that is not stored is found only when `announced` lets it (its
/// claims are all live); otherwise it is left for a later derivation.
pub fn reconcile(
    stored: &[Overlap],
    derived: &BTreeSet<Overlap>,
    covered: impl Fn(&Overlap) -> bool,
    announced: impl Fn(&Overlap) -> bool,
) -> OverlapChanges {
    let known: BTreeMap<OverlapKey, &Overlap> = stored
        .iter()
        .map(|overlap| (overlap.key(), overlap))
        .collect();
    let derived_keys: BTreeSet<OverlapKey> = derived.iter().map(Overlap::key).collect();
    let mut changes = OverlapChanges::default();
    for overlap in derived {
        match known.get(&overlap.key()) {
            None if announced(overlap) => changes.found.push(overlap.clone()),
            None => {}
            Some(before) if before.paths != overlap.paths => {
                changes.moved.push(before.with_paths(overlap.paths.clone()));
            }
            Some(_) => {}
        }
    }
    changes.ended = stored
        .iter()
        .filter(|overlap| covered(overlap) && !derived_keys.contains(&overlap.key()))
        .cloned()
        .collect();
    changes
}
