//! Updates from the project's releases. The mixer checks for a newer version
//! on start and every few hours (unless that's turned off), downloads it, and
//! checks its minisign signature against the public key built in here: only
//! builds signed with the release key are kept or installed (the
//! administrator copy runs elevated, so nothing else may get in). A verified
//! update waits in the `update` folder. "Restart to update" swaps it in now,
//! and otherwise the next start does.
//!
//! Windows won't overwrite a running exe, but it will rename one: the old
//! exe is moved aside to `<name>.old.exe` (deleted once it has quit) and the
//! new one written in its place. The new one is started before this copy
//! quits, waiting for the port (`--replacing`), so a failed start leaves this
//! copy running.
//!
//! The signature travels inside the exe, appended after the program (Windows
//! ignores what follows it), so a release is one file; or, once releases are
//! code-signed for Windows (which anything appended would break), beside it
//! as `virtual-audio-mixer.exe.minisig`. Either way a downloaded update is
//! kept with its signature appended, and installed without it. Its trusted
//! comment names the version, so an old signed build can't be passed off as
//! a newer one.

use crate::VERSION;
use crate::config::UpdateMode;
use anyhow::{Context, anyhow, bail};
use minisign_verify::{PublicKey, Signature};
use serde::Serialize;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

/// The release key's public half (minisign key 7DC13E604FEE19D0); the
/// private half signs releases in CI (`.github/workflows/release.yml`). To
/// replace it, see the release skill: installed copies only trust this one.
const PUBLIC_KEY: &str = "RWTQGe5PYD7BfREzqd5oovZVKaVFRGNVGLm2VEYfTSk31I3cetfTLLfO";
/// Where the latest release is described, in GitHub's format (see
/// [`parse_release`]). When the project moves host, the last release here
/// carries the new address, and the parser for the new host's format.
const LATEST: &str = "https://api.github.com/repos/actuallyatiger/virtual-audio/releases/latest";
const ASSET: &str = "virtual-audio-mixer.exe";
/// The signature, when it isn't in the exe (see `release.yml`).
const SIGNATURE_ASSET: &str = "virtual-audio-mixer.exe.minisig";
/// What CI signs into each signature's trusted comment, before the version.
const COMMENT: &str = "virtual-audio-mixer ";
const MAX_DOWNLOAD: u64 = 64 << 20;
/// Ends a signed exe (see `release.yml`): the signature's length in 8
/// decimal digits, then this tag.
const SIGNED_TAG: &[u8] = b"VAMSIG01";
const TRAILER: usize = 8 + SIGNED_TAG.len();
const FIRST_CHECK: Duration = Duration::from_secs(30);
const CHECK_EVERY: Duration = Duration::from_secs(6 * 60 * 60);

/// What the UI shows about updates.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct UpdateView {
    /// `idle`, `checking`, `up_to_date`, `available` (found, not
    /// downloaded), `downloading`, `ready`, `error` or `unsupported` (this
    /// copy doesn't update itself; `message` says why).
    pub state: &'static str,
    /// The newer version, while there is one (also on a failed download).
    pub version: Option<String>,
    /// Its release notes.
    pub notes_url: Option<String>,
    /// What went wrong, last time (also while an update is still ready).
    pub message: String,
}

impl UpdateView {
    fn new(state: &'static str) -> Self {
        Self {
            state,
            version: None,
            notes_url: None,
            message: String::new(),
        }
    }
}

enum Wake {
    Check,
    Download,
}

pub struct Updater {
    view: Mutex<UpdateView>,
    mode: AtomicU8,
    wake: Mutex<Option<mpsc::Sender<Wake>>>,
    /// Tells the UI something changed.
    notify: Box<dyn Fn() + Send + Sync>,
    /// One install at a time: the tray and the page can both ask.
    installing: Mutex<()>,
}

impl Updater {
    pub fn new(mode: UpdateMode, notify: impl Fn() + Send + Sync + 'static) -> Arc<Self> {
        let view = match allowed() {
            Ok(()) => UpdateView::new("idle"),
            Err(why) => UpdateView {
                message: why.into(),
                ..UpdateView::new("unsupported")
            },
        };
        Arc::new(Self {
            view: Mutex::new(view),
            mode: AtomicU8::new(mode as u8),
            wake: Mutex::new(None),
            notify: Box::new(notify),
            installing: Mutex::new(()),
        })
    }

    pub fn view(&self) -> UpdateView {
        self.view.lock().unwrap().clone()
    }

    /// Shows `view` and tells the UI.
    fn set(&self, view: UpdateView) {
        *self.view.lock().unwrap() = view;
        (self.notify)();
    }

    fn mode(&self) -> UpdateMode {
        match self.mode.load(Ordering::Relaxed) {
            m if m == UpdateMode::Notify as u8 => UpdateMode::Notify,
            m if m == UpdateMode::Off as u8 => UpdateMode::Off,
            _ => UpdateMode::Auto,
        }
    }

    /// Starts checking in the background, unless this copy doesn't update
    /// itself (see [`allowed`]).
    pub fn start(self: &Arc<Self>) {
        if allowed().is_err() {
            return;
        }
        if let Some((version, _)) = staged() {
            self.set(ready(version, None));
        }
        let (tx, rx) = mpsc::channel();
        *self.wake.lock().unwrap() = Some(tx);
        let me = Arc::downgrade(self);
        std::thread::Builder::new()
            .name("updates".into())
            .spawn(move || {
                let mut wait = FIRST_CHECK;
                loop {
                    let wake = match rx.recv_timeout(wait) {
                        Ok(wake) => Some(wake),
                        Err(mpsc::RecvTimeoutError::Timeout) => None,
                        Err(mpsc::RecvTimeoutError::Disconnected) => return,
                    };
                    wait = CHECK_EVERY;
                    let Some(updater) = me.upgrade() else { return };
                    let mode = updater.mode();
                    let download = match wake {
                        // Off: only "Check now" checks.
                        None if mode == UpdateMode::Off => continue,
                        Some(Wake::Download) => true,
                        _ => mode == UpdateMode::Auto,
                    };
                    updater.check(download);
                    // Automatic downloads were turned on during the check.
                    if !download
                        && updater.mode() == UpdateMode::Auto
                        && updater.view().state == "available"
                    {
                        updater.check(true);
                    }
                }
            })
            .expect("spawn update checker");
    }

    pub fn set_mode(&self, mode: UpdateMode) {
        self.mode.store(mode as u8, Ordering::Relaxed);
        if mode == UpdateMode::Auto && self.view().state == "available" {
            self.send(Wake::Download);
        }
    }

    pub fn check_now(&self) {
        self.send(Wake::Check);
    }

    /// Downloads the update found without downloading it (or retries).
    pub fn download_now(&self) {
        self.send(Wake::Download);
    }

    /// Swaps in the downloaded update and starts it; the caller then quits,
    /// and the new version takes over. If anything fails, this copy keeps
    /// running and the UI says why.
    pub fn install_and_launch(&self) -> anyhow::Result<()> {
        let Ok(_one) = self.installing.try_lock() else {
            bail!("the update is already being installed");
        };
        let result = install_staged().and_then(|(version, exe)| {
            launch(&exe, true).with_context(|| {
                format!("version {version} is installed, but didn't start (it will next time)")
            })
        });
        if let Err(e) = &result {
            tracing::error!("installing the update: {e:#}");
            let message = format!("Couldn't install the update: {e:#}");
            let view = match staged() {
                Some((version, _)) => ready(version, self.view().notes_url),
                None => UpdateView::new("error"),
            };
            self.set(UpdateView { message, ..view });
        }
        result
    }

    fn send(&self, wake: Wake) {
        if let Some(tx) = self.wake.lock().unwrap().as_ref() {
            let _ = tx.send(wake);
        }
    }

    fn check(&self, download: bool) {
        let before = self.view();
        // What's already on offer stays on screen while checking.
        let offering = matches!(before.state, "ready" | "available");
        if !offering {
            self.set(UpdateView::new("checking"));
        }
        let mut found = None;
        let view = self.try_check(download, &mut found).unwrap_or_else(|e| {
            tracing::warn!("update check: {e:#}");
            let message = format!("{e:#}");
            // A staged update is still ready, and an offer still stands,
            // whatever went wrong this time.
            if let Some((version, _)) = staged() {
                let notes = before
                    .notes_url
                    .filter(|_| before.version.as_ref() == Some(&version));
                UpdateView {
                    message,
                    ..ready(version, notes)
                }
            } else if before.state == "available" && found.is_none() {
                UpdateView { message, ..before }
            } else {
                UpdateView {
                    message,
                    version: found,
                    ..UpdateView::new("error")
                }
            }
        });
        self.set(view);
    }

    /// `found` is set to the newer version once there is one, for the error.
    fn try_check(&self, download: bool, found: &mut Option<String>) -> anyhow::Result<UpdateView> {
        let agent = agent();
        let json = agent
            .get(LATEST)
            .header("Accept", "application/vnd.github+json")
            .call()
            .and_then(|mut r| r.body_mut().read_to_string())
            .context("couldn't check for updates")?;
        let release = parse_release(&json).context("couldn't check for updates")?;
        if !crate::is_older(VERSION, &release.version) {
            discard_staged();
            return Ok(UpdateView::new("up_to_date"));
        }
        let view = |state| UpdateView {
            version: Some(release.version.clone()),
            notes_url: release.notes_url.clone(),
            ..UpdateView::new(state)
        };
        let staged = staged().map(|(v, _)| v);
        if staged.as_ref() == Some(&release.version) {
            return Ok(view("ready"));
        }
        if !download {
            // An older update that's downloaded can still be installed.
            return Ok(match staged {
                Some(version) => ready(version, None),
                None => view("available"),
            });
        }
        *found = Some(release.version.clone());
        if staged.is_none() {
            self.set(view("downloading"));
        }
        let fetch = |url: &str| -> anyhow::Result<Vec<u8>> {
            Ok(agent
                .get(url)
                .call()?
                .body_mut()
                .with_config()
                .limit(MAX_DOWNLOAD)
                .read_to_vec()?)
        };
        let failed = || format!("couldn't download version {}", release.version);
        let file = match &release.signature_url {
            Some(url) => {
                let exe = fetch(&release.exe_url).with_context(failed)?;
                let signature = String::from_utf8(fetch(url).with_context(failed)?)
                    .map_err(|_| anyhow!("the signature isn't text"))
                    .with_context(failed)?;
                with_signature(&exe, &signature)
            }
            None => fetch(&release.exe_url).with_context(failed)?,
        };
        verify_signed(PUBLIC_KEY, &file, &release.version).with_context(failed)?;
        stage(&file).with_context(failed)?;
        tracing::info!("update {} downloaded and verified", release.version);
        Ok(view("ready"))
    }
}

fn ready(version: String, notes_url: Option<String>) -> UpdateView {
    UpdateView {
        version: Some(version),
        notes_url,
        ..UpdateView::new("ready")
    }
}

/// Whether this copy may update itself. An elevated copy only may when it's
/// the administrator copy in Program Files: one run as administrator from a
/// folder the user can write to (Downloads, say) would otherwise start a file
/// anything running as the user could swap, elevated, without a prompt.
fn allowed() -> Result<(), &'static str> {
    if !cfg!(windows) {
        return Err("Updates are for the Windows app.");
    }
    if !crate::RELEASE {
        return Err("This is a test build, so it doesn't update itself.");
    }
    let exe = std::env::current_exe().map_err(|_| "Can't tell where the mixer is.")?;
    if crate::platform::is_elevated() && !crate::platform::is_admin_copy(&exe) {
        return Err(
            "This copy runs as administrator from a folder that isn't protected, so it \
                    doesn't update itself. Run it normally, or use Start with Windows → as \
                    Administrator.",
        );
    }
    Ok(())
}

fn agent() -> ureq::Agent {
    use ureq::tls::{RootCerts, TlsConfig};
    ureq::Agent::config_builder()
        // Windows' own certificates, which antivirus that inspects HTTPS
        // adds its own to.
        .tls_config(
            TlsConfig::builder()
                .root_certs(RootCerts::PlatformVerifier)
                .build(),
        )
        .timeout_global(Some(Duration::from_secs(120)))
        .user_agent(format!("virtual-audio-mixer/{VERSION}"))
        .build()
        .into()
}

#[derive(Debug, PartialEq)]
struct Release {
    version: String,
    notes_url: Option<String>,
    exe_url: String,
    /// The signature, when it isn't appended to the exe.
    signature_url: Option<String>,
}

/// The parts of GitHub's latest-release JSON an update needs. Nothing here
/// is signed, so the notes link must be a web page (it ends up in the UI).
fn parse_release(json: &str) -> anyhow::Result<Release> {
    let v: serde_json::Value = serde_json::from_str(json).context("reading the release")?;
    let tag = v["tag_name"]
        .as_str()
        .ok_or_else(|| anyhow!("the release has no version"))?;
    let asset = |name: &str| {
        v["assets"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|a| a["name"] == name)
            .and_then(|a| a["browser_download_url"].as_str())
            .map(str::to_owned)
            .ok_or_else(|| anyhow!("the release has no {name} yet"))
    };
    Ok(Release {
        version: tag.trim_start_matches('v').to_owned(),
        notes_url: v["html_url"]
            .as_str()
            .filter(|u| u.starts_with("https://"))
            .map(str::to_owned),
        exe_url: asset(ASSET)?,
        signature_url: asset(SIGNATURE_ASSET).ok(),
    })
}

/// Checks that `exe` is the release key's build of `version`.
fn verify(key: &str, exe: &[u8], signature: &str, version: &str) -> anyhow::Result<()> {
    let key = PublicKey::from_base64(key).map_err(|e| anyhow!("the update key: {e}"))?;
    let signature = Signature::decode(signature).map_err(|e| anyhow!("the signature: {e}"))?;
    key.verify(exe, &signature, false)
        .map_err(|e| anyhow!("it isn't signed by the release key ({e})"))?;
    if signature.trusted_comment() != format!("{COMMENT}{version}") {
        bail!("its signature is for another version");
    }
    Ok(())
}

/// Splits a signed exe into the program and the signature appended to it.
fn split_signed(file: &[u8]) -> Option<(&[u8], &str)> {
    let (rest, trailer) = file.split_at_checked(file.len().checked_sub(TRAILER)?)?;
    let (digits, tag) = trailer.split_at(8);
    if tag != SIGNED_TAG {
        return None;
    }
    let len: usize = std::str::from_utf8(digits).ok()?.parse().ok()?;
    let (exe, signature) = rest.split_at_checked(rest.len().checked_sub(len)?)?;
    Some((exe, std::str::from_utf8(signature).ok()?))
}

/// `exe` with `signature` appended, as the release workflow appends it (and
/// as an update is kept until it's installed).
fn with_signature(exe: &[u8], signature: &str) -> Vec<u8> {
    let mut file = exe.to_vec();
    file.extend(signature.as_bytes());
    file.extend(format!("{:08}", signature.len()).as_bytes());
    file.extend(SIGNED_TAG);
    file
}

/// Checks that `file` is the release key's signed build of `version`.
fn verify_signed(key: &str, file: &[u8], version: &str) -> anyhow::Result<()> {
    let (exe, signature) = split_signed(file).ok_or_else(|| anyhow!("it isn't signed"))?;
    verify(key, exe, signature, version)
}

/// The version a signature says it's for.
fn signed_version(signature: &str) -> Option<String> {
    let signature = Signature::decode(signature).ok()?;
    Some(
        signature
            .trusted_comment()
            .strip_prefix(COMMENT)?
            .to_owned(),
    )
}

/// Where a downloaded update waits. The elevated administrator copy keeps
/// it in its own folder in Program Files, which only administrators can
/// change, rather than in the user's data folder.
fn staging_dir() -> PathBuf {
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf));
    match exe_dir {
        Some(dir) if crate::platform::is_elevated() => dir.join("update"),
        _ => crate::config::data_dir().join("update"),
    }
}

fn stage(file: &[u8]) -> anyhow::Result<()> {
    stage_in(&staging_dir(), file)
}

/// Writes the update under a temporary name, then renames it into place,
/// so a failure leaves any earlier staged update whole, and a link planted
/// at the temporary name is refused rather than followed.
fn stage_in(dir: &Path, file: &[u8]) -> anyhow::Result<()> {
    std::fs::create_dir_all(dir)?;
    let part = dir.join(format!("{ASSET}.part"));
    let _ = std::fs::remove_file(&part);
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&part)?
        .write_all(file)?;
    std::fs::rename(part, dir.join(ASSET))?;
    Ok(())
}

fn discard_staged() {
    let _ = std::fs::remove_dir_all(staging_dir());
}

/// The staged update, verified, if it's newer than this version. Its bytes
/// are what gets installed, so nothing can swap the file after the check
/// (the data folder isn't protected like Program Files).
fn staged() -> Option<(String, Vec<u8>)> {
    staged_in(&staging_dir(), PUBLIC_KEY)
}

fn staged_in(dir: &Path, key: &str) -> Option<(String, Vec<u8>)> {
    let file = std::fs::read(dir.join(ASSET)).ok()?;
    let version = signed_version(split_signed(&file)?.1)?;
    if !crate::is_older(VERSION, &version) {
        return None;
    }
    verify_signed(key, &file, &version).ok()?;
    Some((version, file))
}

/// Puts the staged update in place of the running exe, returning its
/// version and the path to start.
fn install_staged() -> anyhow::Result<(String, PathBuf)> {
    allowed().map_err(|why| anyhow!("{why}"))?;
    let (version, file) = staged().ok_or_else(|| anyhow!("no update is ready"))?;
    // Without the signature, so the exe's own signature for Windows (on a
    // code-signed release) stays valid. (A staged file always has one.)
    let (exe, _) = split_signed(&file).ok_or_else(|| anyhow!("no update is ready"))?;
    let path = std::env::current_exe()?;
    replace(&path, exe).with_context(|| format!("updating {}", path.display()))?;
    crate::platform::updated_in_place(&path, &version);
    discard_staged();
    tracing::info!("updated to {version}");
    Ok((version, path))
}

/// Writes `exe` over the running exe at `path`, which is moved aside first.
fn replace(path: &Path, exe: &[u8]) -> anyhow::Result<()> {
    let old = old_path(path);
    let _ = std::fs::remove_file(&old);
    std::fs::rename(path, &old)?;
    if let Err(e) = std::fs::write(path, exe) {
        let _ = std::fs::remove_file(path);
        let _ = std::fs::rename(&old, path);
        return Err(e.into());
    }
    Ok(())
}

fn old_path(exe: &Path) -> PathBuf {
    let stem = exe
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("virtual-audio-mixer");
    exe.with_file_name(format!("{stem}.old.exe"))
}

/// Deletes the exe an update moved aside, once the copy running it has
/// quit (it may still be on its way out).
pub fn clean_up() {
    if let Some(old) = old_exe() {
        let _ = std::thread::Builder::new()
            .name("old exe".into())
            .spawn(move || remove_old(&old, 60));
    }
}

/// [`clean_up`], waiting for it (up to 5 s), for a copy that's about to
/// quit.
pub fn clean_up_now() {
    if let Some(old) = old_exe() {
        remove_old(&old, 10);
    }
}

fn old_exe() -> Option<PathBuf> {
    let old = old_path(&std::env::current_exe().ok()?);
    old.exists().then_some(old)
}

fn remove_old(old: &Path, tries: u32) {
    for _ in 0..tries {
        if std::fs::remove_file(old).is_ok() || !old.exists() {
            return;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// On start, before anything else: installs a staged update and starts it.
/// `Some(Ok)` means the new version is starting and this copy should exit;
/// `Some(Err)` that it didn't, so this copy carries on (and should log why
/// once logging is up).
pub fn install_on_start(background: bool) -> Option<anyhow::Result<()>> {
    allowed().ok()?;
    staged()?;
    Some(install_staged().and_then(|(version, exe)| {
        launch(&exe, background)
            .with_context(|| format!("version {version} is installed, but didn't start"))
    }))
}

/// Starts the updated exe, which waits for this copy's port to come free.
/// `background` keeps it from opening the mixer in the browser (open pages
/// reconnect by themselves). A freshly written exe can be held for a
/// moment by antivirus scanning it, so a failed start is retried.
fn launch(exe: &Path, background: bool) -> anyhow::Result<()> {
    let mut command = std::process::Command::new(exe);
    command.arg("--replacing");
    if background {
        command.arg("--background");
    }
    let mut tries = 0;
    loop {
        match command.spawn() {
            Ok(_) => return Ok(()),
            Err(e) if tries == 4 => return Err(e.into()),
            Err(_) => {
                tries += 1;
                std::thread::sleep(Duration::from_secs(1));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A throwaway key, and a file it signed for version 9.9.9.
    const TEST_KEY: &str = "RWT5kwqET3UAeD1NPyw/ic6/EaJTQr+BnAJZ/T5FbG1ypCc176+egMZx";
    const FILE: &[u8] = include_bytes!("testdata/update.bin");
    const SIGNATURE: &str = include_str!("testdata/update.bin.minisig");

    /// `FILE` with its signature appended, as CI does.
    fn signed() -> Vec<u8> {
        with_signature(FILE, SIGNATURE)
    }

    #[test]
    fn only_the_release_keys_build_of_that_version_passes() {
        verify(TEST_KEY, FILE, SIGNATURE, "9.9.9").unwrap();
        let mut tampered = FILE.to_vec();
        tampered[0] ^= 1;
        assert!(verify(TEST_KEY, &tampered, SIGNATURE, "9.9.9").is_err());
        // A signed old build can't pass as a newer version.
        assert!(verify(TEST_KEY, FILE, SIGNATURE, "10.0.0").is_err());
        // Another key's signature doesn't count.
        let other = "RWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3";
        assert!(verify(other, FILE, SIGNATURE, "9.9.9").is_err());
        assert_eq!(signed_version(SIGNATURE).as_deref(), Some("9.9.9"));
        let other = SIGNATURE.replace("virtual-audio-mixer 9.9.9", "something else 9.9.9");
        assert_eq!(signed_version(&other), None);
    }

    #[test]
    fn takes_what_the_release_workflow_makes() {
        // Made with release.yml's own commands, for version 9.9.9.
        let file = include_bytes!("testdata/update.signed.bin");
        verify_signed(TEST_KEY, file, "9.9.9").unwrap();
        assert_eq!(split_signed(file).unwrap().0, FILE);
        // A code-signed release's exe and signature, kept together.
        assert_eq!(with_signature(FILE, SIGNATURE), file);
    }

    #[test]
    fn the_signature_rides_at_the_end_of_the_exe() {
        let file = signed();
        assert_eq!(split_signed(&file), Some((FILE, SIGNATURE)));
        verify_signed(TEST_KEY, &file, "9.9.9").unwrap();
        // Unsigned, cut short, or with the program changed.
        assert!(verify_signed(TEST_KEY, FILE, "9.9.9").is_err());
        assert_eq!(split_signed(&file[..file.len() - 1]), None);
        assert_eq!(
            split_signed(&file[10..]).map(|(exe, _)| exe.len()),
            Some(FILE.len() - 10)
        );
        assert!(verify_signed(TEST_KEY, &file[10..], "9.9.9").is_err());
        assert_eq!(split_signed(b"VAMSIG01"), None);
    }

    #[test]
    fn the_built_in_key_is_the_release_key() {
        PublicKey::from_base64(PUBLIC_KEY).unwrap();
        assert_ne!(PUBLIC_KEY, TEST_KEY);
        assert!(verify(PUBLIC_KEY, FILE, SIGNATURE, "9.9.9").is_err());
    }

    /// Talks to GitHub for real: `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn reaches_github() {
        let json = agent()
            .get(LATEST)
            .call()
            .unwrap()
            .body_mut()
            .read_to_string()
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(v["tag_name"].as_str().unwrap().starts_with('v'), "{json}");
    }

    #[test]
    fn reads_githubs_latest_release() {
        let json = r#"{
            "tag_name": "v0.2.0",
            "html_url": "https://github.com/o/r/releases/tag/v0.2.0",
            "assets": [
                {"name": "virtual-audio-mixer.exe", "browser_download_url": "https://x/exe"}
            ]
        }"#;
        assert_eq!(
            parse_release(json).unwrap(),
            Release {
                version: "0.2.0".into(),
                notes_url: Some("https://github.com/o/r/releases/tag/v0.2.0".into()),
                exe_url: "https://x/exe".into(),
                signature_url: None,
            }
        );
        // A code-signed release has its signature beside the exe.
        let beside = json.replace(
            "]",
            r#", {"name": "virtual-audio-mixer.exe.minisig", "browser_download_url": "https://x/sig"}]"#,
        );
        assert_eq!(
            parse_release(&beside).unwrap().signature_url.as_deref(),
            Some("https://x/sig")
        );
        // A release still building has no files yet.
        let bare = r#"{"tag_name": "v0.2.0", "assets": []}"#;
        assert!(parse_release(bare).is_err());
        // The notes link isn't signed: only a web page gets to the UI.
        let sneaky = json.replace(
            "https://github.com/o/r/releases/tag/v0.2.0",
            "javascript:alert(1)",
        );
        assert_eq!(parse_release(&sneaky).unwrap().notes_url, None);
    }

    #[test]
    fn a_staged_update_is_checked_before_it_is_used() {
        let dir = std::env::temp_dir().join(format!("vam-update-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(staged_in(&dir, TEST_KEY), None, "nothing staged");
        std::fs::write(dir.join(ASSET), signed()).unwrap();
        assert_eq!(
            staged_in(&dir, TEST_KEY),
            Some(("9.9.9".to_owned(), signed()))
        );
        // Swapped for something else after it was downloaded.
        let mut swapped = signed();
        swapped[0] ^= 1;
        std::fs::write(dir.join(ASSET), swapped).unwrap();
        assert_eq!(staged_in(&dir, TEST_KEY), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn replacing_moves_the_old_exe_aside() {
        let dir = std::env::temp_dir().join(format!("vam-replace-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("virtual-audio-mixer (1).exe");
        std::fs::write(&exe, b"old").unwrap();
        replace(&exe, b"new").unwrap();
        assert_eq!(std::fs::read(&exe).unwrap(), b"new");
        let old = dir.join("virtual-audio-mixer (1).old.exe");
        assert_eq!(std::fs::read(&old).unwrap(), b"old");
        // Again, over the leftover from last time.
        replace(&exe, b"newer").unwrap();
        assert_eq!(std::fs::read(&old).unwrap(), b"new");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn staging_replaces_an_update_whole_or_not_at_all() {
        let dir = std::env::temp_dir().join(format!("vam-stage-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        stage_in(&dir, &signed()).unwrap();
        let version = |dir: &Path| staged_in(dir, TEST_KEY).map(|(v, _)| v);
        assert_eq!(version(&dir).as_deref(), Some("9.9.9"));
        // Leftovers from an interrupted download don't get in the way.
        std::fs::write(dir.join(format!("{ASSET}.part")), b"half").unwrap();
        stage_in(&dir, &signed()).unwrap();
        assert!(version(&dir).is_some());
        assert!(!dir.join(format!("{ASSET}.part")).exists());
        // A failed write leaves the earlier update as it was.
        std::fs::create_dir(dir.join(format!("{ASSET}.part"))).unwrap();
        assert!(stage_in(&dir, b"newer").is_err());
        assert_eq!(
            staged_in(&dir, TEST_KEY).map(|(_, file)| file),
            Some(signed())
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
