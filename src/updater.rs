//! Official release updates, separate from playback and library data.
use std::{
    fs::{self, File, OpenOptions},
    io::Read,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver},
    },
    thread,
    time::{Duration, SystemTime},
};

use anyhow::{Context, Result, bail, ensure};
use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tempfile::{NamedTempFile, TempPath};

const RELEASES_API: &str = "https://api.github.com/repos/Kumpelo/muscli/releases?per_page=100";
const DOWNLOAD_PREFIX: &str = "https://github.com/Kumpelo/muscli/releases/download/";
const MAX_DOWNLOAD: u64 = 256 * 1024 * 1024;
#[cfg(unix)]
const MAX_BINARY: u64 = 128 * 1024 * 1024;
const CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdateMode {
    Off,
    Notify,
    Automatic,
}

impl Default for UpdateMode {
    fn default() -> Self {
        if option_env!("MUSCLI_RELEASE_BUILD") == Some("1") {
            Self::Automatic
        } else {
            Self::Notify
        }
    }
}

#[derive(Debug, Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
    size: u64,
}

#[derive(Debug, Deserialize)]
struct Release {
    tag_name: String,
    draft: bool,
    prerelease: bool,
    assets: Vec<Asset>,
}

#[derive(Debug)]
pub enum UpdateEvent {
    Current,
    Available(String),
    Installed(String),
    InstallerReady(Installer),
}

#[derive(Debug)]
pub struct Installer {
    file: TempPath,
    directory: PathBuf,
    pub version: String,
    digest: String,
}

impl Installer {
    /// Called only after the player has closed on Windows.
    pub fn launch(self) -> Result<()> {
        verify_digest(File::open(&self.file)?, &self.digest)?;
        std::process::Command::new(&self.file)
            .args(["/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART", "/SP-"])
            .arg(format!("/DIR={}", self.directory.display()))
            .spawn()
            .context("could not start the verified muscli installer")?;
        // Windows keeps the installer open. Leave it in the cache until it finishes.
        let _ = self.file.keep()?;
        Ok(())
    }
}

fn agent(timeout: u64) -> ureq::Agent {
    ureq::Agent::config_builder()
        .user_agent(concat!("muscli/", env!("CARGO_PKG_VERSION")))
        .https_only(true)
        .timeout_global(Some(Duration::from_secs(timeout)))
        .build()
        .into()
}

fn read_limited(mut input: impl Read, limit: u64) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    input.by_ref().take(limit + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= limit,
        "update response exceeds its size limit"
    );
    Ok(bytes)
}

fn select_release<'a>(
    releases: &'a [Release],
    current: &Version,
) -> Option<(&'a Release, Version)> {
    releases
        .iter()
        .filter(|release| !release.draft)
        .filter_map(|release| {
            let version = Version::parse(
                release
                    .tag_name
                    .strip_prefix('v')
                    .unwrap_or(&release.tag_name),
            )
            .ok()?;
            if (release.prerelease || !version.pre.is_empty()) && current.pre.is_empty() {
                return None;
            }
            version
                .cmp_precedence(current)
                .is_gt()
                .then_some((release, version))
        })
        .max_by(|(_, left), (_, right)| left.cmp_precedence(right))
}

fn asset<'a>(release: &'a Release, name: &str) -> Result<&'a Asset> {
    let mut matches = release.assets.iter().filter(|asset| asset.name == name);
    let found = matches
        .next()
        .context("release is missing the required update asset")?;
    ensure!(
        matches.next().is_none(),
        "release has duplicate update assets"
    );
    let expected = format!("{DOWNLOAD_PREFIX}{}/{name}", release.tag_name);
    ensure!(
        found.browser_download_url == expected,
        "unexpected update download URL"
    );
    ensure!(
        found.size > 0 && found.size <= MAX_DOWNLOAD,
        "invalid update asset size"
    );
    Ok(found)
}

fn checksum(contents: &str, name: &str) -> Result<String> {
    let mut matches = contents.lines().filter_map(|line| {
        let (digest, file) = line.split_once(char::is_whitespace)?;
        (file.trim().trim_start_matches('*') == name).then_some(digest)
    });
    let value = matches.next().context("missing update checksum")?;
    ensure!(matches.next().is_none(), "duplicate update checksum");
    ensure!(
        value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "invalid SHA-256 checksum"
    );
    Ok(value.to_ascii_lowercase())
}

fn verify_digest(mut input: impl Read, expected: &str) -> Result<()> {
    let mut hash = Sha256::new();
    let mut buffer = [0; 64 * 1024];
    loop {
        let count = input.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    ensure!(
        format!("{:x}", hash.finalize()) == expected,
        "update SHA-256 verification failed"
    );
    Ok(())
}

fn download(asset: &Asset, directory: &Path, suffix: &str) -> Result<NamedTempFile> {
    let mut response = agent(120).get(&asset.browser_download_url).call()?;
    let mut file = tempfile::Builder::new()
        .prefix("muscli-update-")
        .suffix(suffix)
        .tempfile_in(directory)?;
    let count = std::io::copy(
        &mut response.body_mut().as_reader().take(asset.size + 1),
        file.as_file_mut(),
    )?;
    ensure!(
        count == asset.size,
        "update download is incomplete or oversized"
    );
    file.as_file_mut().sync_all()?;
    Ok(file)
}

fn package_name(version: &Version) -> Result<String> {
    ensure!(
        std::env::consts::ARCH == "x86_64",
        "automatic updates currently require x86_64"
    );
    match std::env::consts::OS {
        "linux" => Ok(format!("muscli-v{version}-linux-x86_64.tar.gz")),
        "windows" => Ok(format!("muscli-v{version}-windows-x86_64-setup.exe")),
        _ => bail!("automatic updates are unavailable on this platform"),
    }
}

fn inside_installation(executable: &Path, directory: &Path) -> bool {
    directory
        .canonicalize()
        .is_ok_and(|directory| executable.starts_with(directory))
}

fn installed_executable() -> Result<PathBuf> {
    let original = std::env::current_exe()?;
    let executable = original.canonicalize()?;
    ensure!(
        !executable
            .components()
            .any(|part| part.as_os_str() == "target"),
        "development checkout: build with Cargo instead of replacing this executable"
    );
    let home = std::env::var_os(if cfg!(windows) {
        "LOCALAPPDATA"
    } else {
        "HOME"
    })
    .map(PathBuf::from)
    .context("could not locate the user installation")?;
    let allowed = if cfg!(windows) {
        inside_installation(&executable, &home.join("Programs/muscli"))
    } else {
        let cargo = std::env::var_os("CARGO_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".cargo"));
        inside_installation(&executable, &home.join(".local/bin"))
            || inside_installation(&executable, &cargo.join("bin"))
    };
    ensure!(
        allowed,
        "update muscli through the package manager, or install it in the user bin directory"
    );
    // Inno Setup expects a regular Windows path, not the extended-length form.
    Ok(if cfg!(windows) { original } else { executable })
}

#[cfg(unix)]
fn install_linux(
    archive: &Path,
    target: &Path,
    version: &Version,
    cancelled: &AtomicBool,
) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let parent = target.parent().context("missing executable directory")?;
    let mut staged = NamedTempFile::new_in(parent)?;
    let reader = flate2::read::GzDecoder::new(File::open(archive)?).take(512 * 1024 * 1024);
    let mut archive = tar::Archive::new(reader);
    let expected = PathBuf::from(format!("muscli-v{version}-linux-x86_64/muscli"));
    let mut found = false;
    for (index, entry) in archive.entries()?.enumerate() {
        ensure!(index < 2048, "update archive has too many entries");
        let mut entry = entry?;
        if entry.path()?.as_ref() != expected {
            continue;
        }
        ensure!(
            !found && entry.header().entry_type().is_file(),
            "invalid executable in update archive"
        );
        let length = entry.size();
        ensure!(
            (4..=MAX_BINARY).contains(&length),
            "invalid executable size"
        );
        let count = std::io::copy(&mut entry, staged.as_file_mut())?;
        ensure!(count == length, "truncated update executable");
        found = true;
    }
    ensure!(found, "update archive does not contain muscli");
    let mut magic = [0; 4];
    File::open(staged.path())?.read_exact(&mut magic)?;
    ensure!(&magic == b"\x7fELF", "update is not a Linux executable");
    let mode = fs::metadata(target)?.permissions().mode() & 0o777;
    ensure!(mode & 0o111 != 0, "installed muscli is not executable");
    fs::set_permissions(staged.path(), fs::Permissions::from_mode(mode))?;
    staged.as_file_mut().sync_all()?;
    let mut backup = NamedTempFile::new_in(parent)?;
    std::io::copy(&mut File::open(target)?, backup.as_file_mut())?;
    fs::set_permissions(backup.path(), fs::Permissions::from_mode(mode))?;
    backup.as_file_mut().sync_all()?;
    ensure!(!cancelled.load(Ordering::Relaxed), "update cancelled");
    backup.persist(parent.join("muscli.previous"))?;
    staged.persist(target)?;
    File::open(parent)?.sync_all()?;
    Ok(())
}

pub fn update(cache: &Path, install: bool) -> Result<UpdateEvent> {
    update_cancellable(cache, install, &AtomicBool::new(false))
}

fn update_cancellable(cache: &Path, install: bool, cancelled: &AtomicBool) -> Result<UpdateEvent> {
    fs::create_dir_all(cache)?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(cache.join("update.lock"))?;
    fs4::FileExt::try_lock(&lock).context("another muscli update is running")?;
    let mut response = agent(8)
        .get(RELEASES_API)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .call()?;
    let releases: Vec<Release> = serde_json::from_slice(&read_limited(
        response.body_mut().as_reader(),
        2 * 1024 * 1024,
    )?)?;
    let current = Version::parse(env!("CARGO_PKG_VERSION"))?;
    let Some((release, version)) = select_release(&releases, &current) else {
        return Ok(UpdateEvent::Current);
    };
    if !install {
        return Ok(UpdateEvent::Available(version.to_string()));
    }
    let executable = installed_executable()?;
    let name = package_name(&version)?;
    let package = asset(release, &name)?;
    let sums_asset = asset(release, "SHA256SUMS")?;
    ensure!(sums_asset.size <= 256 * 1024, "checksum file is oversized");
    let sums = download(sums_asset, cache, ".txt")?;
    let digest = checksum(&fs::read_to_string(sums.path())?, &name)?;
    let downloaded = download(
        package,
        cache,
        if cfg!(windows) { ".exe" } else { ".tar.gz" },
    )?;
    verify_digest(File::open(downloaded.path())?, &digest)?;
    ensure!(!cancelled.load(Ordering::Relaxed), "update cancelled");
    if cfg!(windows) {
        return Ok(UpdateEvent::InstallerReady(Installer {
            file: downloaded.into_temp_path(),
            directory: executable
                .parent()
                .context("missing installation directory")?
                .to_owned(),
            version: version.to_string(),
            digest,
        }));
    }
    #[cfg(unix)]
    install_linux(downloaded.path(), &executable, &version, cancelled)?;
    Ok(UpdateEvent::Installed(version.to_string()))
}

pub struct UpdateJob {
    receiver: Receiver<Result<UpdateEvent, String>>,
    cancelled: Arc<AtomicBool>,
}

impl UpdateJob {
    pub fn try_recv(&self) -> Result<Result<UpdateEvent, String>, mpsc::TryRecvError> {
        self.receiver.try_recv()
    }
}

impl Drop for UpdateJob {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }
}

/// One startup check per day; failures remain retryable and never affect playback.
pub fn background(cache: PathBuf, mode: UpdateMode, force_install: bool) -> Option<UpdateJob> {
    if mode == UpdateMode::Off && !force_install {
        return None;
    }
    let stamp = cache.join("update-checked");
    if !force_install
        && fs::metadata(&stamp)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|time| SystemTime::now().duration_since(time).ok())
            .is_some_and(|age| age < CHECK_INTERVAL)
    {
        return None;
    }
    let (tx, rx) = mpsc::channel();
    let cancelled = Arc::new(AtomicBool::new(false));
    let worker_cancelled = Arc::clone(&cancelled);
    thread::Builder::new()
        .name("muscli-update".into())
        .spawn(move || {
            let result = update_cancellable(
                &cache,
                force_install || mode == UpdateMode::Automatic,
                &worker_cancelled,
            );
            if result.is_ok() {
                let _ = fs::write(stamp, b"checked");
            }
            let _ = tx.send(result.map_err(|error| format!("{error:#}")));
        })
        .ok()?;
    Some(UpdateJob {
        receiver: rx,
        cancelled,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(tag: &str, draft: bool, prerelease: bool) -> Release {
        Release {
            tag_name: tag.into(),
            draft,
            prerelease,
            assets: Vec::new(),
        }
    }

    #[test]
    fn version_order_does_not_downgrade_a_local_beta() {
        let releases = [release("v0.2.0-beta.2", false, true)];
        assert!(select_release(&releases, &Version::parse("0.2.0-beta.3").unwrap()).is_none());
        let releases = [
            release("v0.2.0-beta.9", false, true),
            release("v0.2.0-beta.10", false, true),
        ];
        assert_eq!(
            select_release(&releases, &Version::parse("0.2.0-beta.3").unwrap())
                .unwrap()
                .1
                .to_string(),
            "0.2.0-beta.10"
        );
    }

    #[test]
    fn stable_users_do_not_receive_betas_drafts_or_build_metadata_updates() {
        let releases = [
            release("v2.0.0-beta.1", false, true),
            release("v2.0.0", true, false),
            release("v1.0.0+new-build", false, false),
            release("../bad", false, false),
        ];
        assert!(select_release(&releases, &Version::parse("1.0.0").unwrap()).is_none());
        let releases = [
            release("v1.0.1", false, false),
            release("v1.0.0", false, false),
        ];
        assert_eq!(
            select_release(&releases, &Version::parse("1.0.0").unwrap())
                .unwrap()
                .1
                .to_string(),
            "1.0.1"
        );
    }

    #[test]
    fn checksum_requires_one_exact_filename_and_valid_digest() {
        let digest = format!("{:x}", Sha256::digest(b"payload"));
        let sums = format!("{digest}  muscli.tar.gz\n{digest} *other.exe\n");
        assert_eq!(checksum(&sums, "muscli.tar.gz").unwrap(), digest);
        assert!(checksum(&sums, "cli.tar.gz").is_err());
        assert!(checksum(&format!("{sums}{sums}"), "muscli.tar.gz").is_err());
        assert!(checksum("bad  muscli.tar.gz", "muscli.tar.gz").is_err());
        assert!(verify_digest(&b"corrupted"[..], &digest).is_err());
        assert!(verify_digest(&b"payload"[..], &digest).is_ok());
    }

    #[test]
    fn asset_origin_and_release_tag_must_match_exactly() {
        let mut release = release("v1.0.1", false, false);
        release.assets.push(Asset {
            name: "SHA256SUMS".into(),
            browser_download_url: "https://evil.example/SHA256SUMS".into(),
            size: 128,
        });
        assert!(asset(&release, "SHA256SUMS").is_err());
        release.assets[0].browser_download_url = format!("{DOWNLOAD_PREFIX}v1.0.1/SHA256SUMS");
        assert!(asset(&release, "SHA256SUMS").is_ok());
        release.tag_name = "v1.0.2".into();
        assert!(asset(&release, "SHA256SUMS").is_err());
    }

    #[test]
    fn disabled_and_recent_checks_do_not_start_a_worker() {
        let root = tempfile::tempdir().unwrap();
        assert!(background(root.path().into(), UpdateMode::Off, false).is_none());
        fs::write(root.path().join("update-checked"), "checked").unwrap();
        assert!(background(root.path().into(), UpdateMode::Notify, false).is_none());
        assert!(read_limited(&b"oversized"[..], 3).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn canonical_executable_accepts_an_installation_directory_alias() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("actual-install");
        fs::create_dir(&directory).unwrap();
        let executable = directory.join("muscli");
        fs::write(&executable, "binary").unwrap();
        let alias = root.path().join("installation-alias");
        std::os::unix::fs::symlink(&directory, &alias).unwrap();
        assert!(inside_installation(
            &executable.canonicalize().unwrap(),
            &alias
        ));
        assert!(!inside_installation(
            &executable.canonicalize().unwrap(),
            root.path().join("unrelated").as_path()
        ));
    }

    #[cfg(windows)]
    #[test]
    fn canonical_windows_executable_matches_the_user_installation() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("Programs").join("muscli");
        fs::create_dir_all(&directory).unwrap();
        let executable = directory.join("muscli.exe");
        fs::write(&executable, "binary").unwrap();
        assert!(inside_installation(
            &executable.canonicalize().unwrap(),
            &directory
        ));
    }

    #[test]
    fn legacy_configs_use_the_builds_default_update_mode() {
        let config: crate::config::Config = toml::from_str("language = 'es'").unwrap();
        assert_eq!(config.update_mode, UpdateMode::default());
        let config: crate::config::Config = toml::from_str("update_mode = 'off'").unwrap();
        assert_eq!(config.update_mode, UpdateMode::Off);
    }

    #[cfg(unix)]
    fn archive(path: &Path, member: &str, data: &[u8], symlink: bool) {
        use std::io::Write;
        let gzip = flate2::write::GzEncoder::new(
            File::create(path).unwrap(),
            flate2::Compression::default(),
        );
        let mut tar = tar::Builder::new(gzip);
        let mut header = tar::Header::new_gnu();
        header.set_mode(0o755);
        if symlink {
            header.set_entry_type(tar::EntryType::Symlink);
            header.set_link_name("/tmp/unrelated").unwrap();
            header.set_size(0);
            header.set_cksum();
            tar.append_data(&mut header, member, &[][..]).unwrap();
        } else {
            header.set_size(data.len() as u64);
            header.set_cksum();
            tar.append_data(&mut header, member, data).unwrap();
        }
        tar.into_inner().unwrap().finish().unwrap().flush().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn verified_linux_update_replaces_only_the_executable_and_preserves_a_backup() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("muscli");
        fs::write(&target, "old executable").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(root.path().join("library.db"), "keep library").unwrap();
        let package = root.path().join("package.tar.gz");
        let data = b"\x7fELFnew executable";
        archive(&package, "muscli-v1.0.1-linux-x86_64/muscli", data, false);
        install_linux(
            &package,
            &target,
            &Version::parse("1.0.1").unwrap(),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(fs::read(&target).unwrap(), data);
        assert_eq!(
            fs::read(root.path().join("muscli.previous")).unwrap(),
            b"old executable"
        );
        assert_eq!(
            fs::read(root.path().join("library.db")).unwrap(),
            b"keep library"
        );
        assert_eq!(
            fs::metadata(target).unwrap().permissions().mode() & 0o777,
            0o755
        );
    }

    #[cfg(unix)]
    #[test]
    fn invalid_archives_leave_the_installed_binary_untouched() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("muscli");
        fs::write(&target, "original").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o755)).unwrap();
        let package = root.path().join("package.tar.gz");
        for (member, data, symlink) in [
            ("muscli-v1.0.1-linux-x86_64/muscli", &b"invalid"[..], false),
            ("muscli-v1.0.1-linux-x86_64/muscli", &b""[..], true),
            ("elsewhere/muscli", &b"\x7fELFfake"[..], false),
        ] {
            archive(&package, member, data, symlink);
            assert!(
                install_linux(
                    &package,
                    &target,
                    &Version::parse("1.0.1").unwrap(),
                    &AtomicBool::new(false)
                )
                .is_err()
            );
            assert_eq!(fs::read(&target).unwrap(), b"original");
            assert!(!root.path().join("muscli.previous").exists());
        }
    }

    #[cfg(unix)]
    #[test]
    fn cancelled_install_preserves_the_current_executable() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("muscli");
        fs::write(&target, "original").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o755)).unwrap();
        let package = root.path().join("package.tar.gz");
        archive(
            &package,
            "muscli-v1.0.1-linux-x86_64/muscli",
            b"\x7fELFtest",
            false,
        );
        assert!(
            install_linux(
                &package,
                &target,
                &Version::parse("1.0.1").unwrap(),
                &AtomicBool::new(true)
            )
            .is_err()
        );
        assert_eq!(fs::read(&target).unwrap(), b"original");
        assert!(!root.path().join("muscli.previous").exists());
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "downloads an official release and installs it only in a temporary directory"]
    fn live_release_archive_checksum_and_installation() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let mut response = agent(8).get(RELEASES_API).call().unwrap();
        let releases: Vec<Release> = serde_json::from_slice(
            &read_limited(response.body_mut().as_reader(), 2 * 1024 * 1024).unwrap(),
        )
        .unwrap();
        let release = releases
            .iter()
            .find(|release| {
                !release.draft
                    && release
                        .assets
                        .iter()
                        .any(|asset| asset.name.ends_with("linux-x86_64.tar.gz"))
            })
            .unwrap();
        let version = Version::parse(release.tag_name.trim_start_matches('v')).unwrap();
        let name = package_name(&version).unwrap();
        let sums = download(asset(release, "SHA256SUMS").unwrap(), root.path(), ".txt").unwrap();
        let digest = checksum(&fs::read_to_string(sums.path()).unwrap(), &name).unwrap();
        let package = download(asset(release, &name).unwrap(), root.path(), ".tar.gz").unwrap();
        verify_digest(File::open(package.path()).unwrap(), &digest).unwrap();
        let target = root.path().join("muscli");
        fs::write(&target, "previous executable").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o755)).unwrap();
        install_linux(package.path(), &target, &version, &AtomicBool::new(false)).unwrap();
        let output = std::process::Command::new(target)
            .arg("--version")
            .output()
            .unwrap();
        assert!(output.status.success());
        assert!(String::from_utf8_lossy(&output.stdout).contains(&version.to_string()));
        assert_eq!(
            fs::read(root.path().join("muscli.previous")).unwrap(),
            b"previous executable"
        );
    }

    #[test]
    #[ignore = "requires the public GitHub releases API"]
    fn live_official_release_check_never_installs() {
        let root = tempfile::tempdir().unwrap();
        assert!(matches!(
            update(root.path(), false).unwrap(),
            UpdateEvent::Current | UpdateEvent::Available(_)
        ));
    }
}
