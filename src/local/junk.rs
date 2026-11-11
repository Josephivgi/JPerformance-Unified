//! Clean-up: places that only hold files programs download or build again, how big each one
//! is, whether its program is open right now, and clearing the ones the user picks. Also what
//! sits in the Downloads folder and the big files Windows keeps for itself.
//!
//! Everything here is measured on its own, so it works without scanning a drive.

use super::storage::{CLOUD, env_path, file_type, norm, protected_reason};
use serde_json::{Value, json};
use std::collections::HashSet;
use std::fs::Metadata;
use std::os::windows::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const ATTR_READONLY: u32 = 0x1;
const ATTR_HIDDEN: u32 = 0x2;
const ATTR_SYSTEM: u32 = 0x4;
const ATTR_DIRECTORY: u32 = 0x10;

/// The order the groups are listed in.
pub const GROUPS: [&str; 5] = ["Windows", "Browsers", "Apps", "Developer tools", "Games"];

fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

fn mtime(md: &Metadata) -> i64 {
    md.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// A junction or symbolic link. It is removed as a link; what it points at is never touched.
fn is_link(md: &Metadata) -> bool {
    md.file_type().is_symlink()
}

/// One folder a rule clears: everything in it, or only the entries whose names start a certain way.
#[derive(Clone)]
struct Target {
    dir: PathBuf,
    prefixes: &'static [&'static str],
}

#[derive(Clone)]
struct Rule {
    id: String,
    group: &'static str,
    label: String,
    explain: String,
    targets: Vec<Target>,
    /// Only entries where nothing changed for this many seconds are cleared (temporary files a
    /// running program may still use).
    older_than: Option<i64>,
    /// Cheap to get back, so "Select recommended" picks it.
    rec: bool,
    /// Files a program holds open for as long as it runs.
    locks: Vec<PathBuf>,
    /// Process names (lower case, with .exe) that mean the program is open.
    procs: Vec<String>,
    /// The program to name when it is open.
    app: String,
    // Measured:
    measured: bool,
    running: bool,
    size: u64,
    files: u64,
    sizes: Vec<u64>,
}

fn rule(id: &str, group: &'static str, label: &str, explain: &str, rec: bool, dirs: Vec<PathBuf>) -> Option<Rule> {
    let targets: Vec<Target> = dirs.into_iter().filter(|p| p.is_dir()).map(|dir| Target { dir, prefixes: &[] }).collect();
    if targets.is_empty() {
        return None;
    }
    Some(Rule {
        id: id.into(),
        group,
        label: label.into(),
        explain: explain.into(),
        targets,
        older_than: None,
        rec,
        locks: Vec::new(),
        procs: Vec::new(),
        app: label.into(),
        measured: false,
        running: false,
        size: 0,
        files: 0,
        sizes: Vec::new(),
    })
}

/// Cache folders every Chromium profile has. Sign-ins, history and settings live elsewhere.
const CHROMIUM_CACHES: [&str; 11] = [
    "Cache",
    "Code Cache",
    "GPUCache",
    "DawnCache",
    "DawnGraphiteCache",
    "DawnWebGPUCache",
    r"Service Worker\CacheStorage",
    r"Service Worker\ScriptCache",
    // VS Code and the editors built on it.
    "CachedData",
    "CachedExtensionVSIXs",
    "CachedProfilesData",
];
/// Shader caches a Chromium program keeps next to its profiles.
const CHROMIUM_SHARED: [&str; 3] = ["ShaderCache", "GrShaderCache", "GraphiteDawnCache"];

/// The cache folders of a Chromium browser or an Electron app, given where it keeps its profiles.
fn chromium_dirs(user_data: &Path) -> Vec<PathBuf> {
    let mut profiles = vec![user_data.to_path_buf()];
    if let Ok(rd) = std::fs::read_dir(user_data) {
        profiles.extend(rd.flatten().map(|e| e.path()).filter(|p| p.join("Preferences").is_file()));
    }
    // Electron apps keep extra sessions under Partitions.
    if let Ok(rd) = std::fs::read_dir(user_data.join("Partitions")) {
        profiles.extend(rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()));
    }
    let mut v = Vec::new();
    for p in &profiles {
        for c in CHROMIUM_CACHES {
            v.push(p.join(c));
        }
    }
    for c in CHROMIUM_SHARED {
        v.push(user_data.join(c));
    }
    v
}

/// A nicer name for an app's folder in AppData.
fn app_label(folder: &str) -> String {
    match folder.to_ascii_lowercase().as_str() {
        "code" => "VS Code".into(),
        "code - insiders" => "VS Code Insiders".into(),
        "discord" => "Discord".into(),
        "obsidian" => "Obsidian".into(),
        "t3code" => "T3 Code".into(),
        "lunarclient" => "Lunar Client".into(),
        _ => folder.to_string(),
    }
}

/// Every clean-up rule that has something on this PC.
fn rules() -> Vec<Rule> {
    let local = env_path("LOCALAPPDATA");
    let roaming = env_path("APPDATA");
    let home = env_path("USERPROFILE");
    let temp = env_path("TEMP");
    let mut out: Vec<Rule> = Vec::new();

    // ----- Windows -----
    if let Some(t) = &temp {
        if let Some(mut r) = rule(
            "temp",
            "Windows",
            "Temporary files",
            "Files programs left in your Temp folder. Only items where nothing changed for a day are deleted, so programs running now keep theirs.",
            true,
            vec![t.clone()],
        ) {
            r.older_than = Some(86_400);
            out.push(r);
        }
    }
    if let Some(l) = &local {
        if let Some(mut r) = rule(
            "thumbs",
            "Windows",
            "Thumbnail and icon caches",
            "Small previews of pictures, videos and icons. File Explorer makes them again as you browse. It keeps the ones in use until you sign out.",
            true,
            vec![l.join(r"Microsoft\Windows\Explorer")],
        ) {
            r.targets[0].prefixes = &["thumbcache_", "iconcache_"];
            out.push(r);
        }
        out.extend(rule(
            "dumps",
            "Windows",
            "Crash dumps and error reports",
            "Memory snapshots saved when programs crashed, and reports Windows already sent.",
            true,
            vec![l.join("CrashDumps"), l.join(r"Microsoft\Windows\WER\ReportArchive"), l.join(r"Microsoft\Windows\WER\ReportQueue")],
        ));
        let mut shaders = vec![l.join("D3DSCache"), l.join(r"NVIDIA\DXCache"), l.join(r"NVIDIA\GLCache"), l.join(r"AMD\DxCache"), l.join(r"AMD\DxcCache"), l.join(r"AMD\VkCache"), l.join(r"Intel\ShaderCache")];
        if let Some(h) = &home {
            shaders.push(h.join(r"AppData\LocalLow\NVIDIA\PerDriverVersion\DXCache"));
            shaders.push(h.join(r"AppData\LocalLow\Intel\ShaderCache"));
        }
        out.extend(rule(
            "shaders",
            "Windows",
            "Graphics shader caches",
            "Compiled shaders that games and the graphics driver build again when needed. The first start of a game may stutter a little.",
            true,
            shaders,
        ));
        out.extend(rule(
            "inet",
            "Windows",
            "Windows web cache",
            "Web files kept by Windows and by apps that show web pages. They are downloaded again when needed.",
            true,
            vec![l.join(r"Microsoft\Windows\INetCache\IE")],
        ));
    }

    // ----- Browsers -----
    if let Some(l) = &local {
        for (id, name, base, exe) in [
            ("chrome", "Google Chrome", r"Google\Chrome\User Data", "chrome.exe"),
            ("edge", "Microsoft Edge", r"Microsoft\Edge\User Data", "msedge.exe"),
            ("brave", "Brave", r"BraveSoftware\Brave-Browser\User Data", "brave.exe"),
            ("vivaldi", "Vivaldi", r"Vivaldi\User Data", "vivaldi.exe"),
            ("chromium", "Chromium", r"Chromium\User Data", "chromium.exe"),
            ("opera", "Opera", r"Opera Software\Opera Stable", "opera.exe"),
            ("operagx", "Opera GX", r"Opera Software\Opera GX Stable", "opera.exe"),
        ] {
            let ud = l.join(base);
            if let Some(mut r) = rule(
                id,
                "Browsers",
                name,
                &format!("Copies of web pages, pictures and scripts that {name} downloads again when needed. You stay signed in, and history, passwords and bookmarks are kept."),
                true,
                chromium_dirs(&ud),
            ) {
                r.locks = vec![ud.join("lockfile")];
                if let Some(ro) = &roaming {
                    r.locks.push(ro.join(base).join("lockfile"));
                }
                // Edge always has helper processes, so only its lock file says the browser is open.
                if id != "edge" && !id.starts_with("opera") {
                    r.procs = vec![exe.into()];
                }
                out.push(r);
            }
        }
        let mut dirs = Vec::new();
        if let Ok(rd) = std::fs::read_dir(l.join(r"Mozilla\Firefox\Profiles")) {
            for prof in rd.flatten() {
                dirs.push(prof.path().join("cache2"));
                dirs.push(prof.path().join("startupCache"));
            }
        }
        if let Some(mut r) = rule(
            "firefox",
            "Browsers",
            "Firefox",
            "Copies of web pages, pictures and scripts that Firefox downloads again when needed. You stay signed in, and history, passwords and bookmarks are kept.",
            true,
            dirs,
        ) {
            r.procs = vec!["firefox.exe".into()];
            out.push(r);
        }
    }

    // ----- Apps -----
    // Electron apps (VS Code, Discord, Slack and many more) keep Chromium caches in their AppData folder.
    if let Some(ro) = &roaming {
        let mut folders: Vec<PathBuf> = std::fs::read_dir(ro).into_iter().flatten().flatten().map(|e| e.path()).collect();
        folders.push(ro.join(r"Microsoft\Teams"));
        for dir in folders {
            if !dir.join("Code Cache").is_dir() && !dir.join("GPUCache").is_dir() {
                continue;
            }
            let Some(folder) = dir.file_name().map(|n| n.to_string_lossy().to_string()) else { continue };
            let name = app_label(&folder);
            if let Some(mut r) = rule(
                &format!("app:{}", folder.to_lowercase()),
                "Apps",
                &name,
                "Copies of pages and compiled code this app makes again when needed. Its settings and sign-ins are kept.",
                true,
                chromium_dirs(&dir),
            ) {
                r.locks = vec![dir.join("lockfile")];
                // claude.exe is also the Claude Code command line tool, so its name says nothing here.
                if !folder.eq_ignore_ascii_case("claude") {
                    r.procs = vec![format!("{}.exe", folder.to_lowercase()), format!("{}.exe", name.to_lowercase())];
                }
                out.push(r);
            }
        }
    }
    if let Some(l) = &local {
        if let Some(mut r) = rule(
            "spotify",
            "Apps",
            "Spotify",
            "Songs and pictures Spotify kept to play faster. Downloaded playlists for offline use are kept.",
            true,
            vec![l.join(r"Spotify\Data"), l.join(r"Spotify\Browser\Cache"), l.join(r"Spotify\Browser\Code Cache")],
        ) {
            r.procs = vec!["spotify.exe".into()];
            out.push(r);
        }
    }

    // ----- Developer tools -----
    const DEV: &str = "Developer tools";
    if let Some(l) = &local {
        out.extend(rule("npm", DEV, "npm cache", "Packages npm downloads again when a project needs them.", true, vec![l.join("npm-cache")]));
        out.extend(rule("pnpm", DEV, "pnpm cache", "Package information pnpm downloads again when needed.", true, vec![l.join("pnpm-cache")]));
        out.extend(rule(
            "pnpm-store",
            DEV,
            "pnpm package store",
            "Every package pnpm has installed. Projects keep working, but the next pnpm install downloads everything again.",
            false,
            vec![l.join(r"pnpm\store")],
        ));
        out.extend(rule("yarn", DEV, "Yarn cache", "Packages Yarn downloads again when needed.", true, vec![l.join(r"Yarn\Cache"), l.join(r"Yarn\Berry\cache")]));
        out.extend(rule(
            "pip",
            DEV,
            "Python package caches (pip, uv, Poetry)",
            "Packages pip, uv and Poetry download again when needed.",
            true,
            vec![l.join(r"pip\cache"), l.join(r"uv\cache"), l.join(r"pypoetry\Cache")],
        ));
        out.extend(rule("go", DEV, "Go build cache", "Compiled Go code that is rebuilt when needed.", true, vec![l.join("go-build")]));
        out.extend(rule(
            "nuget",
            DEV,
            ".NET download cache",
            "NuGet downloads that are fetched again when needed.",
            true,
            vec![l.join(r"NuGet\v3-cache"), l.join(r"NuGet\plugins-cache"), l.join(r"NuGet\http-cache")],
        ));
        out.extend(rule(
            "electron",
            DEV,
            "Electron downloads",
            "Copies of Electron that npm install and electron-builder download again when a project needs them.",
            true,
            vec![l.join(r"electron\Cache"), l.join(r"electron-builder\Cache")],
        ));
        out.extend(rule("node-gyp", DEV, "node-gyp cache", "Node.js headers for building native packages. They are downloaded again when needed.", true, vec![l.join(r"node-gyp\Cache")]));
        out.extend(rule("typescript", DEV, "TypeScript type cache", "Type files editors download again when needed.", true, vec![l.join(r"Microsoft\TypeScript")]));
        out.extend(rule(
            "playwright",
            DEV,
            "Playwright browsers",
            "Browsers that Playwright tests use. Get them back with npx playwright install.",
            false,
            vec![l.join("ms-playwright")],
        ));
        out.extend(rule("cypress", DEV, "Cypress app cache", "The Cypress test app. The next npm install of Cypress downloads it again.", false, vec![l.join(r"Cypress\Cache")]));
    }
    if let Some(h) = &home {
        out.extend(rule(
            "cargo",
            DEV,
            "Rust download cache",
            "Crate downloads and their unpacked sources. cargo fetches them again when a project needs them. Do not clear this while a build is running.",
            true,
            vec![h.join(r".cargo\registry\cache"), h.join(r".cargo\registry\src"), h.join(r".rustup\downloads"), h.join(r".rustup\tmp")],
        ));
        out.extend(rule("bun", DEV, "Bun cache", "Packages Bun downloads again when needed.", true, vec![h.join(r".bun\install\cache")]));
        out.extend(rule("android", DEV, "Android tools cache", "Download information Android Studio fetches again when needed.", true, vec![h.join(r".android\cache")]));
        out.extend(rule(
            "gradle",
            DEV,
            "Gradle cache",
            "Libraries and builds Gradle downloads or makes again. The next build of each project takes much longer.",
            false,
            vec![h.join(r".gradle\caches")],
        ));
        out.extend(rule(
            "maven",
            DEV,
            "Maven repository",
            "Libraries Maven downloads again. The next build of each project takes much longer.",
            false,
            vec![h.join(r".m2\repository")],
        ));
        out.extend(rule(
            "nuget-packages",
            DEV,
            ".NET packages",
            "Every NuGet package your projects have used. The next build of each project downloads its packages again.",
            false,
            vec![h.join(r".nuget\packages")],
        ));
        out.extend(rule("go-mod", DEV, "Go modules", "Go modules your projects have used. The next build downloads them again.", false, vec![h.join(r"go\pkg\mod")]));
        out.extend(rule("puppeteer", DEV, "Puppeteer browsers", "Browsers Puppeteer downloaded. The next npm install of Puppeteer downloads them again.", false, vec![h.join(r".cache\puppeteer")]));
        out.extend(rule(
            "huggingface",
            DEV,
            "Hugging Face models",
            "AI models and datasets downloaded from Hugging Face. They download again when a program asks for them, which can take a long time.",
            false,
            vec![h.join(r".cache\huggingface\hub")],
        ));
    }

    // ----- Games -----
    if let Some(l) = &local {
        let mut dirs = vec![l.join(r"Roblox\logs"), l.join(r"Roblox\Downloads")];
        if let Some(t) = &temp {
            dirs.push(t.join("Roblox"));
        }
        if let Some(mut r) = rule(
            "roblox",
            "Games",
            "Roblox",
            "Logs, old installer downloads and files Roblox and Roblox Studio fetch again when needed. Your places and settings are kept.",
            true,
            dirs,
        ) {
            r.procs = vec!["robloxplayerbeta.exe".into(), "robloxstudiobeta.exe".into()];
            out.push(r);
        }
        if let Some(mut r) = rule(
            "epic",
            "Games",
            "Epic Games Launcher",
            "Web pages and logs the launcher makes again. Your games are kept.",
            true,
            vec![l.join(r"EpicGamesLauncher\Saved"), l.join(r"EpicGamesLauncher\Saved\Logs")],
        ) {
            if r.targets[0].dir.ends_with("Saved") {
                r.targets[0].prefixes = &["webcache"];
            }
            r.procs = vec!["epicgameslauncher.exe".into()];
            out.push(r);
        }
    }
    out
}

/// True when a running program holds this file. Chromium and Firefox keep a lock file open,
/// without sharing it, for as long as they run.
fn held(p: &Path) -> bool {
    matches!(std::fs::OpenOptions::new().write(true).open(p), Err(e) if e.raw_os_error() == Some(32))
}

/// Names of every running process, lower case.
fn process_names() -> HashSet<String> {
    use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System};
    let mut sys = System::new();
    sys.refresh_processes_specifics(ProcessesToUpdate::All, true, ProcessRefreshKind::nothing());
    sys.processes().values().map(|p| p.name().to_string_lossy().to_lowercase()).collect()
}

/// Size, file count and newest change of each entry, walked with a few threads at once.
/// Links are not followed and files that are only in OneDrive are not counted.
fn stat_many(entries: &[(PathBuf, Metadata)]) -> Vec<(u64, u64, i64)> {
    let out: Vec<Mutex<(u64, u64, i64)>> = entries
        .iter()
        .map(|(_, md)| Mutex::new(if is_link(md) || md.is_dir() { (0, 0, mtime(md)) } else { (md.len(), 1, mtime(md)) }))
        .collect();
    let jobs: Vec<(usize, PathBuf)> = entries.iter().enumerate().filter(|(_, (_, md))| md.is_dir() && !is_link(md)).map(|(i, (p, _))| (i, p.clone())).collect();
    if jobs.is_empty() {
        return out.into_iter().map(|m| m.into_inner().unwrap()).collect();
    }
    // Folders still to read and how many workers are busy.
    let queue = Mutex::new((jobs, 0usize));
    let cv = Condvar::new();
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).clamp(2, 6);
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| loop {
                let job = {
                    let mut q = queue.lock().unwrap();
                    loop {
                        if let Some(j) = q.0.pop() {
                            q.1 += 1;
                            break Some(j);
                        }
                        if q.1 == 0 {
                            cv.notify_all();
                            break None;
                        }
                        q = cv.wait(q).unwrap();
                    }
                };
                let Some((i, dir)) = job else { return };
                let (mut size, mut n, mut newest) = (0u64, 0u64, 0i64);
                let mut next = Vec::new();
                if let Ok(rd) = std::fs::read_dir(&dir) {
                    for e in rd.flatten() {
                        let Ok(md) = e.metadata() else { continue };
                        newest = newest.max(mtime(&md));
                        if is_link(&md) {
                            continue;
                        }
                        if md.is_dir() {
                            next.push((i, e.path()));
                        } else if md.file_attributes() & CLOUD == 0 {
                            size += md.len();
                            n += 1;
                        }
                    }
                }
                {
                    let mut o = out[i].lock().unwrap();
                    o.0 += size;
                    o.1 += n;
                    o.2 = o.2.max(newest);
                }
                let mut q = queue.lock().unwrap();
                q.0.extend(next);
                q.1 -= 1;
                cv.notify_all();
            });
        }
    });
    out.into_iter().map(|m| m.into_inner().unwrap()).collect()
}

/// The entries of one target that the rule covers.
fn listed(t: &Target) -> Vec<(PathBuf, Metadata)> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(&t.dir) else { return out };
    for e in rd.flatten() {
        if !t.prefixes.is_empty() {
            let name = e.file_name().to_string_lossy().to_lowercase();
            if !t.prefixes.iter().any(|p| name.starts_with(p)) {
                continue;
            }
        }
        if let Ok(md) = e.metadata() {
            out.push((e.path(), md));
        }
    }
    out
}

/// Fills in a rule's size and whether its program is open.
fn measure(r: &mut Rule, procs: &HashSet<String>) {
    let cutoff = r.older_than.map(|age| now() - age);
    r.sizes.clear();
    (r.size, r.files) = (0, 0);
    for t in &r.targets {
        let (mut s, mut n) = (0, 0);
        for (size, files, newest) in stat_many(&listed(t)) {
            if cutoff.is_none_or(|c| newest < c) {
                s += size;
                n += files;
            }
        }
        r.sizes.push(s);
        r.size += s;
        r.files += n;
    }
    r.running = r.locks.iter().any(|l| held(l)) || r.procs.iter().any(|p| procs.contains(p));
    r.measured = true;
}

// ---------- what is measured, kept between looks ----------

struct State {
    measuring: bool,
    at: Option<Instant>,
    list: Vec<Rule>,
    downloads: Option<Downloads>,
    system: Vec<Value>,
}

fn slot() -> &'static Mutex<State> {
    static S: OnceLock<Mutex<State>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(State { measuring: false, at: None, list: Vec::new(), downloads: None, system: Vec::new() }))
}

/// Measures everything in the background, unless that was done in the last half hour and
/// `force` is not set. Each cache shows as soon as it is measured.
pub fn refresh(force: bool) {
    {
        let mut c = slot().lock().unwrap();
        if c.measuring || (!force && c.at.is_some_and(|t| t.elapsed() < Duration::from_secs(1800))) {
            return;
        }
        c.measuring = true;
    }
    std::thread::spawn(|| {
        let mut list = rules();
        {
            // Sizes from the last pass stay up until each one is measured again.
            let mut c = slot().lock().unwrap();
            for r in list.iter_mut() {
                if let Some(old) = c.list.iter().find(|o| o.id == r.id) {
                    (r.size, r.files, r.running) = (old.size, old.files, old.running);
                    r.sizes = old.sizes.clone();
                }
            }
            // Small ones first, and Temp (often the biggest by far) last.
            list.sort_by_key(|r| (r.id == "temp", r.size));
            c.list = list.clone();
            c.system = system_items();
        }
        let procs = process_names();
        for mut r in list {
            measure(&mut r, &procs);
            if let Some(k) = slot().lock().unwrap().list.iter_mut().find(|k| k.id == r.id) {
                *k = r;
            }
        }
        slot().lock().unwrap().at = Some(Instant::now());
        let downloads = list_downloads();
        let mut c = slot().lock().unwrap();
        c.downloads = downloads;
        c.measuring = false;
    });
}

/// True while the first or a later measuring pass runs.
pub fn busy() -> bool {
    slot().lock().unwrap().measuring
}

fn rule_json(r: &Rule) -> Value {
    json!({
        "id": r.id, "group": r.group, "label": r.label, "explain": r.explain, "size": r.size, "files": r.files, "rec": r.rec,
        "running": r.running.then(|| r.app.clone()),
        "paths": r.targets.iter().zip(&r.sizes).filter(|(_, s)| **s > 0).map(|(t, s)| json!({ "path": t.dir.to_string_lossy(), "size": s })).collect::<Vec<_>>(),
    })
}

/// Every rule with its size, for the window and `--cleanup`.
pub fn list_json() -> Vec<Value> {
    slot().lock().unwrap().list.iter().map(rule_json).collect()
}

/// What the Storage page shows about clean-up, measured in the background.
pub fn status() -> Value {
    refresh(false);
    let c = slot().lock().unwrap();
    let dl = c.downloads.as_ref().map(|d| json!({ "path": d.path.to_string_lossy(), "size": d.items.iter().map(|i| i.size).sum::<u64>(), "items": d.items.len() }));
    json!({
        "measuring": c.measuring,
        "pending": c.list.iter().filter(|r| !r.measured).count(),
        "list": c.list.iter().map(rule_json).collect::<Vec<_>>(),
        "groups": GROUPS,
        "downloads": dl,
        "system": c.system,
        "clean": job_json(),
    })
}

// ---------- clearing ----------

#[derive(Default)]
struct Tally {
    freed: AtomicU64,
    files: AtomicU64,
    /// Bytes and items a running program holds open, so they stayed.
    left: AtomicU64,
    in_use: AtomicU64,
}

struct Job {
    cancel: AtomicBool,
    done: AtomicBool,
    freed: AtomicU64,
    /// What was measured before clearing, for the progress bar.
    total: u64,
    label: Mutex<String>,
    results: Mutex<Vec<Value>>,
}

fn job_slot() -> &'static Mutex<Option<Arc<Job>>> {
    static J: OnceLock<Mutex<Option<Arc<Job>>>> = OnceLock::new();
    J.get_or_init(Mutex::default)
}

fn job_json() -> Value {
    let Some(j) = job_slot().lock().unwrap().clone() else { return Value::Null };
    let done = j.done.load(Ordering::SeqCst);
    json!({
        "state": if done { "done" } else { "running" },
        "cancelled": j.cancel.load(Ordering::SeqCst),
        "freed": j.freed.load(Ordering::Relaxed),
        "total": j.total,
        "label": j.label.lock().unwrap().clone(),
        "results": *j.results.lock().unwrap(),
    })
}

/// Deletes one file. A read-only file has that mark taken off first.
fn remove_file(p: &Path, md: &Metadata) -> bool {
    if std::fs::remove_file(p).is_ok() {
        return true;
    }
    if md.file_attributes() & ATTR_READONLY != 0 {
        let mut perm = md.permissions();
        perm.set_readonly(false);
        return std::fs::set_permissions(p, perm).is_ok() && std::fs::remove_file(p).is_ok();
    }
    false
}

/// Deletes a file or a whole folder for good. Returns true when nothing of it is left.
/// Files a running program has open are skipped and counted.
fn remove_entry(p: &Path, md: &Metadata, job: &Job, t: &Tally) -> bool {
    if job.cancel.load(Ordering::Relaxed) {
        return false;
    }
    if is_link(md) {
        // Only the link goes; the folder it points at is not ours to clear.
        return if md.file_attributes() & ATTR_DIRECTORY != 0 { std::fs::remove_dir(p).is_ok() } else { std::fs::remove_file(p).is_ok() };
    }
    if md.is_dir() {
        let Ok(rd) = std::fs::read_dir(p) else {
            t.in_use.fetch_add(1, Ordering::Relaxed);
            return false;
        };
        let mut all = true;
        for e in rd.flatten() {
            match e.metadata() {
                Ok(m) => all &= remove_entry(&e.path(), &m, job, t),
                Err(_) => all = false,
            }
        }
        return all && std::fs::remove_dir(p).is_ok();
    }
    let size = if md.file_attributes() & CLOUD == 0 { md.len() } else { 0 };
    if remove_file(p, md) {
        t.freed.fetch_add(size, Ordering::Relaxed);
        t.files.fetch_add(1, Ordering::Relaxed);
        job.freed.fetch_add(size, Ordering::Relaxed);
        true
    } else {
        t.left.fetch_add(size, Ordering::Relaxed);
        t.in_use.fetch_add(1, Ordering::Relaxed);
        false
    }
}

/// Deletes a stale temporary folder, but only when no running program has a file open in it.
/// Windows refuses to rename a folder while anything inside it is open, so the rename is the
/// test: a folder that cannot be renamed is left exactly as it is.
fn remove_idle_dir(p: &Path, size: u64, job: &Job, t: &Tally) {
    static N: AtomicUsize = AtomicUsize::new(0);
    let aside = p.with_file_name(format!(".jpu-clearing-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
    if std::fs::rename(p, &aside).is_err() {
        t.left.fetch_add(size, Ordering::Relaxed);
        t.in_use.fetch_add(1, Ordering::Relaxed);
        return;
    }
    if let Ok(md) = std::fs::symlink_metadata(&aside) {
        // Stopped part way (cancelled): what is left gets its own name back.
        if !remove_entry(&aside, &md, job, t) && aside.exists() && !p.exists() {
            let _ = std::fs::rename(&aside, p);
        }
    }
}

/// Clears one rule and says what happened.
fn clean_rule(r: &Rule, job: &Job) -> Value {
    let t = Tally::default();
    let cutoff = r.older_than.map(|age| now() - age);
    for target in &r.targets {
        let entries = listed(target);
        // For temporary files, how big each item is and when anything in it last changed.
        let stats = if cutoff.is_some() { stat_many(&entries) } else { Vec::new() };
        let next = AtomicUsize::new(0);
        std::thread::scope(|s| {
            for _ in 0..4 {
                s.spawn(|| loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some((path, md)) = entries.get(i) else { break };
                    if job.cancel.load(Ordering::Relaxed) {
                        break;
                    }
                    if protected_reason(path).is_some() {
                        continue;
                    }
                    match cutoff {
                        None => {
                            remove_entry(path, md, job, &t);
                        }
                        // An item where anything changed lately may belong to a program that is running.
                        Some(c) if stats[i].2 >= c => {}
                        Some(_) if md.is_dir() && !is_link(md) => remove_idle_dir(path, stats[i].0, job, &t),
                        Some(_) => {
                            remove_entry(path, md, job, &t);
                        }
                    }
                });
            }
        });
    }
    let n = |a: &AtomicU64| a.load(Ordering::Relaxed);
    json!({ "id": r.id, "label": r.label, "freed": n(&t.freed), "files": n(&t.files), "left": n(&t.left), "in_use": n(&t.in_use), "app": r.app })
}

fn run_job(job: &Job, picked: Vec<Rule>) {
    let procs = process_names();
    for mut r in picked {
        if job.cancel.load(Ordering::SeqCst) {
            break;
        }
        *job.label.lock().unwrap() = r.label.clone();
        let mut res = clean_rule(&r, job);
        measure(&mut r, &procs);
        res["running"] = json!(r.running);
        job.results.lock().unwrap().push(res);
        // Keep the list in step with what is left.
        for k in slot().lock().unwrap().list.iter_mut().filter(|k| k.id == r.id) {
            (k.size, k.files, k.running) = (r.size, r.files, r.running);
            k.sizes = r.sizes.clone();
        }
    }
    let (freed, files, what) = {
        let res = job.results.lock().unwrap();
        (
            res.iter().map(|x| x["freed"].as_u64().unwrap_or(0)).sum::<u64>(),
            res.iter().map(|x| x["files"].as_u64().unwrap_or(0)).sum::<u64>(),
            res.iter().filter(|x| x["freed"].as_u64() > Some(0)).filter_map(|x| x["label"].as_str()).collect::<Vec<_>>().join(", "),
        )
    };
    if freed > 0 {
        if let Ok(conn) = super::db::open(&crate::paths::local_db()) {
            let _ = super::db::log_storage_clean(&conn, now(), &what, freed, files);
        }
    }
    job.done.store(true, Ordering::SeqCst);
}

fn new_job(ids: &[String]) -> Result<(Arc<Job>, Vec<Rule>), String> {
    let picked: Vec<Rule> = rules().into_iter().filter(|r| ids.iter().any(|i| *i == r.id)).collect();
    if picked.is_empty() {
        return Err("Nothing to clear.".into());
    }
    // The sizes measured for the list are close enough for a progress bar.
    let total = slot().lock().unwrap().list.iter().filter(|r| ids.iter().any(|i| *i == r.id)).map(|r| r.size).sum();
    let mut cur = job_slot().lock().unwrap();
    if cur.as_ref().is_some_and(|j| !j.done.load(Ordering::SeqCst)) {
        return Err("A clean-up is already running.".into());
    }
    let job = Arc::new(Job {
        cancel: AtomicBool::new(false),
        done: AtomicBool::new(false),
        freed: AtomicU64::new(0),
        total,
        label: Mutex::new(picked[0].label.clone()),
        results: Mutex::new(Vec::new()),
    });
    *cur = Some(job.clone());
    Ok((job, picked))
}

/// Starts clearing the chosen rules in the background. Progress shows in `status`.
pub fn clean_start(ids: &[String]) -> Result<(), String> {
    let (job, picked) = new_job(ids)?;
    std::thread::Builder::new().name("storage-clean".into()).spawn(move || run_job(&job, picked)).map_err(|e| e.to_string())?;
    Ok(())
}

/// Clears the chosen rules and returns what happened to each. For `--cleanup clear`.
pub fn clean_blocking(ids: &[String]) -> Result<Vec<Value>, String> {
    let (job, picked) = new_job(ids)?;
    run_job(&job, picked);
    Ok(job.results.lock().unwrap().clone())
}

pub fn clean_cancel() {
    if let Some(j) = job_slot().lock().unwrap().as_ref() {
        j.cancel.store(true, Ordering::SeqCst);
    }
}

/// Forgets a finished clean-up once the window has shown its result.
pub fn clean_dismiss() {
    let mut cur = job_slot().lock().unwrap();
    if cur.as_ref().is_some_and(|j| j.done.load(Ordering::SeqCst)) {
        *cur = None;
    }
}

// ---------- Downloads ----------

struct DlItem {
    path: PathBuf,
    name: String,
    size: u64,
    files: u64,
    /// The newest change to it, or to anything inside it.
    modified: i64,
    dir: bool,
}

struct Downloads {
    path: PathBuf,
    items: Vec<DlItem>,
}

/// Replaces `%NAME%` with that environment variable.
fn expand_env(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(a) = rest.find('%') {
        out.push_str(&rest[..a]);
        let after = &rest[a + 1..];
        match after.find('%') {
            Some(b) => {
                match std::env::var(&after[..b]) {
                    Ok(v) => out.push_str(&v),
                    Err(_) => out.push_str(&rest[a..a + b + 2]),
                }
                rest = &after[b + 1..];
            }
            None => {
                out.push_str(&rest[a..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

/// The user's Downloads folder, which may have been moved to another drive.
fn downloads_dir() -> Option<PathBuf> {
    use winreg::RegKey;
    use winreg::enums::HKEY_CURRENT_USER;
    let moved = RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey(r"Software\Microsoft\Windows\CurrentVersion\Explorer\User Shell Folders")
        .ok()
        .and_then(|k| k.get_value::<String, _>("{374DE290-123F-4565-9164-39C4925E467B}").ok())
        .map(|s| PathBuf::from(expand_env(&s)))
        .filter(|p| p.is_dir());
    moved.or_else(|| env_path("USERPROFILE").map(|h| h.join("Downloads")).filter(|p| p.is_dir()))
}

/// Everything at the top of Downloads with its size. Files that are only in OneDrive are left out.
fn list_downloads() -> Option<Downloads> {
    let path = downloads_dir()?;
    let mut entries: Vec<(PathBuf, Metadata)> = Vec::new();
    for e in std::fs::read_dir(&path).ok()?.flatten() {
        let Ok(md) = e.metadata() else { continue };
        let attrs = md.file_attributes();
        if attrs & CLOUD != 0 || (attrs & ATTR_HIDDEN != 0 && attrs & ATTR_SYSTEM != 0) || is_link(&md) {
            continue;
        }
        entries.push((e.path(), md));
    }
    let items = stat_many(&entries)
        .into_iter()
        .zip(&entries)
        .map(|((size, files, modified), (p, md))| DlItem {
            path: p.clone(),
            name: p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(),
            size,
            files,
            modified,
            dir: md.is_dir(),
        })
        .collect();
    Some(Downloads { path, items })
}

/// What is in Downloads, biggest first, optionally only what has not changed for `days`.
pub fn downloads(days: i64, limit: usize) -> Value {
    let c = slot().lock().unwrap();
    let Some(d) = c.downloads.as_ref() else { return json!({ "ready": false, "busy": c.measuring }) };
    let cutoff = if days > 0 { now() - days * 86_400 } else { i64::MAX };
    let mut old: Vec<&DlItem> = d.items.iter().filter(|i| i.modified < cutoff).collect();
    old.sort_by(|a, b| b.size.cmp(&a.size).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    let setup = |name: &str| {
        let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
        matches!(ext.as_str(), "exe" | "msi" | "msix" | "msixbundle" | "appx" | "appxbundle" | "iso" | "img" | "dmg" | "zip" | "7z" | "rar" | "tar" | "gz" | "tgz")
    };
    json!({
        "ready": true,
        "busy": c.measuring,
        "path": d.path.to_string_lossy(),
        "size": d.items.iter().map(|i| i.size).sum::<u64>(),
        "count": d.items.len(),
        "old_size": old.iter().map(|i| i.size).sum::<u64>(),
        "old_count": old.len(),
        "items": old.iter().take(limit.clamp(1, 2000)).map(|i| json!({
            "path": i.path.to_string_lossy(), "name": i.name, "size": i.size, "files": i.files, "modified": i.modified, "dir": i.dir,
            "type": if i.dir { "Folder" } else { file_type(&i.name) }, "setup": !i.dir && setup(&i.name),
        })).collect::<Vec<_>>(),
    })
}

/// Drops deleted paths from what was measured, so they do not show until the next pass.
pub fn forget_paths(paths: &[PathBuf]) {
    let gone: HashSet<String> = paths.iter().map(|p| norm(p)).collect();
    if let Some(d) = slot().lock().unwrap().downloads.as_mut() {
        d.items.retain(|i| !gone.contains(&norm(&i.path)));
    }
}

// ---------- big files Windows keeps for itself ----------

/// The hibernation and page files and left-over Windows installs on the system drive. These
/// need administrator rights, so they are explained here and removed with Windows' own tools.
fn system_items() -> Vec<Value> {
    let drive = env_path("SystemDrive").map(|d| d.to_string_lossy().to_string()).unwrap_or_else(|| "C:".into());
    let root = PathBuf::from(format!("{drive}\\"));
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(&root) else { return out };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        let (label, explain, how) = match name.to_ascii_lowercase().as_str() {
            "hiberfil.sys" => (
                "Hibernation file",
                "Holds what is in memory while the PC hibernates, and makes Fast Startup work. Turning hibernation off deletes it.",
                "Run powercfg /h off in a terminal opened as administrator.",
            ),
            "pagefile.sys" => (
                "Page file",
                "Windows uses it as extra memory. Windows sets its size by itself, and making it too small can make apps crash.",
                "Change it in Settings > System > About > Advanced system settings > Performance > Advanced > Virtual memory.",
            ),
            "windows.old" => ("Previous version of Windows", "Kept after a big update so you can go back. Windows deletes it by itself after about ten days.", "Remove it in Windows Storage settings > Cleanup recommendations."),
            "$windows.~bt" | "$windows.~ws" => ("Windows update files", "Left over from installing or preparing a Windows update.", "Remove them in Windows Storage settings > Cleanup recommendations."),
            _ => continue,
        };
        let Ok(md) = e.metadata() else { continue };
        let size = if md.is_dir() { None } else { Some(md.len()) };
        out.push(json!({ "name": name, "path": e.path().to_string_lossy(), "label": label, "explain": explain, "how": how, "size": size }));
    }
    out.sort_by(|a, b| b["size"].as_u64().cmp(&a["size"].as_u64()));
    out
}
