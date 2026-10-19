//! Embeds the browser bundle: every `.js` and `.css` file of `web/src`, under one directory hash.
//!
//! The hash is taken over the names and contents of the whole set, so a change to any file moves
//! every file to a new directory, and a module's relative imports still resolve beside it. The
//! generated `bundle.rs` holds the hash and each file's name and bytes.

use std::error::Error;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// How many hex characters of the digest name the directory.
const HASH_CHARS: usize = 16;

fn main() -> Result<(), Box<dyn Error>> {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR")?);
    let source = manifest.join("../../web/src");
    println!("cargo:rerun-if-changed={}", source.display());
    let files = bundle_files(&source)?;
    let mut hasher = Sha256::new();
    for (name, path) in &files {
        println!("cargo:rerun-if-changed={}", path.display());
        let bytes = fs::read(path)?;
        hasher.update((name.len() as u64).to_le_bytes());
        hasher.update(name.as_bytes());
        hasher.update((bytes.len() as u64).to_le_bytes());
        hasher.update(&bytes);
    }
    let mut hash = String::new();
    for byte in hasher.finalize() {
        write!(hash, "{byte:02x}")?;
    }
    hash.truncate(HASH_CHARS);
    let mut code = String::new();
    writeln!(
        code,
        "/// The directory every file of the bundle is served under."
    )?;
    writeln!(code, "pub const BUNDLE_HASH: &str = {hash:?};")?;
    writeln!(code, "/// Each file of the bundle: its name and its bytes.")?;
    writeln!(code, "pub static BUNDLE: &[(&str, &[u8])] = &[")?;
    for (name, path) in &files {
        let absolute = fs::canonicalize(path)?;
        let absolute = absolute.to_str().ok_or("a bundle path is not UTF-8")?;
        writeln!(code, "    ({name:?}, include_bytes!({absolute:?})),")?;
    }
    writeln!(code, "];")?;
    let out = PathBuf::from(std::env::var("OUT_DIR")?).join("bundle.rs");
    fs::write(out, code)?;
    Ok(())
}

/// The `.js` and `.css` files directly inside `dir`, sorted by name; hidden files are skipped.
fn bundle_files(dir: &Path) -> Result<Vec<(String, PathBuf)>, Box<dyn Error>> {
    let mut files = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let served = !name.starts_with('.') && (name.ends_with(".js") || name.ends_with(".css"));
        if served {
            files.push((name, entry.path()));
        }
    }
    files.sort();
    Ok(files)
}
