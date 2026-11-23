//! The stored footprints of one claim, read with their paths, for the rules that judge the claim's
//! areas by what its branch changes.

use std::collections::BTreeSet;

use dibs_domain::claim::FootprintPaths;
use dibs_domain::ids::ItemId;
use dibs_domain::paths::RelPath;
use dibs_domain::radar::Footprint;

use crate::error::AppError;
use crate::work::Work;

/// The footprints of the claim on one item, in every repository one is stored for, each with its
/// paths.
#[derive(Debug, Default)]
pub(crate) struct ClaimFootprints {
    stored: Vec<(Footprint, BTreeSet<RelPath>)>,
}

impl ClaimFootprints {
    /// Reads the footprints of the claim on `item`, finding which repositories have one in
    /// `listed`, the project's footprints as [`crate::ports::RadarRows::footprints`] lists them
    /// without their paths, so that a caller judging several claims lists them once.
    pub(crate) async fn read(
        work: &mut Work,
        item: ItemId,
        listed: &[Footprint],
    ) -> Result<Self, AppError> {
        let project = work.project_id();
        let mut stored = Vec::new();
        for footprint in listed.iter().filter(|footprint| footprint.item == item) {
            if let Some(found) = work.footprint(project, item, &footprint.repo).await? {
                stored.push(found);
            }
        }
        Ok(Self { stored })
    }

    /// The footprints as the domain's rules read them.
    pub(crate) fn paths(&self) -> Vec<FootprintPaths<'_>> {
        self.stored
            .iter()
            .map(|(footprint, paths)| FootprintPaths {
                repo: &footprint.repo,
                paths,
                truncated: footprint.truncated,
            })
            .collect()
    }
}
