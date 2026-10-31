//! The agents' workflow text has one source, `docs/agent-workflow.md`; each integration under
//! `integrations/<host>/` declares in its `workflow.toml` which of its files carry that text and
//! what goes before it there:
//!
//! ```toml
//! [[render]]
//! path = "skills/dibs/SKILL.md"
//! header = """
//! ---
//! name: dibs
//! ---
//! """
//! ```
//!
//! Rendering writes each declared file as its header, a line saying where the text comes from,
//! and the source. Checking renders in memory and names every file that differs, so a change to
//! the source that was not rendered, or a hand edit of a rendered file, fails.
//!
//! An integration also declares the values its JSON files must repeat from dibs's code, each by
//! the JSON pointer to a number and the name of the value:
//!
//! ```toml
//! [[stamp]]
//! path = "hooks/hooks.json"
//! pointer = "/hooks/WorktreeWanted/0/timeout"
//! value = "worktree_hook_timeout"
//! ```
//!
//! Rendering writes the value there, the file written back with its keys in order and indented by
//! two spaces; checking fails when the file differs from that, so the file and the code cannot
//! drift.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

use serde::Deserialize;
use thiserror::Error;

/// The source of the workflow text, relative to the repository's root.
pub const SOURCE: &str = "docs/agent-workflow.md";

/// The directory holding one directory per integration.
pub const INTEGRATIONS: &str = "integrations";

/// The file each integration declares its rendered files in.
pub const MANIFEST: &str = "workflow.toml";

/// The line between a rendered file's header and the text.
pub const MARKER: &str = "<!-- Rendered from docs/agent-workflow.md by `cargo xtask integrations`; edit that file, not this one. -->";

/// Why rendering could not be done.
#[derive(Debug, Error)]
pub enum RenderError {
    /// A file could not be read or written.
    #[error("{path}: {source}")]
    Io {
        /// The file.
        path: PathBuf,
        /// Why.
        #[source]
        source: io::Error,
    },
    /// An integration's manifest is not valid.
    #[error("{path}: {reason}")]
    Manifest {
        /// The manifest.
        path: PathBuf,
        /// The rule broken.
        reason: String,
    },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    #[serde(default)]
    render: Vec<Target>,
    #[serde(default)]
    stamp: Vec<Stamp>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Stamp {
    path: String,
    pointer: String,
    value: String,
}

/// The value named `name` that an integration's files repeat from dibs's code, as JSON; `None`
/// for a name dibs does not know.
fn stamped_value(name: &str) -> Option<serde_json::Value> {
    match name {
        "worktree_hook_timeout" => Some(serde_json::Value::from(
            dibs_agent_host::adapter::WorktreeTerms::HOOK_TIMEOUT.as_secs(),
        )),
        _ => None,
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Target {
    path: String,
    #[serde(default)]
    header: String,
}

/// One rendered file and what it must hold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rendered {
    /// The file, relative to the repository's root.
    pub path: PathBuf,
    /// Its text.
    pub text: String,
}

/// Every file the integrations under `root` declare, rendered from the source. No
/// `integrations` directory is no file.
///
/// # Errors
/// [`RenderError`] when the source or a manifest cannot be read, or a manifest is not valid.
pub fn render(root: &Path) -> Result<Vec<Rendered>, RenderError> {
    let integrations = root.join(INTEGRATIONS);
    if !integrations.is_dir() {
        return Ok(Vec::new());
    }
    let source = read(&root.join(SOURCE))?;
    let mut dirs: Vec<PathBuf> = fs::read_dir(&integrations)
        .map_err(|source| io_error(&integrations, source))?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    dirs.sort();
    let mut rendered = Vec::new();
    for dir in dirs {
        let manifest_path = dir.join(MANIFEST);
        let invalid = |reason: String| RenderError::Manifest {
            path: relative(root, &manifest_path),
            reason,
        };
        if !manifest_path.is_file() {
            return Err(invalid(
                "every integration declares the files its workflow text goes into".to_owned(),
            ));
        }
        let manifest: Manifest = toml::from_str(&read(&manifest_path)?)
            .map_err(|error| invalid(error.message().to_owned()))?;
        if manifest.render.is_empty() {
            return Err(invalid("it declares no [[render]] file".to_owned()));
        }
        for target in manifest.render {
            let inside = inside(&target.path).ok_or_else(|| {
                invalid(format!(
                    "'{}' is not a path inside the integration",
                    target.path
                ))
            })?;
            rendered.push(Rendered {
                path: relative(root, &dir.join(inside)),
                text: compose(&target.header, &source),
            });
        }
        let mut stamped: BTreeMap<PathBuf, serde_json::Value> = BTreeMap::new();
        for stamp in manifest.stamp {
            let inside = inside(&stamp.path).ok_or_else(|| {
                invalid(format!(
                    "'{}' is not a path inside the integration",
                    stamp.path
                ))
            })?;
            let value = stamped_value(&stamp.value)
                .ok_or_else(|| invalid(format!("dibs has no value named '{}'", stamp.value)))?;
            let file = dir.join(inside);
            let document = match stamped.entry(file.clone()) {
                std::collections::btree_map::Entry::Occupied(entry) => entry.into_mut(),
                std::collections::btree_map::Entry::Vacant(entry) => {
                    let text = read(&file)?;
                    let parsed: serde_json::Value = serde_json::from_str(&text)
                        .map_err(|error| invalid(format!("{} is not JSON: {error}", stamp.path)))?;
                    entry.insert(parsed)
                }
            };
            match document.pointer_mut(&stamp.pointer) {
                Some(slot) if slot.is_number() => *slot = value,
                _ => {
                    return Err(invalid(format!(
                        "{} has no number at {}",
                        stamp.path, stamp.pointer
                    )));
                }
            }
        }
        for (file, document) in stamped {
            let mut text = serde_json::to_string_pretty(&document).map_err(|error| {
                invalid(format!("{} could not be written: {error}", file.display()))
            })?;
            text.push('\n');
            rendered.push(Rendered {
                path: relative(root, &file),
                text,
            });
        }
    }
    Ok(rendered)
}

/// Writes every rendered file; answers the ones whose text changed.
///
/// # Errors
/// [`RenderError`].
pub fn write(root: &Path) -> Result<Vec<PathBuf>, RenderError> {
    let mut changed = Vec::new();
    for file in render(root)? {
        let path = root.join(&file.path);
        if fs::read_to_string(&path).ok().as_deref() == Some(file.text.as_str()) {
            continue;
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|source| io_error(parent, source))?;
        }
        fs::write(&path, &file.text).map_err(|source| io_error(&path, source))?;
        changed.push(file.path);
    }
    Ok(changed)
}

/// The rendered files whose text on disk is not what rendering gives.
///
/// # Errors
/// [`RenderError`].
pub fn stale(root: &Path) -> Result<Vec<PathBuf>, RenderError> {
    Ok(render(root)?
        .into_iter()
        .filter(|file| {
            fs::read_to_string(root.join(&file.path)).ok().as_deref() != Some(file.text.as_str())
        })
        .map(|file| file.path)
        .collect())
}

fn compose(header: &str, source: &str) -> String {
    let mut text = String::new();
    if !header.is_empty() {
        text.push_str(header);
        if !header.ends_with('\n') {
            text.push('\n');
        }
    }
    text.push_str(MARKER);
    text.push_str("\n\n");
    text.push_str(source);
    if !source.ends_with('\n') {
        text.push('\n');
    }
    text
}

/// The path when it stays inside its integration: relative, with no `..` and no root.
fn inside(path: &str) -> Option<PathBuf> {
    let path = Path::new(path);
    let plain = path
        .components()
        .all(|component| matches!(component, Component::Normal(_)));
    (plain && path.components().next().is_some()).then(|| path.to_path_buf())
}

fn relative(root: &Path, path: &Path) -> PathBuf {
    path.strip_prefix(root).unwrap_or(path).to_path_buf()
}

fn read(path: &Path) -> Result<String, RenderError> {
    fs::read_to_string(path).map_err(|source| io_error(path, source))
}

fn io_error(path: &Path, source: io::Error) -> RenderError {
    RenderError::Io {
        path: path.to_path_buf(),
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("docs")).unwrap();
        fs::write(
            dir.path().join(SOURCE),
            "# Working with dibs\n\nClaim first.\n",
        )
        .unwrap();
        dir
    }

    fn integration(root: &Path, name: &str, manifest: &str) {
        let dir = root.join(INTEGRATIONS).join(name);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(MANIFEST), manifest).unwrap();
    }

    #[test]
    fn without_integrations_there_is_nothing_to_render() {
        let dir = repo();
        assert!(render(dir.path()).unwrap().is_empty());
        assert!(write(dir.path()).unwrap().is_empty());
        assert!(stale(dir.path()).unwrap().is_empty());
    }

    #[test]
    fn each_declared_file_gets_its_header_the_marker_and_the_source() {
        let dir = repo();
        integration(
            dir.path(),
            "alpha",
            "[[render]]\npath = \"skills/dibs/GUIDE.md\"\nheader = \"---\\nname: dibs\\n---\"\n",
        );
        integration(dir.path(), "beta", "[[render]]\npath = \"AGENTS.md\"\n");
        let changed = write(dir.path()).unwrap();
        assert_eq!(
            changed,
            [
                PathBuf::from("integrations/alpha/skills/dibs/GUIDE.md"),
                PathBuf::from("integrations/beta/AGENTS.md")
            ]
        );
        let guide =
            fs::read_to_string(dir.path().join("integrations/alpha/skills/dibs/GUIDE.md")).unwrap();
        assert_eq!(
            guide,
            format!("---\nname: dibs\n---\n{MARKER}\n\n# Working with dibs\n\nClaim first.\n")
        );
        assert!(stale(dir.path()).unwrap().is_empty());
        assert!(
            write(dir.path()).unwrap().is_empty(),
            "a second run changes nothing"
        );
    }

    #[test]
    fn a_source_change_or_a_hand_edit_makes_the_file_stale() {
        let dir = repo();
        integration(dir.path(), "beta", "[[render]]\npath = \"AGENTS.md\"\n");
        write(dir.path()).unwrap();
        fs::write(
            dir.path().join(SOURCE),
            "# Working with dibs\n\nClaim before editing.\n",
        )
        .unwrap();
        assert_eq!(
            stale(dir.path()).unwrap(),
            [PathBuf::from("integrations/beta/AGENTS.md")]
        );
        write(dir.path()).unwrap();
        fs::write(dir.path().join("integrations/beta/AGENTS.md"), "edited").unwrap();
        assert_eq!(stale(dir.path()).unwrap().len(), 1);
    }

    /// A hook configuration whose `wait` hook has the timeout `timeout`.
    fn hooks(timeout: u64) -> String {
        format!(
            "{{\n  \"hooks\": {{\n    \"Wait\": [\n      {{\n        \"command\": \"sh wait\",\n        \"timeout\": {timeout}\n      }}\n    ]\n  }}\n}}\n"
        )
    }

    const STAMPED: &str = "[[render]]\npath = \"AGENTS.md\"\n\n[[stamp]]\npath = \"hooks/hooks.json\"\npointer = \"/hooks/Wait/0/timeout\"\nvalue = \"worktree_hook_timeout\"\n";

    #[test]
    fn a_stamped_value_is_written_from_the_code_and_a_drift_is_stale() {
        let dir = repo();
        integration(dir.path(), "delta", STAMPED);
        let file = dir.path().join("integrations/delta/hooks/hooks.json");
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, hooks(5)).unwrap();
        assert!(
            stale(dir.path())
                .unwrap()
                .contains(&PathBuf::from("integrations/delta/hooks/hooks.json")),
            "a value that is not the code's is stale"
        );
        write(dir.path()).unwrap();
        let timeout = dibs_agent_host::adapter::WorktreeTerms::HOOK_TIMEOUT.as_secs();
        assert_eq!(fs::read_to_string(&file).unwrap(), hooks(timeout));
        assert!(stale(dir.path()).unwrap().is_empty());
        assert!(write(dir.path()).unwrap().is_empty());
    }

    #[test]
    fn a_stamp_names_a_value_dibs_knows_and_a_number_its_file_has() {
        for (manifest, file) in [
            (STAMPED.replace("worktree_hook_timeout", "nap"), hooks(5)),
            (STAMPED.replace("/Wait/0", "/Sleep/0"), hooks(5)),
            (
                STAMPED.to_owned(),
                "{\"hooks\": {\"Wait\": [{\"timeout\": \"5\"}]}}".to_owned(),
            ),
            (STAMPED.to_owned(), "not json".to_owned()),
            (
                STAMPED.replace("hooks/hooks.json", "../hooks.json"),
                hooks(5),
            ),
        ] {
            let dir = repo();
            integration(dir.path(), "delta", &manifest);
            let path = dir.path().join("integrations/delta/hooks/hooks.json");
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, &file).unwrap();
            let error = render(dir.path()).unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("integrations/delta/workflow.toml"),
                "{manifest}: {error}"
            );
        }
    }

    #[test]
    fn a_manifest_must_exist_declare_something_and_stay_inside_its_directory() {
        for manifest in [
            None,
            Some(""),
            Some("[[render]]\npath = \"../escape.md\"\n"),
            Some("[[render]]\npath = \"/abs.md\"\n"),
            Some("[[render]]\nfile = \"x.md\"\n"),
        ] {
            let dir = repo();
            match manifest {
                Some(text) => integration(dir.path(), "gamma", text),
                None => fs::create_dir_all(dir.path().join(INTEGRATIONS).join("gamma")).unwrap(),
            }
            let error = render(dir.path()).unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("integrations/gamma/workflow.toml"),
                "{manifest:?}: {error}"
            );
        }
    }
}
