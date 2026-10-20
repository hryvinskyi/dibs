//! A release's `SHA256SUMS`: one `<64 hex digits>  <file name>` line per archive, as `sha256sum`
//! writes it (a `*` before the name, binary mode, is read the same). Lines in no such shape are
//! left out.

use semver::Version;

use crate::update::archive::Platform;

/// The digests a checksum file lists, by file name.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Sums(Vec<(String, [u8; 32])>);

impl Sums {
    /// The lines of `bytes` that list a digest and a name.
    pub fn parse(bytes: &[u8]) -> Self {
        let text = String::from_utf8_lossy(bytes);
        let entries = text
            .lines()
            .filter_map(|line| {
                let (hex, name) = line.trim().split_once(char::is_whitespace)?;
                let name = name.trim_start();
                let name = name.strip_prefix('*').unwrap_or(name).trim_end();
                Some((name.to_owned(), digest(hex)?))
            })
            .filter(|(name, _)| !name.is_empty())
            .collect();
        Self(entries)
    }

    /// The digest listed for `name`.
    pub fn digest(&self, name: &str) -> Option<[u8; 32]> {
        self.0
            .iter()
            .find(|(listed, _)| listed == name)
            .map(|(_, digest)| *digest)
    }

    /// The newest release of an archive for `platform` the file lists: its version, its name and
    /// its digest.
    pub fn newest_for(&self, platform: &Platform) -> Option<(Version, String, [u8; 32])> {
        self.0
            .iter()
            .filter_map(|(name, digest)| {
                platform
                    .version_of(name)
                    .map(|version| (version, name.clone(), *digest))
            })
            .max_by(|a, b| a.0.cmp(&b.0))
    }
}

/// 64 hexadecimal digits as 32 bytes.
fn digest(hex: &str) -> Option<[u8; 32]> {
    if hex.len() != 64 {
        return None;
    }
    let mut out = [0_u8; 32];
    for (index, byte) in out.iter_mut().enumerate() {
        let pair = hex.get(index * 2..index * 2 + 2)?;
        *byte = u8::from_str_radix(pair, 16).ok()?;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "0f0e0d0c0b0a09080706050403020100ffeeddccbbaa99887766554433221100";
    const B: &str = "00112233445566778899AABBCCDDEEFF00112233445566778899aabbccddeeff";

    #[test]
    fn each_well_formed_line_lists_one_digest() {
        let text = format!(
            "{A}  dibs-0.2.0-x86_64-unknown-linux-gnu.tar.gz\n{B} *dibs-0.2.0-x86_64-pc-windows-msvc.zip\r\nnot a line\nabcd  short.tar.gz\n"
        );
        let sums = Sums::parse(text.as_bytes());
        let linux = sums
            .digest("dibs-0.2.0-x86_64-unknown-linux-gnu.tar.gz")
            .unwrap();
        assert_eq!(linux[0], 0x0f);
        assert_eq!(linux[31], 0x00);
        let windows = sums
            .digest("dibs-0.2.0-x86_64-pc-windows-msvc.zip")
            .unwrap();
        assert_eq!(windows[10], 0xaa);
        assert_eq!(sums.digest("short.tar.gz"), None);
        assert_eq!(sums.digest("not"), None);
    }

    #[test]
    fn the_newest_archive_of_the_platform_is_found_by_its_name() {
        let text = format!(
            "{A}  dibs-0.2.0-aarch64-apple-darwin.tar.gz\n{A}  dibs-0.10.0-aarch64-apple-darwin.tar.gz\n{A}  dibs-0.11.0-x86_64-apple-darwin.tar.gz\n{A}  dibs-x-aarch64-apple-darwin.tar.gz\n"
        );
        let sums = Sums::parse(text.as_bytes());
        let (version, name, _) = sums
            .newest_for(&Platform::new("aarch64-apple-darwin"))
            .unwrap();
        assert_eq!(version, Version::new(0, 10, 0));
        assert_eq!(name, "dibs-0.10.0-aarch64-apple-darwin.tar.gz");
        assert_eq!(
            sums.newest_for(&Platform::new("x86_64-pc-windows-msvc")),
            None
        );
    }
}
