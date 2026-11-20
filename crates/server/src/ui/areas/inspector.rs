//! The inspector of one area: its paths, who holds it and for which tickets, and which tickets of
//! the queue will probably touch it.

use dibs_domain::repo::RepoId;

use crate::ui::areas::index::AreaEntry;
use crate::ui::links::{DirectoryView, LinkContext};
use crate::ui::queue::listing::Narrowing;
use crate::ui::queue::narrowed_address;
use crate::ui::queue::view::QueueItemView;

/// How many of the tickets that will touch the area the inspector lists.
const WANTED_SHOWN: usize = 6;

/// The inspector of a declared area.
#[derive(Debug, Clone)]
pub struct InspectorView {
    /// The area.
    pub entry: AreaEntry,
    /// Each declared path, leading to the directory it governs.
    pub paths: Vec<DirectoryView>,
    /// The first tickets that will probably touch it.
    pub wanted: Vec<QueueItemView>,
    /// How many tickets will probably touch it.
    pub wanted_total: usize,
    /// The address of those tickets in the queue.
    pub queue: String,
}

impl InspectorView {
    /// The inspector of `entry`, the paths of the repository `repo` linked through `links`.
    pub fn of(entry: AreaEntry, repo: &RepoId, links: &LinkContext) -> Self {
        let paths = entry
            .paths
            .iter()
            .map(|path| DirectoryView {
                label: path.clone(),
                ..DirectoryView::of(repo, directory_of(path), links)
            })
            .collect();
        Self {
            paths,
            wanted: entry.wanted_by.iter().take(WANTED_SHOWN).cloned().collect(),
            wanted_total: entry.wanted_by.len(),
            queue: narrowed_address(Narrowing::touching(entry.qualified.clone())),
            entry,
        }
    }
}

/// The directory a declared path governs: the part before its first `*`, `?`, `[` or `{`, cut back
/// to its last `/`; empty, the repository's root, when nothing is left.
pub fn directory_of(path: &str) -> &str {
    let fixed = path
        .find(['*', '?', '[', '{'])
        .map_or(path, |end| &path[..end]);
    fixed
        .rsplit_once('/')
        .map_or("", |(directory, _)| directory)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_glob_governs_the_directory_before_its_first_wildcard() {
        assert_eq!(directory_of("src/billing/**"), "src/billing");
        assert_eq!(directory_of("src/search/*.rs"), "src/search");
        assert_eq!(directory_of("src/a/b/c?.rs"), "src/a/b");
        assert_eq!(directory_of("src/[ab]/x"), "src");
        assert_eq!(directory_of("src/{a,b}/x"), "src");
    }

    #[test]
    fn a_wildcard_at_the_start_or_in_a_name_cuts_back_to_the_last_directory() {
        assert_eq!(directory_of("**/*.md"), "");
        assert_eq!(directory_of("*.md"), "");
        assert_eq!(directory_of("src/bill*"), "src");
        assert_eq!(directory_of("src/billing/inv*/x"), "src/billing");
    }

    #[test]
    fn a_path_with_no_wildcard_governs_the_directory_it_is_in() {
        assert_eq!(directory_of("src/billing/a.rs"), "src/billing");
        assert_eq!(directory_of("src/billing/"), "src/billing");
        assert_eq!(directory_of("Cargo.lock"), "");
        assert_eq!(directory_of(""), "");
    }
}
