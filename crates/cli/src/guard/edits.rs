//! The edits this machine's hooks let through without asking the server, in the project's state,
//! until a heartbeat carries them: `edits.spool/` holds one empty file per area an edit was let
//! through in, named `<repo>+<area>` (`api+billing`), and one named `outside` once an edit was
//! made in a path no area governs. A hook makes the file it needs without a lock and without
//! syncing it to the disk, and leaves it alone when it is already there, so recording the same area
//! again costs one failed create and the spool never holds more files than the project has areas,
//! and one, besides those in flight.
//!
//! The sync reads the spool under the project's sync lock. Each file it reads is first moved into
//! `edits.spool/sending/`, so an edit recorded while the heartbeat is in flight makes a fresh file
//! in the spool instead of finding the one about to be deleted. It sends what it read with the
//! heartbeat (at most [`ReportedEdits::MAX_AREAS`] areas, the others staying in `sending/` for the
//! next one) and, once the server accepted that heartbeat, deletes the files it sent from
//! `sending/`. A heartbeat that failed leaves them there, and the next read moves the fresh ones
//! over them, so an area recorded in both is one file and counts once. Hooks never touch
//! `sending/`. Nothing here holds a path or a work item.

use std::collections::BTreeSet;
use std::fs::OpenOptions;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use dibs_agent_host::ports::UncheckedEdits;
use dibs_client::state::StateDir;
use dibs_domain::area::QualifiedArea;
use dibs_domain::edit::ReportedEdits;

/// The name of the spool in the project's state.
const NAME: &str = "edits";

/// The file that says an edit was made outside every area.
const OUTSIDE: &str = "outside";

/// What stands between the repository and the area in a file's name, as `:` cannot be in a file
/// name everywhere.
const SEPARATOR: char = '+';

/// The directory of the spool that holds what a heartbeat in flight carries.
const SENDING: &str = "sending";

/// The spool directory of the project.
fn spool_dir(state: &StateDir) -> PathBuf {
    state.spool_dir(NAME)
}

/// The name of the file that records an edit in `area`.
fn file_of(area: &QualifiedArea) -> String {
    format!("{}{SEPARATOR}{}", area.repo, area.name)
}

/// The area a file's name records, when it records one.
fn area_of(name: &str) -> Option<QualifiedArea> {
    let (repo, area) = name.split_once(SEPARATOR)?;
    QualifiedArea::parse(&format!("{repo}:{area}")).ok()
}

/// Makes the file at `path` unless it is there; a file that cannot be made is skipped.
fn mark(path: &Path) {
    match OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(_) => {}
        Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
        Err(error) => tracing::debug!(error = %error, "an edit could not be recorded"),
    }
}

/// The edits of the project on this machine, as the guard's port: every record goes to the spool,
/// and none waits.
pub struct StateEdits {
    state: StateDir,
}

impl StateEdits {
    /// The edits kept in `state`.
    pub fn new(state: StateDir) -> Self {
        Self { state }
    }
}

impl UncheckedEdits for StateEdits {
    fn record(&self, areas: &[QualifiedArea], outside: bool) {
        let dir = spool_dir(&self.state);
        if let Err(error) = std::fs::create_dir_all(&dir) {
            tracing::debug!(error = %error, "the edits could not be recorded");
            return;
        }
        for area in areas {
            mark(&dir.join(file_of(area)));
        }
        if outside {
            mark(&dir.join(OUTSIDE));
        }
    }
}

/// What the spool held when it was read: the edits to report and the files they were read from.
#[derive(Debug, Default)]
pub struct Recorded {
    /// The edits, at most [`ReportedEdits::MAX_AREAS`] areas of them.
    pub edits: ReportedEdits,
    files: Vec<PathBuf>,
}

/// The names of the files in `dir`, sorted; none when it cannot be read.
fn file_names(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
        .filter_map(|entry| entry.file_name().into_string().ok())
        .collect();
    names.sort_unstable();
    names
}

/// What the spool holds, for the next heartbeat: the first [`ReportedEdits::MAX_AREAS`] areas by
/// name, and whether an edit was made outside every area, from what a heartbeat that failed left
/// in flight and what was recorded since, each moved in flight as it is read. A file whose name
/// records nothing this dibs reads is taken with them, so it is removed and never sent; a file that
/// cannot be moved stays in the spool for a later read.
pub fn recorded(state: &StateDir) -> Recorded {
    let dir = spool_dir(state);
    let fresh = file_names(&dir);
    let sending = dir.join(SENDING);
    if !fresh.is_empty()
        && let Err(error) = std::fs::create_dir_all(&sending)
    {
        tracing::debug!(error = %error, "the edits could not be read");
    }
    for name in fresh {
        if let Err(error) = std::fs::rename(dir.join(&name), sending.join(&name)) {
            tracing::debug!(error = %error, "a recorded edit could not be read");
        }
    }
    let mut areas = BTreeSet::new();
    let mut outside = false;
    let mut files = Vec::new();
    for name in file_names(&sending) {
        if name == OUTSIDE {
            outside = true;
        } else if let Some(area) = area_of(&name) {
            if areas.len() == ReportedEdits::MAX_AREAS {
                continue;
            }
            areas.insert(area);
        }
        files.push(sending.join(name));
    }
    let edits = ReportedEdits::new(areas, outside).unwrap_or_default();
    Recorded { edits, files }
}

/// Removes what `recorded` was read from, as a heartbeat the server accepted carried it. An edit
/// recorded since it was read is in a file of its own, which stays for the next heartbeat.
pub fn sent(recorded: &Recorded) {
    for file in &recorded.files {
        if let Err(error) = std::fs::remove_file(file)
            && error.kind() != ErrorKind::NotFound
        {
            tracing::debug!(error = %error, "a sent edit could not be removed");
        }
    }
    if let Some(sending) = recorded.files.first().and_then(|file| file.parent())
        && let Err(error) = std::fs::remove_dir(sending)
        && !matches!(
            error.kind(),
            ErrorKind::NotFound | ErrorKind::DirectoryNotEmpty
        )
    {
        tracing::debug!(error = %error, "the edits in flight could not be tidied away");
    }
}

#[cfg(test)]
mod tests {
    use dibs_client::credentials::CredentialKey;

    use super::*;

    fn state(dir: &tempfile::TempDir) -> StateDir {
        let key = CredentialKey {
            server: dibs_client::server_url::ServerUrl::parse("https://dibs.example.com").unwrap(),
            project: dibs_domain::project::ProjectSlug::parse("widgets").unwrap(),
        };
        StateDir::new(dir.path(), &key)
    }

    fn area(text: &str) -> QualifiedArea {
        QualifiedArea::parse(text).unwrap()
    }

    fn named(edits: &ReportedEdits) -> Vec<String> {
        edits.areas().iter().map(ToString::to_string).collect()
    }

    #[test]
    fn an_area_recorded_twice_is_one_record_and_outside_every_area_is_one_more() {
        let dir = tempfile::tempdir().unwrap();
        let edits = StateEdits::new(state(&dir));
        edits.record(&[area("api:billing"), area("web:checkout")], false);
        edits.record(&[area("api:billing")], true);
        edits.record(&[], true);
        let recorded = recorded(&state(&dir));
        assert_eq!(named(&recorded.edits), ["api:billing", "web:checkout"]);
        assert!(recorded.edits.outside());
    }

    #[test]
    fn what_was_sent_is_removed_and_what_came_after_stays() {
        let dir = tempfile::tempdir().unwrap();
        let edits = StateEdits::new(state(&dir));
        edits.record(&[area("api:billing")], true);
        let first = recorded(&state(&dir));
        edits.record(&[area("api:search")], false);
        sent(&first);
        let next = recorded(&state(&dir));
        assert_eq!(named(&next.edits), ["api:search"]);
        assert!(!next.edits.outside());
        sent(&next);
        assert!(recorded(&state(&dir)).edits.is_empty());
    }

    #[test]
    fn an_area_recorded_again_while_its_heartbeat_is_in_flight_stays_for_the_next() {
        let dir = tempfile::tempdir().unwrap();
        let edits = StateEdits::new(state(&dir));
        edits.record(&[area("api:billing")], true);
        let first = recorded(&state(&dir));
        assert_eq!(named(&first.edits), ["api:billing"]);
        edits.record(&[area("api:billing")], true);
        sent(&first);
        let next = recorded(&state(&dir));
        assert_eq!(named(&next.edits), ["api:billing"]);
        assert!(next.edits.outside());
        sent(&next);
        assert!(recorded(&state(&dir)).edits.is_empty());
    }

    #[test]
    fn a_heartbeat_that_failed_leaves_its_edits_for_the_next_once() {
        let dir = tempfile::tempdir().unwrap();
        let edits = StateEdits::new(state(&dir));
        edits.record(&[area("api:billing")], false);
        let failed = recorded(&state(&dir));
        edits.record(&[area("api:billing"), area("api:search")], true);
        let next = recorded(&state(&dir));
        assert_eq!(named(&failed.edits), ["api:billing"]);
        assert_eq!(named(&next.edits), ["api:billing", "api:search"]);
        assert!(next.edits.outside());
        sent(&next);
        assert!(recorded(&state(&dir)).edits.is_empty());
    }

    #[test]
    fn a_read_takes_at_most_two_hundred_areas_and_leaves_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        let edits = StateEdits::new(state(&dir));
        let many: Vec<QualifiedArea> = (0..205)
            .map(|n| area(&format!("api:area-{n:03}")))
            .collect();
        edits.record(&many, false);
        let first = recorded(&state(&dir));
        assert_eq!(first.edits.areas().len(), ReportedEdits::MAX_AREAS);
        sent(&first);
        let rest = recorded(&state(&dir));
        assert_eq!(
            named(&rest.edits),
            [
                "api:area-200",
                "api:area-201",
                "api:area-202",
                "api:area-203",
                "api:area-204"
            ]
        );
    }

    #[test]
    fn a_file_that_records_nothing_is_dropped_unsent() {
        let dir = tempfile::tempdir().unwrap();
        let spool = spool_dir(&state(&dir));
        std::fs::create_dir_all(&spool).unwrap();
        std::fs::write(spool.join("not an area"), b"").unwrap();
        let read = recorded(&state(&dir));
        assert!(read.edits.is_empty());
        sent(&read);
        assert_eq!(std::fs::read_dir(&spool).unwrap().count(), 0);
    }

    #[test]
    fn a_spool_that_cannot_be_made_records_nothing_and_fails_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let state = state(&dir);
        let spool = spool_dir(&state);
        std::fs::create_dir_all(spool.parent().unwrap()).unwrap();
        std::fs::write(&spool, b"a file where the spool would be").unwrap();
        StateEdits::new(state.clone()).record(&[area("api:billing")], true);
        assert!(recorded(&state).edits.is_empty());
    }
}
