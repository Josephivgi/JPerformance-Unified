//! Updates from GitHub Releases. The app checks for a newer release, and the
//! user decides whether to install it. Installing downloads the new exe,
//! checks its SHA-256 against the one GitHub reports, swaps it in place of the
//! running exe and restarts the app and the background recorder.

use crate::local::db;
use crate::{cmd, collector, paths};
use serde_json::{Value, json};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const REPO: &str = "Josephivgi/JPerformance-Unified";
pub const CURRENT: &str = env!("CARGO_PKG_VERSION");
const ASSET: &str = "JPerformanceUnified.exe";

#[derive(Clone)]
pub struct Release {
    pub version: String,
    pub notes: String,
    pub page: String,
    asset_url: String,
    size: u64,
    sha256: String,
}

#[derive(Clone)]
pub enum State {
    Idle,
    Checking,
    UpToDate,
    Available(Release),
    /// Bytes downloaded so far and the total.
    Downloading(u64, u64),
    /// The new version is in place and has been started; this window should close.
    Restarting,
    Failed(String),
}

#[derive(Deserialize)]
struct GhRelease {
    tag_name: String,
    #[serde(default)]
    body: Option<String>,
    html_url: String,
    assets: Vec<GhAsset>,
}

#[derive(Deserialize)]
struct GhAsset {
    name: String,
    browser_download_url: String,
    size: u64,
    #[serde(default)]
    digest: Option<String>,
}

/// "v1.2.3" or "1.2.3" as comparable numbers.
fn parse_version(v: &str) -> Option<Vec<u64>> {
    v.trim().trim_start_matches(['v', 'V']).split('.').map(|p| p.parse().ok()).collect()
}

pub fn is_newer(candidate: &str, current: &str) -> bool {
    match (parse_version(candidate), parse_version(current)) {
        (Some(a), Some(b)) => a > b,
        _ => false,
    }
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(15)))
        .timeout_recv_response(Some(Duration::from_secs(30)))
        // Longest wait for the next bytes of a download. A stalled connection then fails instead of hanging,
        // and a slow but moving download is not cut off.
        .timeout_per_read(Some(Duration::from_secs(30)))
        .user_agent(concat!("JPerformanceUnified/", env!("CARGO_PKG_VERSION")))
        .build()
        .into()
}

/// What the last check found, and the ETag GitHub sent with it. The next check
/// asks with that ETag, so an unchanged release comes back as 304 Not Modified,
/// which saves sending the release again. Without a GitHub sign-in a 304 still
/// counts against the 60 requests an hour, so the window only checks every 2 hours.
#[derive(Default)]
pub struct Cache {
    etag: Option<String>,
    found: Option<Option<Release>>,
    /// True when the last check was answered with 304 Not Modified.
    last_was_304: bool,
}

/// Developer option: whether update checks use the GitHub CLI's sign-in.
pub fn use_gh() -> bool {
    db::open(&paths::local_db()).is_ok_and(|c| db::setting_i64(&c, db::KEY_USE_GH, 0) != 0)
}

/// The GitHub CLI's token, when gh is installed and signed in. It is only sent to api.github.com, never logged or shown.
fn gh_token() -> Option<String> {
    let o = cmd::run("gh", &["auth", "token", "--hostname", "github.com"], Duration::from_secs(10));
    let t = o.stdout.trim();
    (o.ok && !t.is_empty() && !t.contains(char::is_whitespace)).then(|| t.to_string())
}

/// Whether the GitHub CLI is installed and signed in, for Settings.
pub fn gh_status() -> Value {
    let v = cmd::run("gh", &["--version"], Duration::from_secs(10));
    if !v.ok {
        return json!({ "installed": false });
    }
    let version = v.stdout.lines().next().unwrap_or("").trim().to_string();
    if gh_token().is_none() {
        return json!({ "installed": true, "version": version, "signed_in": false });
    }
    let u = cmd::run("gh", &["api", "user", "--jq", ".login"], Duration::from_secs(15));
    json!({ "installed": true, "version": version, "signed_in": true, "user": u.ok.then(|| u.stdout.trim().to_string()).filter(|s| !s.is_empty()) })
}

/// The latest published release, or None if there is none yet.
fn latest(cache: &Mutex<Cache>) -> Result<Option<Release>, String> {
    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let (etag, found) = {
        let c = cache.lock().unwrap();
        (c.etag.clone(), c.found.clone())
    };
    let mut req = agent().get(&url).header("Accept", "application/vnd.github+json");
    // With the developer option on, the check counts against the user's GitHub account (5,000 an hour)
    // instead of the 60 an hour shared by everyone on their connection. Without gh it checks as before.
    let token = if use_gh() { gh_token() } else { None };
    if use_gh() && token.is_none() {
        crate::log("update check: the GitHub CLI is not installed or not signed in, so it checked without it");
    }
    if let Some(t) = &token {
        req = req.header("Authorization", format!("Bearer {t}"));
    }
    if let (Some(tag), Some(_)) = (&etag, &found) {
        req = req.header("If-None-Match", tag.clone());
    }
    let mut resp = match req.call() {
        Ok(r) => r,
        Err(ureq::Error::StatusCode(404)) => return Ok(None),
        Err(ureq::Error::StatusCode(401)) if token.is_some() => {
            return Err("GitHub did not accept the GitHub CLI's sign-in. Run gh auth login again, or turn off Use GitHub CLI in Settings > Developer.".into());
        }
        Err(ureq::Error::StatusCode(403 | 429)) => {
            return Err("GitHub is limiting update checks for now. Checks will try again on their own.".into());
        }
        Err(e) => return Err(format!("Could not reach GitHub: {e}")),
    };
    if resp.status() == ureq::http::StatusCode::NOT_MODIFIED {
        cache.lock().unwrap().last_was_304 = true;
        return Ok(found.flatten());
    }
    let new_etag = resp.headers().get("etag").and_then(|v| v.to_str().ok()).map(str::to_string);
    let rel: GhRelease = resp.body_mut().read_json().map_err(|e| format!("Unexpected reply from GitHub: {e}"))?;
    let asset = rel
        .assets
        .into_iter()
        .find(|a| a.name.eq_ignore_ascii_case(ASSET))
        .ok_or_else(|| format!("Release {} has no {ASSET} to download.", rel.tag_name))?;
    let sha256 = asset
        .digest
        .as_deref()
        .and_then(|d| d.strip_prefix("sha256:"))
        .map(str::to_ascii_lowercase)
        .ok_or_else(|| format!("Release {} has no checksum, so it was not trusted.", rel.tag_name))?;
    let found = Release {
        version: rel.tag_name.trim_start_matches(['v', 'V']).to_string(),
        notes: rel.body.unwrap_or_default().trim().to_string(),
        page: rel.html_url,
        asset_url: asset.browser_download_url,
        size: asset.size,
        sha256,
    };
    *cache.lock().unwrap() = Cache { etag: new_etag, found: Some(Some(found.clone())), last_was_304: false };
    Ok(Some(found))
}

/// The window asks for checks (at launch, every 2 hours, and by the Refresh button).
/// Each job has a number. A job only writes the state while it is still the current one, so a check
/// that finishes late cannot overwrite a download that started after it.
struct Slot {
    state: State,
    job: u64,
    /// True for the timer's check. It keeps a known update on screen while it runs.
    background: bool,
    /// The newer release the last check found, kept while a later check runs or fails.
    found: Option<Release>,
}

pub struct Updater {
    slot: Arc<Mutex<Slot>>,
    cache: Arc<Mutex<Cache>>,
}

impl Default for Updater {
    fn default() -> Self {
        Self {
            slot: Arc::new(Mutex::new(Slot { state: State::Idle, job: 0, background: false, found: None })),
            cache: Arc::new(Mutex::new(Cache::default())),
        }
    }
}

/// Starts a job if `allowed` says the current state permits it. Returns the job number.
fn start(slot: &Mutex<Slot>, next: State, background: bool, allowed: impl Fn(&State) -> bool) -> Option<u64> {
    let mut s = slot.lock().unwrap();
    if !allowed(&s.state) {
        return None;
    }
    s.job += 1;
    s.state = next;
    s.background = background;
    Some(s.job)
}

/// Writes the state if `job` is still the current job.
fn finish(slot: &Mutex<Slot>, job: u64, next: State) {
    let mut s = slot.lock().unwrap();
    if s.job == job {
        s.state = next;
    }
}

/// Stores the result of a check. A failed check keeps an update found earlier.
fn finish_check(slot: &Mutex<Slot>, job: u64, found: Result<Option<Release>, String>) {
    let mut s = slot.lock().unwrap();
    if s.job != job {
        return;
    }
    match found {
        Ok(Some(r)) if is_newer(&r.version, CURRENT) => match blocked_release(&r.version) {
            Some(why) => {
                s.found = None;
                s.state = State::Failed(why);
            }
            None => {
                s.found = Some(r.clone());
                s.state = State::Available(r);
            }
        },
        Ok(_) => {
            s.found = None;
            s.state = State::UpToDate;
        }
        Err(e) => {
            crate::log(&format!("update check failed: {e}"));
            s.state = match s.found.clone() {
                Some(r) => State::Available(r),
                None => State::Failed(e),
            };
        }
    }
}

fn not_busy(s: &State) -> bool {
    !matches!(s, State::Checking | State::Downloading(..) | State::Restarting)
}

impl Updater {
    pub fn state(&self) -> State {
        self.slot.lock().unwrap().state.clone()
    }

    /// What the window shows. While a check runs, an update found earlier stays on screen,
    /// so the Install button does not disappear on every check.
    pub fn view(&self) -> State {
        let s = self.slot.lock().unwrap();
        match (&s.state, &s.found) {
            (State::Checking, Some(r)) if s.background => State::Available(r.clone()),
            _ => s.state.clone(),
        }
    }

    pub fn check(&self) {
        let Some(job) = start(&self.slot, State::Checking, true, not_busy) else { return };
        let slot = self.slot.clone();
        let cache = self.cache.clone();
        std::thread::spawn(move || finish_check(&slot, job, latest(&cache)));
    }

    /// The Refresh button: checks now, and if a newer release is there, installs it and restarts.
    pub fn refresh(&self) {
        let Some(job) = start(&self.slot, State::Checking, false, |s| !matches!(s, State::Downloading(..) | State::Restarting)) else { return };
        let slot = self.slot.clone();
        let cache = self.cache.clone();
        std::thread::spawn(move || match latest(&cache) {
            Ok(Some(r)) if is_newer(&r.version, CURRENT) => {
                // Checking becomes downloading in one step, so no other job can start in between.
                {
                    let mut s = slot.lock().unwrap();
                    if s.job != job {
                        return;
                    }
                    if let Some(why) = blocked_release(&r.version) {
                        s.found = None;
                        s.state = State::Failed(why);
                        return;
                    }
                    s.found = Some(r.clone());
                    s.state = State::Downloading(0, r.size);
                }
                install_job(slot, job, r);
            }
            other => finish_check(&slot, job, other),
        });
    }

    /// Downloads, verifies and installs the release found by the last check, then starts the new version.
    /// It also works while a check is still running, using the release that check found earlier.
    pub fn install(&self) {
        let rel = {
            let s = self.slot.lock().unwrap();
            match (&s.state, &s.found) {
                (State::Available(r), _) => r.clone(),
                (State::Checking, Some(r)) => r.clone(),
                _ => return,
            }
        };
        let Some(job) = start(&self.slot, State::Downloading(0, rel.size), false, |s| !matches!(s, State::Downloading(..) | State::Restarting)) else {
            return;
        };
        let slot = self.slot.clone();
        std::thread::spawn(move || install_job(slot, job, rel));
    }
}


/// Downloads and verifies the release, swaps the exe and starts the new version.
/// The job must still be the current one when the download ends, or the exe is not touched.
fn install_job(slot: Arc<Mutex<Slot>>, job: u64, rel: Release) {
    let progress = |done, total| finish(&slot, job, State::Downloading(done, total));
    let file = match download(&rel, progress) {
        Ok(file) => file,
        Err(e) => {
            crate::log(&format!("update to {} failed: {e}", rel.version));
            finish(&slot, job, State::Failed(e));
            return;
        }
    };
    if slot.lock().unwrap().job != job {
        let _ = std::fs::remove_file(&file);
        return;
    }
    let s = match swap_and_restart(&file, &rel.version) {
        Ok(()) => {
            crate::log(&format!("updated to {}", rel.version));
            State::Restarting
        }
        Err(e) => {
            let _ = std::fs::remove_file(installing_marker());
            crate::log(&format!("update to {} failed: {e}", rel.version));
            State::Failed(e)
        }
    };
    finish(&slot, job, s);
}

/// `--check-update` prints whether a newer release exists; `--update` also installs it.
pub fn cli(install: bool) -> Result<(), String> {
    println!("JPerformance Unified {CURRENT}");
    let rel = match latest(&Mutex::new(Cache::default()))? {
        Some(r) if is_newer(&r.version, CURRENT) => r,
        Some(r) => {
            println!("Up to date (latest release is {}).", r.version);
            return Ok(());
        }
        None => {
            println!("No releases published yet.");
            return Ok(());
        }
    };
    println!("Version {} is available: {}", rel.version, rel.page);
    if !install {
        return Ok(());
    }
    if app_open() {
        return Err("JPerformance Unified is open. Close it, then run the update again.".into());
    }
    let file = download(&rel, |_, _| {})?;
    println!("Downloaded and verified {} bytes, sha256 {}", rel.size, rel.sha256);
    if let Err(e) = swap_and_restart(&file, &rel.version) {
        let _ = std::fs::remove_file(installing_marker());
        return Err(e);
    }
    println!("Installed {}. JPerformance Unified is restarting.", rel.version);
    Ok(())
}

/// True when the app window (not the background recorder) is running.
fn app_open() -> bool {
    let me = std::process::id();
    let mut sys = sysinfo::System::new();
    sys.refresh_processes_specifics(
        sysinfo::ProcessesToUpdate::All,
        true,
        sysinfo::ProcessRefreshKind::nothing().with_cmd(sysinfo::UpdateKind::Always),
    );
    sys.processes().values().any(|p| {
        p.pid().as_u32() != me
            && p.name().to_string_lossy().eq_ignore_ascii_case(ASSET)
            && !p.cmd().iter().any(|a| a == "--collector")
    })
}

/// Where the version being installed is written, so a release whose exe reports another version is not offered again.
fn installing_marker() -> PathBuf {
    paths::data_dir().join("update-installing.txt")
}

/// The release that was installed but reports a different version, if any.
fn blocked_release(version: &str) -> Option<String> {
    let marker = std::fs::read_to_string(installing_marker()).ok()?;
    (marker.trim() == version && version != CURRENT).then(|| {
        format!("Version {version} was installed, but the app still reports {CURRENT}. It will not be offered again.")
    })
}

fn update_dir() -> PathBuf {
    let dir = paths::data_dir().join("update");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

fn download(rel: &Release, progress: impl Fn(u64, u64)) -> Result<PathBuf, String> {
    let path = update_dir().join(format!("JPerformanceUnified-{}.exe", rel.version));
    match fetch(rel, &path, progress) {
        Ok(()) => Ok(path),
        Err(e) => {
            // A failed or unverified download is never kept around.
            let _ = std::fs::remove_file(&path);
            Err(e)
        }
    }
}

fn fetch(rel: &Release, path: &Path, progress: impl Fn(u64, u64)) -> Result<(), String> {
    let fail = |e: &dyn std::fmt::Display| format!("Download failed: {e}");
    let mut resp = agent().get(&rel.asset_url).call().map_err(|e| fail(&e))?;
    let mut file = std::fs::File::create(path).map_err(|e| fail(&e))?;
    let mut reader = resp.body_mut().with_config().limit(rel.size.max(1) + 1).reader();
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    let mut done = 0u64;
    let mut last_report = Instant::now();
    loop {
        let n = reader.read(&mut buf).map_err(|e| fail(&e))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        file.write_all(&buf[..n]).map_err(|e| fail(&e))?;
        done += n as u64;
        if last_report.elapsed() >= Duration::from_millis(100) {
            last_report = Instant::now();
            progress(done, rel.size);
        }
    }
    file.flush().map_err(|e| fail(&e))?;
    drop(file);
    let got: String = hasher.finalize().iter().map(|b| format!("{b:02x}")).collect();
    if done != rel.size || got != rel.sha256 {
        return Err("The download did not match the release checksum, so it was not installed.".into());
    }
    progress(done, rel.size);
    Ok(())
}

fn old_exe(exe: &Path) -> PathBuf {
    exe.with_file_name("JPerformanceUnified.old.exe")
}

/// Stops the background recorder so it restarts from the new exe.
fn stop_collector() {
    let Ok(conn) = db::open(&paths::local_db()) else { return };
    let _ = db::set_setting(&conn, db::KEY_TRACKING, "0");
    // The recorder checks this setting every 5 seconds.
    let deadline = Instant::now() + Duration::from_secs(12);
    while collector::is_running() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(250));
    }
    if collector::is_running() {
        if let Ok(Some(pid)) = db::collector_pid(&conn) {
            let mut sys = sysinfo::System::new();
            let pid = sysinfo::Pid::from_u32(pid);
            sys.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[pid]), true);
            if let Some(p) = sys.process(pid) {
                p.kill();
            }
        }
    }
    let _ = db::set_setting(&conn, db::KEY_TRACKING, "1");
}

fn swap_and_restart(new_exe: &Path, version: &str) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let old = old_exe(&exe);
    stop_collector();
    let _ = std::fs::write(installing_marker(), version);
    let _ = std::fs::remove_file(&old);
    // Windows lets a running exe be renamed but not overwritten.
    std::fs::rename(&exe, &old).map_err(|e| {
        let _ = collector::spawn_detached();
        format!("Could not replace {}: {e}. Try moving JPerformance Unified to a folder you can write to.", exe.display())
    })?;
    if let Err(e) = std::fs::copy(new_exe, &exe) {
        let _ = std::fs::rename(&old, &exe);
        let _ = collector::spawn_detached();
        return Err(format!("Could not install the new version: {e}"));
    }
    let _ = std::fs::remove_file(new_exe);
    let started = std::process::Command::new(&exe)
        .arg("--after-update")
        .arg(std::process::id().to_string())
        .creation_flags(DETACHED_PROCESS)
        .spawn();
    if let Err(e) = started {
        // Put the old exe back under its own name, so the next update can replace it.
        let _ = std::fs::remove_file(&exe);
        let _ = std::fs::rename(&old, &exe);
        let _ = collector::spawn_detached();
        return Err(format!("Could not start the new version: {e}. Open JPerformance Unified again."));
    }
    Ok(())
}

/// The new version was started by the old one; wait (up to 15 s) for the old
/// process to exit so the single-instance check does not hand off to it.
pub fn wait_for_old(pid: u32) {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject};
    unsafe {
        let h = OpenProcess(PROCESS_SYNCHRONIZE, 0, pid);
        if !h.is_null() {
            WaitForSingleObject(h, 15_000);
            CloseHandle(h);
        }
    }
}

/// Removes the exe left behind by the last update. It may still be running
/// for a moment while the old window closes, so this retries briefly.
pub fn cleanup_after_update() {
    if std::fs::read_to_string(installing_marker()).is_ok_and(|v| v.trim() == CURRENT) {
        let _ = std::fs::remove_file(installing_marker());
    }
    let Ok(exe) = std::env::current_exe() else { return };
    let old = old_exe(&exe);
    if !old.exists() {
        return;
    }
    std::thread::spawn(move || {
        for _ in 0..30 {
            if std::fs::remove_file(&old).is_ok() || !old.exists() {
                let _ = std::fs::remove_dir_all(update_dir());
                return;
            }
            std::thread::sleep(Duration::from_secs(1));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newer_versions_compare_by_number() {
        assert!(is_newer("1.1.0", "1.0.1"));
        assert!(is_newer("v1.10.0", "1.9.9"));
        assert!(!is_newer("1.1.0", "1.1.0"));
        assert!(!is_newer("garbage", "1.1.0"));
    }

    /// Talks to GitHub, so it is skipped by default:
    /// `cargo test --release -- --ignored etag`
    #[test]
    #[ignore]
    fn etag_check_gets_304_from_github() {
        let cache = Mutex::new(Cache::default());
        let first = latest(&cache).expect("first check");
        assert!(cache.lock().unwrap().etag.is_some(), "GitHub sent no ETag");
        assert!(!cache.lock().unwrap().last_was_304, "first check cannot be 304");
        let second = latest(&cache).expect("second check");
        assert!(cache.lock().unwrap().last_was_304, "second check was not 304");
        assert_eq!(first.map(|r| r.version), second.map(|r| r.version));
    }
}
