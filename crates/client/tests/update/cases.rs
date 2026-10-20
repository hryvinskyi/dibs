use std::fs;
use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use futures_util::future::BoxFuture;
use semver::Version;
use tempfile::TempDir;

use dibs_client::update::{
    Platform, ReleaseRef, ReleaseSource, TrustedKey, UpdateError, UpdateRequest, Updated, Wanted,
    releases, update,
};

const LINUX: &str = "x86_64-unknown-linux-gnu";
const WINDOWS: &str = "x86_64-pc-windows-msvc";

/// A key made for one test.
struct Key {
    pair: minisign::KeyPair,
}

impl Key {
    fn new() -> Self {
        Self {
            pair: minisign::KeyPair::generate_unencrypted_keypair().unwrap(),
        }
    }

    fn trusted(&self) -> TrustedKey {
        TrustedKey::parse(&self.pair.pk.to_box().unwrap().to_string())
    }

    fn sign(&self, data: &[u8]) -> Vec<u8> {
        minisign::sign(
            Some(&self.pair.pk),
            &self.pair.sk,
            Cursor::new(data),
            Some("dibs release"),
            None,
        )
        .unwrap()
        .to_string()
        .into_bytes()
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// The archive of one platform's release, its binary holding `contents`.
fn archive(platform: &Platform, version: &Version, contents: &[u8]) -> Vec<u8> {
    let inner = platform.inner(version);
    if platform.archive(version).ends_with(".zip") {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        writer
            .start_file(inner, zip::write::SimpleFileOptions::default())
            .unwrap();
        writer.write_all(contents).unwrap();
        return writer.finish().unwrap().into_inner();
    }
    let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
        Vec::new(),
        flate2::Compression::fast(),
    ));
    let mut header = tar::Header::new_gnu();
    header.set_size(contents.len() as u64);
    header.set_mode(0o755);
    header.set_cksum();
    builder.append_data(&mut header, inner, contents).unwrap();
    builder.into_inner().unwrap().finish().unwrap()
}

/// A directory of releases, laid out as the releases are.
struct Releases {
    dir: TempDir,
}

impl Releases {
    fn new() -> Self {
        Self {
            dir: tempfile::tempdir().unwrap(),
        }
    }

    fn url(&self) -> String {
        url::Url::from_directory_path(self.dir.path())
            .unwrap()
            .to_string()
    }

    fn tag_dir(&self, version: &str) -> PathBuf {
        self.dir.path().join(format!("download/v{version}"))
    }

    /// Publishes `version` for Linux and Windows, signed by `key`, each binary holding
    /// `dibs <version>`; `latest` also makes it the newest release.
    fn publish(&self, version: &str, key: &Key, latest: bool) {
        let parsed = Version::parse(version).unwrap();
        let dir = self.tag_dir(version);
        fs::create_dir_all(&dir).unwrap();
        let mut sums = String::new();
        for target in [LINUX, WINDOWS] {
            let platform = Platform::new(target);
            let name = platform.archive(&parsed);
            let bytes = archive(&platform, &parsed, format!("dibs {version}").as_bytes());
            fs::write(dir.join(&name), &bytes).unwrap();
            sums.push_str(&format!("{}  {name}\n", sha256_hex(&bytes)));
        }
        fs::write(dir.join("SHA256SUMS"), &sums).unwrap();
        fs::write(dir.join("SHA256SUMS.minisig"), key.sign(sums.as_bytes())).unwrap();
        if latest {
            let newest = self.dir.path().join("latest/download");
            let _ = fs::remove_dir_all(&newest);
            fs::create_dir_all(&newest).unwrap();
            for entry in fs::read_dir(&dir).unwrap() {
                let entry = entry.unwrap();
                fs::copy(entry.path(), newest.join(entry.file_name())).unwrap();
            }
        }
    }
}

/// An installed `dibs` of `version` in a directory of its own.
struct Installed {
    dir: TempDir,
    binary: PathBuf,
    contents: Vec<u8>,
}

impl Installed {
    fn new(target: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let name = if target.contains("windows") {
            "dibs.exe"
        } else {
            "dibs"
        };
        let binary = dir.path().join(name);
        let contents = b"dibs 0.1.0".to_vec();
        fs::write(&binary, &contents).unwrap();
        Self {
            dir,
            binary,
            contents,
        }
    }

    fn request(&self, target: &str, wanted: Wanted) -> UpdateRequest {
        UpdateRequest {
            installed: Version::new(0, 1, 0),
            platform: Platform::new(target),
            wanted,
            binary: self.binary.clone(),
        }
    }

    fn unchanged(&self) {
        assert_eq!(fs::read(&self.binary).unwrap(), self.contents);
        let names: Vec<_> = fs::read_dir(self.dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(
            names.len(),
            1,
            "nothing is left beside the binary: {names:?}"
        );
    }
}

fn exactly(version: &str) -> Wanted {
    Wanted::Exactly(Version::parse(version).unwrap())
}

async fn run(
    key: &TrustedKey,
    releases_dir: &Releases,
    request: &UpdateRequest,
) -> Result<Updated, UpdateError> {
    let source = releases(&releases_dir.url()).unwrap();
    update(key, source.as_ref(), request).await
}

#[tokio::test]
async fn a_signed_release_replaces_the_binary_on_every_platform() {
    let key = Key::new();
    let releases = Releases::new();
    releases.publish("0.2.0", &key, true);
    for target in [LINUX, WINDOWS] {
        for wanted in [Wanted::Latest, exactly("0.2.0")] {
            let installed = Installed::new(target);
            let updated = run(
                &key.trusted(),
                &releases,
                &installed.request(target, wanted.clone()),
            )
            .await
            .unwrap();
            assert_eq!(
                updated,
                Updated::Installed {
                    from: Version::new(0, 1, 0),
                    to: Version::new(0, 2, 0)
                },
                "{target} {wanted:?}"
            );
            assert_eq!(fs::read(&installed.binary).unwrap(), b"dibs 0.2.0");
        }
    }
}

#[tokio::test]
async fn a_tampered_archive_is_refused_by_its_checksum() {
    let key = Key::new();
    let releases = Releases::new();
    releases.publish("0.2.0", &key, false);
    let archive = releases
        .tag_dir("0.2.0")
        .join(Platform::new(LINUX).archive(&Version::new(0, 2, 0)));
    let mut bytes = fs::read(&archive).unwrap();
    bytes.extend_from_slice(b"a backdoor");
    fs::write(&archive, bytes).unwrap();
    let installed = Installed::new(LINUX);
    let refusal = run(
        &key.trusted(),
        &releases,
        &installed.request(LINUX, exactly("0.2.0")),
    )
    .await
    .unwrap_err();
    assert!(
        matches!(refusal, UpdateError::Checksum { .. }),
        "{refusal:?}"
    );
    assert!(
        refusal.to_string().contains("nothing was installed"),
        "{refusal}"
    );
    installed.unchanged();
}

#[tokio::test]
async fn a_tampered_checksum_file_is_refused_by_its_signature() {
    let key = Key::new();
    let releases = Releases::new();
    releases.publish("0.2.0", &key, false);
    let version = Version::new(0, 2, 0);
    let platform = Platform::new(LINUX);
    let evil = archive(&platform, &version, b"not the released binary");
    let name = platform.archive(&version);
    fs::write(releases.tag_dir("0.2.0").join(&name), &evil).unwrap();
    fs::write(
        releases.tag_dir("0.2.0").join("SHA256SUMS"),
        format!("{}  {name}\n", sha256_hex(&evil)),
    )
    .unwrap();
    let installed = Installed::new(LINUX);
    let refusal = run(
        &key.trusted(),
        &releases,
        &installed.request(LINUX, exactly("0.2.0")),
    )
    .await
    .unwrap_err();
    assert_eq!(refusal, UpdateError::BadSignature);
    installed.unchanged();
}

#[tokio::test]
async fn a_missing_signature_or_one_by_another_key_is_refused() {
    let key = Key::new();
    let releases = Releases::new();
    releases.publish("0.2.0", &key, false);
    let installed = Installed::new(LINUX);
    let request = installed.request(LINUX, exactly("0.2.0"));
    let stranger = Key::new();
    let refusal = run(&stranger.trusted(), &releases, &request)
        .await
        .unwrap_err();
    assert_eq!(refusal, UpdateError::BadSignature);
    fs::remove_file(releases.tag_dir("0.2.0").join("SHA256SUMS.minisig")).unwrap();
    let refusal = run(&key.trusted(), &releases, &request).await.unwrap_err();
    assert_eq!(refusal, UpdateError::Unsigned);
    assert!(refusal.to_string().contains("no signature"), "{refusal}");
    installed.unchanged();
}

#[tokio::test]
async fn an_older_release_is_never_installed_and_the_same_one_changes_nothing() {
    let key = Key::new();
    let releases = Releases::new();
    releases.publish("0.0.9", &key, true);
    releases.publish("0.1.0", &key, false);
    let installed = Installed::new(LINUX);
    for wanted in [Wanted::Latest, exactly("0.0.9")] {
        let refusal = run(
            &key.trusted(),
            &releases,
            &installed.request(LINUX, wanted.clone()),
        )
        .await
        .unwrap_err();
        assert!(
            matches!(refusal, UpdateError::Downgrade { .. }),
            "{wanted:?}: {refusal:?}"
        );
        assert!(
            refusal.to_string().contains("never installs an older"),
            "{refusal}"
        );
    }
    let same = run(
        &key.trusted(),
        &releases,
        &installed.request(LINUX, exactly("0.1.0")),
    )
    .await
    .unwrap();
    assert_eq!(same, Updated::Current(Version::new(0, 1, 0)));
    installed.unchanged();
}

#[tokio::test]
async fn a_release_whose_signed_file_lists_no_archive_for_the_platform_is_refused() {
    let key = Key::new();
    let releases = Releases::new();
    releases.publish("0.2.0", &key, true);
    let target = "aarch64-apple-darwin";
    let installed = Installed::new(target);
    let refusal = run(
        &key.trusted(),
        &releases,
        &installed.request(target, Wanted::Latest),
    )
    .await
    .unwrap_err();
    assert!(
        matches!(refusal, UpdateError::NotListed { .. }),
        "{refusal:?}"
    );
    installed.unchanged();
}

/// A source that counts what it is asked for and has nothing.
#[derive(Default)]
struct Counting(AtomicUsize);

impl ReleaseSource for Counting {
    fn fetch<'a>(
        &'a self,
        _release: &'a ReleaseRef,
        _asset: &'a str,
        _limit: usize,
    ) -> BoxFuture<'a, Result<Option<Vec<u8>>, UpdateError>> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(None) })
    }
}

#[tokio::test]
async fn the_placeholder_key_refuses_every_update_before_any_download() {
    let placeholder = TrustedKey::parse(
        "untrusted comment: no release signing key has been made for this repository yet\nplaceholder\n",
    );
    let key = Key::new();
    let releases = Releases::new();
    releases.publish("0.2.0", &key, true);
    let installed = Installed::new(LINUX);
    for wanted in [Wanted::Latest, exactly("0.2.0"), exactly("0.0.1")] {
        let refusal = run(
            &placeholder,
            &releases,
            &installed.request(LINUX, wanted.clone()),
        )
        .await
        .unwrap_err();
        assert_eq!(refusal, UpdateError::NoKey, "{wanted:?}");
        let counting = Counting::default();
        let refusal = update(&placeholder, &counting, &installed.request(LINUX, wanted))
            .await
            .unwrap_err();
        assert_eq!(refusal, UpdateError::NoKey);
        assert_eq!(
            counting.0.load(Ordering::SeqCst),
            0,
            "nothing is downloaded"
        );
    }
    assert!(UpdateError::NoKey.to_string().contains("install script"));
    installed.unchanged();
}

#[cfg(unix)]
#[tokio::test]
async fn an_install_directory_this_user_cannot_write_refuses_before_any_download() {
    use std::os::unix::fs::PermissionsExt;
    let key = Key::new();
    let releases = Releases::new();
    releases.publish("0.2.0", &key, true);
    let installed = Installed::new(LINUX);
    let dir: &Path = installed.dir.path();
    fs::set_permissions(dir, fs::Permissions::from_mode(0o555)).unwrap();
    if tempfile::tempfile_in(dir).is_ok() {
        fs::set_permissions(dir, fs::Permissions::from_mode(0o755)).unwrap();
        return;
    }
    let counting = Counting::default();
    let refusal = update(
        &key.trusted(),
        &counting,
        &installed.request(LINUX, Wanted::Latest),
    )
    .await
    .unwrap_err();
    fs::set_permissions(dir, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(
        matches!(refusal, UpdateError::Unwritable { .. }),
        "{refusal:?}"
    );
    assert!(refusal.to_string().contains("install script"), "{refusal}");
    assert_eq!(counting.0.load(Ordering::SeqCst), 0);
    installed.unchanged();
}
