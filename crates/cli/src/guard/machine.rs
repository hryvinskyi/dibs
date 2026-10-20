//! The id of this machine for the projects' reports: 128 random bits, made once and kept in the
//! state directory as `machine-id`, so a project counts one machine as one whatever it is called.
//! The id travels with the reports only, and the server shows it to nobody.

use std::fs::{self, OpenOptions};
use std::path::Path;

use rand::RngExt as _;

use dibs_client::state::write_json_durably;
use dibs_domain::report::MachineId;

/// The file the id is kept in, under the state directory.
const FILE: &str = "machine-id";

/// This machine's id, made and kept the first time it is asked for; `None` when it can be neither
/// read nor kept, in which case the reports wait for a run that can.
pub fn machine_id(state_dir: &Path) -> Option<MachineId> {
    let path = state_dir.join(FILE);
    if let Some(id) = read(&path) {
        return Some(id);
    }
    fs::create_dir_all(state_dir).ok()?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(state_dir.join(format!("{FILE}.lock")))
        .ok()?;
    lock.lock().ok()?;
    if let Some(id) = read(&path) {
        return Some(id);
    }
    let id = MachineId::from_bits(rand::rng().random::<u128>());
    match write_json_durably(&path, &id) {
        Ok(()) => Some(id),
        Err(error) => {
            tracing::debug!(error = %error, "this machine's id could not be kept");
            None
        }
    }
}

fn read(path: &Path) -> Option<MachineId> {
    let text = fs::read(path).ok()?;
    serde_json::from_slice(&text).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_id_is_made_once_and_kept() {
        let dir = tempfile::tempdir().unwrap();
        let first = machine_id(dir.path()).unwrap();
        assert_eq!(first.as_str().len(), 32);
        assert_eq!(machine_id(dir.path()), Some(first.clone()));
        let other = tempfile::tempdir().unwrap();
        assert_ne!(machine_id(other.path()), Some(first));
    }
}
