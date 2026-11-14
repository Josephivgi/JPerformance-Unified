//! Storage clean-up: what fills a drive, duplicate files, caches that rebuild themselves,
//! and deleting what the user picks. Deleting goes to the Recycle Bin unless the user asks
//! for permanent, and Windows, program and profile folders can never be deleted.

use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::io::{Read, Seek, SeekFrom};
use std::os::windows::fs::MetadataExt;
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

const ATTR_REPARSE: u32 = 0x400;
const ATTR_OFFLINE: u32 = 0x1000;
const ATTR_RECALL_ON_OPEN: u32 = 0x40000;
const ATTR_RECALL_ON_DATA: u32 = 0x40_0000;
/// OneDrive and other cloud files that are not on this PC. Reading them would download them.
pub(crate) const CLOUD: u32 = ATTR_OFFLINE | ATTR_RECALL_ON_OPEN | ATTR_RECALL_ON_DATA;

/// Files at least this big are compared for duplicates and listed as large files.
pub const MIN_FILE: u64 = 256 * 1024;
/// How much of the start and end of a file is read before reading all of it.
const SAMPLE: usize = 16 * 1024;

/// What kind of folder a scanned folder is.
const KIND_NORMAL: u8 = 0;
const KIND_NODE: u8 = 1;
const KIND_RUST: u8 = 2;
/// Windows' own folder, which is not scanned.
const KIND_SKIPPED: u8 = 3;
/// A Python virtual environment (`.venv` or `venv` with a `pyvenv.cfg`).
const KIND_VENV: u8 = 4;
/// Build output of a web project (`.next`, `.nuxt`, `.turbo` and the like, next to a `package.json`).
const KIND_WEB: u8 = 5;
/// `bin` and `obj` next to a .NET project file.
const KIND_DOTNET: u8 = 6;
/// Unity's `Library` folder, next to `Assets` and `ProjectSettings`.
const KIND_UNITY: u8 = 7;

/// True for folders a project makes again when it is built or installed.
fn is_build(kind: u8) -> bool {
    matches!(kind, KIND_NODE | KIND_RUST | KIND_VENV | KIND_WEB | KIND_DOTNET | KIND_UNITY)
}

struct Dir {
    name: String,
    parent: u32,
    own: u64,
    total: u64,
    files: u64,
    kind: u8,
    modified: i64,
    removed: bool,
}

struct FileRec {
    dir: u32,
    name: String,
    size: u64,
    modified: i64,
    removed: bool,
}

struct DupGroup {
    size: u64,
    files: Vec<usize>,
}

struct Done {
    dirs: Vec<Dir>,
    children: Vec<Vec<u32>>,
    files: Vec<FileRec>,
    dups: Vec<DupGroup>,
    /// For every folder, the newest change to it or to anything inside it.
    newest: Vec<i64>,
    /// Files under `MIN_FILE`, added up by type.
    small: Small,
    cloud: u64,
    denied: u64,
    secs: f64,
}

#[derive(Default)]
struct Progress {
    files: AtomicU64,
    bytes: AtomicU64,
    dirs: AtomicU64,
    to_hash: AtomicU64,
    hashed: AtomicU64,
}

pub struct Scan {
    root: PathBuf,
    started: Instant,
    phase: Mutex<&'static str>,
    cancel: AtomicBool,
    p: Progress,
    current: Mutex<String>,
    done: Mutex<Option<Done>>,
    /// Identical files are still being compared after the scan finished.
    dups_busy: AtomicBool,
    /// When these results were found (unix seconds). Set once a scan finishes or saved results are opened.
    saved_at: Mutex<Option<i64>>,
}

static SCAN: OnceLock<Mutex<Option<Arc<Scan>>>> = OnceLock::new();
fn slot() -> &'static Mutex<Option<Arc<Scan>>> {
    SCAN.get_or_init(Mutex::default)
}
fn current() -> Option<Arc<Scan>> {
    slot().lock().unwrap().clone()
}

fn unix(t: std::io::Result<SystemTime>) -> i64 {
    t.ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_secs() as i64).unwrap_or(0)
}

pub(crate) fn env_path(k: &str) -> Option<PathBuf> {
    std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from)
}

/// Lower case, backslashes, no `\\?\` prefix and no trailing backslash, for comparing paths.
pub(crate) fn norm(p: &Path) -> String {
    let s = p.to_string_lossy().replace('/', "\\").to_lowercase();
    let s = s.strip_prefix(r"\\?\").unwrap_or(&s).to_string();
    s.trim_end_matches('\\').to_string()
}

fn under(path: &str, root: &str) -> bool {
    !root.is_empty() && (path == root || path.starts_with(&format!("{root}\\")))
}

/// Folders that may never be deleted, along with everything inside them.
fn protected_trees() -> Vec<String> {
    let mut v: Vec<String> = ["SystemRoot", "ProgramFiles", "ProgramFiles(x86)", "ProgramW6432", "ProgramData"]
        .iter()
        .filter_map(|k| env_path(k))
        .map(|p| norm(&p))
        .collect();
    v.push(norm(&crate::paths::data_dir()));
    if let Some(dir) = std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf)) {
        v.push(norm(&dir));
    }
    v
}

/// Folders whose contents may be deleted but not the folder itself.
fn protected_exact() -> Vec<String> {
    let mut v = Vec::new();
    if let Some(home) = env_path("USERPROFILE") {
        v.push(norm(&home));
        if let Some(users) = home.parent() {
            v.push(norm(users));
            v.push(norm(&users.join("Public")));
        }
        for sub in [
            "AppData", r"AppData\Local", r"AppData\Roaming", r"AppData\LocalLow", "Desktop", "Documents", "Downloads",
            "Pictures", "Music", "Videos", "OneDrive", "Favorites", "Contacts", "Links", "Saved Games", "Searches",
        ] {
            v.push(norm(&home.join(sub)));
        }
    }
    for k in ["OneDrive", "OneDriveConsumer", "OneDriveCommercial", "TEMP", "TMP", "LOCALAPPDATA", "APPDATA"] {
        if let Some(p) = env_path(k) {
            v.push(norm(&p));
        }
    }
    v
}

/// Why `path` may not be deleted, judged from the path alone. None when it may be.
pub fn protected_reason(path: &Path) -> Option<&'static str> {
    let s = norm(path);
    if !path.is_absolute() || s.len() <= 2 || (s.len() <= 3 && s.ends_with(':')) {
        return Some("A whole drive cannot be deleted.");
    }
    let parts: Vec<&str> = s.split('\\').collect();
    if parts.len() == 1 {
        return Some("A whole drive cannot be deleted.");
    }
    // Things at the top of any drive that Windows needs.
    const TOP: &[&str] = &[
        "$recycle.bin", "system volume information", "recovery", "boot", "windows", "program files", "program files (x86)",
        "programdata", "users", "pagefile.sys", "hiberfil.sys", "swapfile.sys", "dumpstack.log", "bootmgr", "$winreagent",
        "$windows.~bt", "$windows.~ws", "config.msi", "documents and settings", "msocache",
    ];
    if TOP.contains(&parts[1]) && (parts.len() == 2 || parts[1] != "users") {
        return Some("Windows needs this.");
    }
    if protected_trees().iter().any(|r| under(&s, r)) {
        return Some("Windows, an installed program or JPerformance needs this.");
    }
    if protected_exact().iter().any(|r| &s == r) {
        return Some("This is one of your main folders. Its contents can be deleted, not the folder.");
    }
    None
}

/// Checks a path just before it is deleted.
pub fn check_deletable(path: &Path) -> Result<std::fs::Metadata, String> {
    if let Some(why) = protected_reason(path) {
        return Err(why.into());
    }
    let md = std::fs::symlink_metadata(path).map_err(|_| "It no longer exists.".to_string())?;
    if md.is_dir() && md.file_attributes() & ATTR_REPARSE != 0 && std::fs::read_link(path).is_ok() {
        return Err("It is a link to another folder. Links are not deleted from here.".into());
    }
    Ok(md)
}

// ---------- scanning ----------

/// Starts scanning `root` in the background. A scan already running is cancelled first.
pub fn start(root: PathBuf) -> Result<(), String> {
    if !root.is_dir() {
        return Err(format!("{} is not a folder that can be read.", root.display()));
    }
    if let Some(old) = current() {
        old.cancel.store(true, Ordering::SeqCst);
    }
    let scan = Arc::new(Scan {
        root: root.clone(),
        started: Instant::now(),
        phase: Mutex::new("walking"),
        cancel: AtomicBool::new(false),
        p: Progress::default(),
        current: Mutex::new(String::new()),
        done: Mutex::new(None),
        dups_busy: AtomicBool::new(false),
        saved_at: Mutex::new(None),
    });
    *slot().lock().unwrap() = Some(scan.clone());
    std::thread::Builder::new()
        .name("storage-scan".into())
        .spawn(move || {
            let done = run(&scan);
            let cancelled = scan.cancel.load(Ordering::SeqCst);
            let found = done.is_some();
            if found && !cancelled {
                scan.dups_busy.store(true, Ordering::SeqCst);
            }
            *scan.done.lock().unwrap() = done;
            *scan.phase.lock().unwrap() = if cancelled { "cancelled" } else { "done" };
            if !found || cancelled {
                return;
            }
            // Saved now, so the results can be opened again later even if the app closes while comparing.
            let at = now_secs();
            *scan.saved_at.lock().unwrap() = Some(at);
            save(&scan, at);
            // What fills the drive is shown now; identical files are compared afterwards, since on a
            // whole drive that means reading many GB.
            let cands: Vec<(usize, PathBuf, u64)> = {
                let g = scan.done.lock().unwrap();
                let d = g.as_ref().unwrap();
                let mut by_size: HashMap<u64, u32> = HashMap::new();
                for f in &d.files {
                    *by_size.entry(f.size).or_default() += 1;
                }
                d.files
                    .iter()
                    .enumerate()
                    .filter(|(_, f)| by_size[&f.size] > 1)
                    .map(|(i, f)| (i, dir_path(&d.dirs, f.dir).join(&f.name), f.size))
                    .collect()
            };
            let groups = find_dups(&scan, &cands);
            if !scan.cancel.load(Ordering::SeqCst) {
                if let Some(d) = scan.done.lock().unwrap().as_mut() {
                    d.dups = groups;
                }
            }
            scan.dups_busy.store(false, Ordering::SeqCst);
            if !scan.cancel.load(Ordering::SeqCst) {
                save(&scan, at);
            }
        })
        .map_err(|e| e.to_string())?;
    Ok(())
}

pub fn cancel() {
    if let Some(s) = current() {
        s.cancel.store(true, Ordering::SeqCst);
    }
}

/// Scans in the calling thread and returns when it is finished. For `--storage` and tests.
pub fn scan_blocking(root: PathBuf) -> Result<(), String> {
    start(root)?;
    let s = current().unwrap();
    super::junk::refresh(false);
    while !matches!(*s.phase.lock().unwrap(), "done" | "cancelled") || s.dups_busy.load(Ordering::SeqCst) || super::junk::busy() {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    Ok(())
}

fn run(scan: &Scan) -> Option<Done> {
    let t0 = Instant::now();
    let (mut dirs, files, cloud, denied, small) = walk(scan);
    if scan.cancel.load(Ordering::SeqCst) {
        return None;
    }
    // Children were created after their parents, so one backwards pass adds every folder into its parent.
    for d in dirs.iter_mut() {
        d.total = d.own;
    }
    for i in (1..dirs.len()).rev() {
        let (t, f, p) = (dirs[i].total, dirs[i].files, dirs[i].parent as usize);
        dirs[p].total += t;
        dirs[p].files += f;
    }
    let mut children = vec![Vec::new(); dirs.len()];
    for (i, d) in dirs.iter().enumerate().skip(1) {
        children[d.parent as usize].push(i as u32);
    }
    // Caches are measured on their own (see `junk`) and duplicates are compared after the
    // results are shown, so a scan never waits on either before showing what fills the drive.
    let newest = newest_changes(&dirs);
    Some(Done { dirs, children, files, dups: Vec::new(), newest, small, cloud, denied, secs: t0.elapsed().as_secs_f64() })
}

/// For every folder, the newest change to it or to anything inside it. Children come after
/// their parents, so one backwards pass carries each folder's time up to its parent.
fn newest_changes(dirs: &[Dir]) -> Vec<i64> {
    let mut newest: Vec<i64> = dirs.iter().map(|d| d.modified).collect();
    for i in (1..dirs.len()).rev() {
        let p = dirs[i].parent as usize;
        newest[p] = newest[p].max(newest[i]);
    }
    newest
}

struct Listed {
    subs: Vec<(String, u8, i64)>,
    files: Vec<(String, u64, i64)>,
    /// Files too small to keep one by one, added up by type: (bytes, count).
    small: HashMap<&'static str, (u64, u64)>,
    own: u64,
    count: u64,
    cloud: u64,
    /// The newest change to a file directly in the folder.
    newest: i64,
}

/// The kinds of file the Types view groups by, in the order it lists them when sizes tie.
pub const TYPES: [&str; 11] =
    ["Videos", "Pictures", "Music", "Archives", "Installers and disk images", "Virtual machines and databases", "Documents", "Programs", "Code", "Game and 3D files", "Other"];

/// Which of `TYPES` a file name belongs to, from its extension.
pub fn file_type(name: &str) -> &'static str {
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    match ext.as_str() {
        "mp4" | "mkv" | "mov" | "avi" | "wmv" | "webm" | "m4v" | "flv" | "mpg" | "mpeg" | "m2ts" | "3gp" => "Videos",
        "jpg" | "jpeg" | "png" | "gif" | "bmp" | "webp" | "heic" | "heif" | "tif" | "tiff" | "raw" | "cr2" | "nef" | "arw" | "dng" | "psd" | "svg" | "ico" | "avif" => "Pictures",
        "mp3" | "wav" | "flac" | "aac" | "m4a" | "ogg" | "opus" | "wma" | "aiff" => "Music",
        "zip" | "7z" | "rar" | "tar" | "gz" | "tgz" | "bz2" | "xz" | "zst" | "cab" => "Archives",
        "iso" | "img" | "msi" | "msix" | "msixbundle" | "appx" | "appxbundle" | "dmg" | "wim" | "esd" => "Installers and disk images",
        "vhd" | "vhdx" | "vmdk" | "vdi" | "qcow2" | "ova" | "ovf" | "sav" | "vmem" | "vmsn" | "db" | "sqlite" | "sqlite3" | "mdf" | "ldf" | "ldb" | "pack" | "bak" => {
            "Virtual machines and databases"
        }
        "pdf" | "doc" | "docx" | "xls" | "xlsx" | "ppt" | "pptx" | "txt" | "md" | "rtf" | "odt" | "ods" | "odp" | "csv" | "epub" | "pages" | "key" | "numbers" => "Documents",
        "exe" | "dll" | "sys" | "so" | "dylib" | "node" | "pdb" | "lib" | "a" | "o" | "obj" | "rlib" | "rmeta" | "pyd" | "jar" | "wasm" | "asar" => "Programs",
        "js" | "mjs" | "cjs" | "ts" | "tsx" | "jsx" | "json" | "rs" | "py" | "pyc" | "c" | "h" | "cpp" | "hpp" | "cs" | "java" | "go" | "rb" | "php" | "html" | "css" | "scss" | "map" | "lua" | "luau" | "toml" | "yaml" | "yml" | "xml" | "sh" | "ps1" | "bat" => "Code",
        "blend" | "blend1" | "fbx" | "glb" | "gltf" | "stl" | "3ds" | "max" | "uasset" | "umap" | "pak" | "unity3d" | "assets" | "bundle" | "vpk" | "rbxl" | "rbxlx" | "rbxm" | "usd" | "usdz" | "dds" | "ktx2" => "Game and 3D files",
        _ => "Other",
    }
}

fn list_dir(path: &Path, in_build: bool, skip: &[String]) -> Option<Listed> {
    let rd = std::fs::read_dir(path).ok()?;
    let mut out = Listed { subs: Vec::new(), files: Vec::new(), small: HashMap::new(), own: 0, count: 0, cloud: 0, newest: 0 };
    let mut entries = Vec::new();
    // What kind of project this folder is, from the files and folders next to its build folders.
    let (mut has_cargo, mut has_package, mut has_dotnet, mut has_assets, mut has_settings) = (false, false, false, false, false);
    for ent in rd.flatten() {
        let name = ent.file_name().to_string_lossy().to_string();
        let l = name.to_ascii_lowercase();
        match l.as_str() {
            "cargo.toml" => has_cargo = true,
            "package.json" => has_package = true,
            "assets" => has_assets = true,
            "projectsettings" => has_settings = true,
            _ if l.ends_with(".csproj") || l.ends_with(".fsproj") || l.ends_with(".vbproj") => has_dotnet = true,
            _ => {}
        }
        entries.push((ent, name));
    }
    for (ent, name) in entries {
        // On Windows this comes from the directory listing itself, without opening the file.
        let Ok(md) = ent.metadata() else { continue };
        let attrs = md.file_attributes();
        if md.is_dir() {
            let full = path.join(&name);
            // Junctions and symbolic links point at folders counted elsewhere (or at themselves).
            if attrs & ATTR_REPARSE != 0 && std::fs::read_link(&full).is_ok() {
                continue;
            }
            let lname = name.to_ascii_lowercase();
            let kind = if skip.iter().any(|s| *s == norm(&full)) {
                KIND_SKIPPED
            } else if in_build {
                KIND_NORMAL
            } else if lname == "node_modules" {
                KIND_NODE
            } else if lname == "target" && has_cargo {
                KIND_RUST
            } else if matches!(lname.as_str(), ".venv" | "venv" | "env") && full.join("pyvenv.cfg").is_file() {
                KIND_VENV
            } else if has_package && matches!(lname.as_str(), ".next" | ".nuxt" | ".turbo" | ".parcel-cache" | ".svelte-kit" | ".angular" | ".vite") {
                KIND_WEB
            } else if has_dotnet && matches!(lname.as_str(), "bin" | "obj") {
                KIND_DOTNET
            } else if lname == "library" && has_assets && has_settings {
                KIND_UNITY
            } else {
                KIND_NORMAL
            };
            out.subs.push((name, kind, unix(md.modified())));
        } else {
            out.count += 1;
            out.newest = out.newest.max(unix(md.modified()));
            if attrs & CLOUD != 0 {
                out.cloud += md.len();
                continue;
            }
            out.own += md.len();
            if md.len() >= MIN_FILE {
                out.files.push((name, md.len(), unix(md.modified())));
            } else {
                let t = out.small.entry(file_type(&name)).or_default();
                t.0 += md.len();
                t.1 += 1;
            }
        }
    }
    Some(out)
}

type Small = HashMap<&'static str, (u64, u64)>;

fn walk(scan: &Scan) -> (Vec<Dir>, Vec<FileRec>, u64, u64, Small) {
    let small: Mutex<Small> = Mutex::new(HashMap::new());
    let root_name = scan.root.to_string_lossy().trim_end_matches('\\').to_string();
    let root_name = if root_name.ends_with(':') { format!("{root_name}\\") } else { root_name };
    let dirs = Mutex::new(vec![Dir { name: root_name, parent: u32::MAX, own: 0, total: 0, files: 0, kind: KIND_NORMAL, modified: 0, removed: false }]);
    let files: Mutex<Vec<FileRec>> = Mutex::new(Vec::new());
    // Jobs to do and how many workers are busy.
    let queue: Mutex<(Vec<(u32, PathBuf, bool)>, usize)> = Mutex::new((vec![(0, scan.root.clone(), false)], 0));
    let cv = Condvar::new();
    let cloud = AtomicU64::new(0);
    let denied = AtomicU64::new(0);
    // Windows' own folder is scanned too, so a whole-drive scan adds up to what the drive uses.
    // It can still never be deleted (see `protected_trees`).
    let skip: Vec<String> = Vec::new();
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).clamp(2, 8);
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| loop {
                let job = {
                    let mut q = queue.lock().unwrap();
                    loop {
                        if scan.cancel.load(Ordering::SeqCst) {
                            q.0.clear();
                            cv.notify_all();
                            break None;
                        }
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
                let Some((id, path, in_build)) = job else { return };
                let mut next = Vec::new();
                match list_dir(&path, in_build, &skip) {
                    None => {
                        denied.fetch_add(1, Ordering::Relaxed);
                    }
                    Some(l) => {
                        scan.p.files.fetch_add(l.count, Ordering::Relaxed);
                        scan.p.bytes.fetch_add(l.own, Ordering::Relaxed);
                        scan.p.dirs.fetch_add(1, Ordering::Relaxed);
                        cloud.fetch_add(l.cloud, Ordering::Relaxed);
                        if !l.small.is_empty() {
                            let mut sm = small.lock().unwrap();
                            for (k, (b, n)) in l.small {
                                let t = sm.entry(k).or_default();
                                t.0 += b;
                                t.1 += n;
                            }
                        }
                        {
                            let mut d = dirs.lock().unwrap();
                            d[id as usize].own = l.own;
                            d[id as usize].files = l.count;
                            // A folder counts as changed when a file in it was, not only when one was added.
                            d[id as usize].modified = d[id as usize].modified.max(l.newest);
                            for (name, kind, modified) in l.subs {
                                let nid = d.len() as u32;
                                let sub = path.join(&name);
                                if kind != KIND_SKIPPED {
                                    next.push((nid, sub, in_build || is_build(kind)));
                                }
                                d.push(Dir { name, parent: id, own: 0, total: 0, files: 0, kind, modified, removed: false });
                            }
                        }
                        if !l.files.is_empty() {
                            let mut f = files.lock().unwrap();
                            for (name, size, modified) in l.files {
                                f.push(FileRec { dir: id, name, size, modified, removed: false });
                            }
                        }
                        if let Ok(mut c) = scan.current.try_lock() {
                            *c = path.to_string_lossy().to_string();
                        }
                    }
                }
                let mut q = queue.lock().unwrap();
                q.0.extend(next);
                q.1 -= 1;
                cv.notify_all();
            });
        }
    });
    (dirs.into_inner().unwrap(), files.into_inner().unwrap(), cloud.into_inner(), denied.into_inner(), small.into_inner().unwrap())
}

fn dir_path(dirs: &[Dir], id: u32) -> PathBuf {
    let mut parts = Vec::new();
    let mut cur = id;
    while cur != u32::MAX {
        let d = &dirs[cur as usize];
        parts.push(d.name.as_str());
        cur = d.parent;
    }
    let mut p = PathBuf::new();
    for part in parts.into_iter().rev() {
        p.push(part);
    }
    p
}

/// The volume and file number, which is the same for every hard link to one file.
fn file_id(f: &std::fs::File) -> Option<(u32, u64)> {
    use windows_sys::Win32::Storage::FileSystem::{BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle};
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    let ok = unsafe { GetFileInformationByHandle(f.as_raw_handle() as _, &mut info) };
    (ok != 0).then(|| (info.dwVolumeSerialNumber, ((info.nFileIndexHigh as u64) << 32) | info.nFileIndexLow as u64))
}

/// Hash of the first and last few KB.
fn sample_hash(f: &mut std::fs::File, size: u64) -> Option<Vec<u8>> {
    let mut h = Sha256::new();
    let mut buf = vec![0u8; SAMPLE];
    let n = f.read(&mut buf).ok()?;
    h.update(&buf[..n]);
    if size > (SAMPLE * 2) as u64 {
        f.seek(SeekFrom::End(-(SAMPLE as i64))).ok()?;
        let n = f.read(&mut buf).ok()?;
        h.update(&buf[..n]);
    }
    Some(h.finalize().to_vec())
}

fn full_hash(path: &Path, scan: &Scan) -> Option<Vec<u8>> {
    let mut f = std::fs::File::open(path).ok()?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        if scan.cancel.load(Ordering::Relaxed) {
            return None;
        }
        let n = f.read(&mut buf).ok()?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
        scan.p.hashed.fetch_add(n as u64, Ordering::Relaxed);
    }
    Some(h.finalize().to_vec())
}

/// Runs `f` on every item with a few threads and collects the answers in order.
fn par_map<T: Sync, R: Send>(items: &[T], f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    let next = AtomicUsize::new(0);
    let out: Mutex<Vec<(usize, R)>> = Mutex::new(Vec::with_capacity(items.len()));
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).clamp(2, 6);
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                let Some(item) = items.get(i) else { break };
                let r = f(item);
                out.lock().unwrap().push((i, r));
            });
        }
    });
    let mut v = out.into_inner().unwrap();
    v.sort_by_key(|(i, _)| *i);
    v.into_iter().map(|(_, r)| r).collect()
}

/// Files with the same size, then the same start and end, then the same SHA-256.
/// Hard links to one file are counted once, since deleting one frees nothing.
/// `cands` are (file index, path, size) of files that share their size with another file.
fn find_dups(scan: &Scan, cands: &[(usize, PathBuf, u64)]) -> Vec<DupGroup> {
    scan.p.to_hash.store(cands.iter().map(|c| c.2).sum(), Ordering::Relaxed);

    // Step 1: file id and a hash of the start and end.
    let sampled = par_map(cands, |(i, path, size)| {
        if scan.cancel.load(Ordering::Relaxed) {
            return None;
        }
        let mut f = std::fs::File::open(path).ok()?;
        let id = file_id(&f)?;
        let h = sample_hash(&mut f, *size)?;
        Some((*i, path.clone(), *size, id, h))
    });
    let mut seen_ids = HashSet::new();
    let mut by_sample: HashMap<(u64, Vec<u8>), Vec<(usize, PathBuf)>> = HashMap::new();
    for (i, path, size, id, h) in sampled.into_iter().flatten() {
        if seen_ids.insert(id) {
            by_sample.entry((size, h)).or_default().push((i, path));
        }
    }
    // Only files that still match need reading in full.
    let to_read: Vec<(usize, PathBuf, u64)> =
        by_sample.into_iter().filter(|(_, v)| v.len() > 1).flat_map(|((size, _), v)| v.into_iter().map(move |(i, p)| (i, p, size))).collect();
    scan.p.to_hash.store(to_read.iter().map(|c| c.2).sum(), Ordering::Relaxed);
    scan.p.hashed.store(0, Ordering::Relaxed);
    let hashed = par_map(&to_read, |(i, path, size)| full_hash(path, scan).map(|h| (*i, *size, h)));
    let mut by_hash: HashMap<(u64, Vec<u8>), Vec<usize>> = HashMap::new();
    for (i, size, h) in hashed.into_iter().flatten() {
        by_hash.entry((size, h)).or_default().push(i);
    }
    let mut groups: Vec<DupGroup> = by_hash.into_iter().filter(|(_, v)| v.len() > 1).map(|((size, _), files)| DupGroup { size, files }).collect();
    groups.sort_by(|a, b| (b.size * (b.files.len() as u64 - 1)).cmp(&(a.size * (a.files.len() as u64 - 1))));
    groups
}

/// Size and file count of a folder, without following links.
pub(crate) fn tree_size(p: &Path) -> (u64, u64) {
    let mut size = 0;
    let mut n = 0;
    let mut stack = vec![p.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let Ok(md) = e.metadata() else { continue };
            if md.is_dir() {
                if md.file_attributes() & ATTR_REPARSE == 0 || std::fs::read_link(e.path()).is_err() {
                    stack.push(e.path());
                }
            } else if md.file_attributes() & CLOUD == 0 {
                size += md.len();
                n += 1;
            }
        }
    }
    (size, n)
}

// ---------- Recycle Bin ----------

pub fn recycle_bin() -> (i64, i64) {
    use windows_sys::Win32::UI::Shell::{SHQUERYRBINFO, SHQueryRecycleBinW};
    let mut info: SHQUERYRBINFO = unsafe { std::mem::zeroed() };
    info.cbSize = std::mem::size_of::<SHQUERYRBINFO>() as u32;
    let hr = unsafe { SHQueryRecycleBinW(std::ptr::null(), &mut info) };
    if hr == 0 { (info.i64Size, info.i64NumItems) } else { (0, 0) }
}

pub fn empty_recycle_bin() -> Result<i64, String> {
    use windows_sys::Win32::UI::Shell::{SHERB_NOCONFIRMATION, SHERB_NOPROGRESSUI, SHERB_NOSOUND, SHEmptyRecycleBinW};
    let (size, n) = recycle_bin();
    if n == 0 {
        return Ok(0);
    }
    let hr = unsafe { SHEmptyRecycleBinW(std::ptr::null_mut(), std::ptr::null(), SHERB_NOCONFIRMATION | SHERB_NOPROGRESSUI | SHERB_NOSOUND) };
    if hr != 0 {
        return Err(format!("Windows could not empty the Recycle Bin (error {hr:#x})."));
    }
    Ok(size - recycle_bin().0)
}

/// Moves one file or folder to the Recycle Bin. If it is too big for the bin, Windows asks first.
fn to_recycle_bin(p: &Path) -> Result<(), String> {
    use windows_sys::Win32::UI::Shell::{
        FO_DELETE, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT, FOF_WANTNUKEWARNING, SHFILEOPSTRUCTW, SHFileOperationW,
    };
    let mut from: Vec<u16> = p.as_os_str().to_string_lossy().encode_utf16().collect();
    from.extend([0, 0]);
    let mut op: SHFILEOPSTRUCTW = unsafe { std::mem::zeroed() };
    op.wFunc = FO_DELETE as _;
    op.pFrom = from.as_ptr();
    op.fFlags = (FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_NOERRORUI | FOF_SILENT | FOF_WANTNUKEWARNING) as _;
    let r = unsafe { SHFileOperationW(&mut op) };
    if op.fAnyOperationsAborted != 0 {
        return Err("Cancelled.".into());
    }
    if r != 0 || p.exists() {
        return Err(if r == 0x20 || r == 0x78 { "A program has it open.".into() } else { format!("Windows could not delete it (error {r:#x}). A program may have it open.") });
    }
    Ok(())
}

// ---------- results ----------

fn removed_dirs(dirs: &[Dir]) -> Vec<bool> {
    let mut r: Vec<bool> = dirs.iter().map(|d| d.removed).collect();
    for i in 1..dirs.len() {
        if r[dirs[i].parent as usize] {
            r[i] = true;
        }
    }
    r
}

fn file_json(d: &Done, i: usize) -> Value {
    let f = &d.files[i];
    let dir = dir_path(&d.dirs, f.dir);
    json!({ "path": dir.join(&f.name).to_string_lossy(), "name": f.name, "folder": dir.to_string_lossy(), "size": f.size, "modified": f.modified, "parent": f.dir })
}

const KIND_NAME: [&str; 8] = ["", "node_modules", "Rust build", "Windows", "Python environment", "Web build", ".NET build", "Unity cache"];
/// What brings each kind of build folder back.
const KIND_BACK: [&str; 8] = ["", "npm install", "cargo build", "", "python -m venv, then pip install", "the next build", "dotnet build", "opening the project in Unity"];

fn kind_name(kind: u8) -> &'static str {
    KIND_NAME.get(kind as usize).copied().unwrap_or("")
}

/// The scan's progress, and its findings once it is done.
pub fn status() -> Value {
    let (bin_size, bin_n) = recycle_bin();
    let caches = super::junk::status();
    let saved = saved_list();
    let Some(s) = current() else { return json!({ "state": "idle", "bin": { "size": bin_size, "items": bin_n }, "caches": caches, "saved": saved }) };
    let phase = *s.phase.lock().unwrap();
    let mut v = json!({
        "state": phase,
        "dups_busy": s.dups_busy.load(Ordering::Relaxed),
        "root": s.root.to_string_lossy(),
        "files": s.p.files.load(Ordering::Relaxed),
        "bytes": s.p.bytes.load(Ordering::Relaxed),
        "dirs": s.p.dirs.load(Ordering::Relaxed),
        "to_hash": s.p.to_hash.load(Ordering::Relaxed),
        "hashed": s.p.hashed.load(Ordering::Relaxed),
        "current": s.current.lock().unwrap().clone(),
        "secs": s.started.elapsed().as_secs(),
        "bin": { "size": bin_size, "items": bin_n },
        "caches": caches,
        "saved": saved,
        "saved_at": *s.saved_at.lock().unwrap(),
    });
    let done = s.done.lock().unwrap();
    let Some(d) = done.as_ref() else { return v };
    let gone = removed_dirs(&d.dirs);
    let live = |i: &usize| !d.files[*i].removed && !gone[d.files[*i].dir as usize];
    let mut largest: Vec<usize> = (0..d.files.len()).filter(live).collect();
    largest.sort_by(|a, b| d.files[*b].size.cmp(&d.files[*a].size));
    let dups: Vec<(u64, Vec<usize>)> = d
        .dups
        .iter()
        .map(|g| (g.size, g.files.iter().copied().filter(live).collect::<Vec<_>>()))
        .filter(|(_, f)| f.len() > 1)
        .collect();
    let wasted: u64 = dups.iter().map(|(s, f)| s * (f.len() as u64 - 1)).sum();
    let mut builds: Vec<Value> = d
        .dirs
        .iter()
        .enumerate()
        .filter(|(i, x)| is_build(x.kind) && !gone[*i] && x.total > 0)
        .map(|(i, x)| {
            // When the project around it last changed says more than the build folder's own date.
            let project = d.newest.get(x.parent as usize).copied().unwrap_or(0).max(d.newest[i]);
            json!({ "id": i, "path": dir_path(&d.dirs, i as u32).to_string_lossy(), "kind": kind_name(x.kind), "back": KIND_BACK.get(x.kind as usize).copied().unwrap_or(""),
                "size": x.total, "files": x.files, "modified": d.newest[i], "project": project })
        })
        .collect();
    builds.sort_by(|a, b| b["size"].as_u64().cmp(&a["size"].as_u64()));
    v["result"] = json!({
        "total": d.dirs[0].total,
        "files": d.dirs[0].files,
        "dirs": d.dirs.len(),
        "cloud": d.cloud,
        "denied": d.denied,
        "secs": d.secs,
        "min_file": MIN_FILE,
        "largest": largest.iter().take(200).map(|&i| file_json(d, i)).collect::<Vec<_>>(),
        "dup_groups": dups.len(),
        "dup_wasted": wasted,
        "dups": dups.iter().take(300).map(|(size, f)| json!({ "size": size, "files": f.iter().map(|&i| file_json(d, i)).collect::<Vec<_>>() })).collect::<Vec<_>>(),
        "builds": builds,
        "types": types_json(d, &gone),
        "junk": super::junk::list_json(),
    });
    v
}

/// One folder of the scan: its subfolders by size, and its files read fresh from disk.
pub fn folder(id: u32, sort: &str, desc: bool) -> Result<Value, String> {
    let s = current().ok_or("Scan first.")?;
    let done = s.done.lock().unwrap();
    let d = done.as_ref().ok_or("Scan first.")?;
    let dir = d.dirs.get(id as usize).ok_or("That folder is not in the scan.")?;
    let gone = removed_dirs(&d.dirs);
    let path = dir_path(&d.dirs, id);
    let mut crumbs = Vec::new();
    let mut cur = id;
    while cur != u32::MAX {
        crumbs.push(json!({ "id": cur, "name": d.dirs[cur as usize].name }));
        cur = d.dirs[cur as usize].parent;
    }
    crumbs.reverse();
    let subs: Vec<Value> = d.children[id as usize]
        .iter()
        .copied()
        .filter(|&c| !gone[c as usize])
        .map(|c| {
            let x = &d.dirs[c as usize];
            let p = dir_path(&d.dirs, c);
            json!({ "id": c, "dir": true, "name": x.name, "path": p.to_string_lossy(), "size": x.total, "files": x.files, "modified": x.modified,
                "kind": kind_name(x.kind), "locked": protected_reason(&p) })
        })
        .collect();
    let n_subs = subs.len();
    let mut items = subs;
    // The second level of the size map, for the biggest subfolders.
    let mut files_by_dir: HashMap<u32, Vec<usize>> = HashMap::new();
    let mut big: Vec<u32> = d.children[id as usize].iter().copied().filter(|&c| !gone[c as usize]).collect();
    big.sort_by(|a, b| d.dirs[*b as usize].total.cmp(&d.dirs[*a as usize].total));
    big.truncate(40);
    for (i, f) in d.files.iter().enumerate() {
        if !f.removed && big.contains(&f.dir) {
            files_by_dir.entry(f.dir).or_default().push(i);
        }
    }
    for it in items.iter_mut() {
        let c = it["id"].as_u64().unwrap_or(0) as u32;
        if big.contains(&c) {
            it["kids"] = json!(kids_json(d, &gone, &files_by_dir, c));
        }
    }
    if let Ok(rd) = std::fs::read_dir(&path) {
        for e in rd.flatten() {
            let Ok(md) = e.metadata() else { continue };
            if md.is_dir() {
                continue;
            }
            let cloud = md.file_attributes() & CLOUD != 0;
            let p = e.path();
            let name = e.file_name().to_string_lossy().to_string();
            items.push(json!({ "dir": false, "type": file_type(&name), "name": name, "path": p.to_string_lossy(), "size": if cloud { 0 } else { md.len() },
                "modified": unix(md.modified()), "cloud": cloud, "locked": protected_reason(&p) }));
        }
    }
    let n_files = items.len() - n_subs;
    sort_items(&mut items, sort, desc);
    let shown = items.len().min(FOLDER_MAX);
    items.truncate(FOLDER_MAX);
    Ok(json!({ "id": id, "path": path.to_string_lossy(), "size": dir.total, "files_total": dir.files, "crumbs": crumbs, "items": items,
        "dirs_n": n_subs, "files_n": n_files, "shown": shown, "skipped": dir.kind == KIND_SKIPPED, "locked": protected_reason(&path) }))
}

/// How many entries of one folder are sent to the window at most.
const FOLDER_MAX: usize = 2000;

/// What is inside one folder for the second level of the size map: its biggest subfolders and big files.
fn kids_json(d: &Done, gone: &[bool], files_by_dir: &HashMap<u32, Vec<usize>>, id: u32) -> Vec<Value> {
    let mut v: Vec<(u64, Value)> = d.children[id as usize]
        .iter()
        .filter(|&&c| !gone[c as usize] && d.dirs[c as usize].total > 0)
        .map(|&c| {
            let x = &d.dirs[c as usize];
            (x.total, json!({ "id": c, "dir": true, "name": x.name, "size": x.total, "files": x.files }))
        })
        .collect();
    for &i in files_by_dir.get(&id).into_iter().flatten() {
        let f = &d.files[i];
        v.push((f.size, json!({ "dir": false, "name": f.name, "size": f.size, "path": dir_path(&d.dirs, id).join(&f.name).to_string_lossy(), "parent": id })));
    }
    v.sort_by(|a, b| b.0.cmp(&a.0));
    v.into_iter().take(16).map(|x| x.1).collect()
}

/// Folders anywhere in the scan, biggest first, so a large folder deep down shows without opening
/// every folder above it. A folder that only passes its space on (one subfolder holds 80% or more
/// of it) is left out, since that subfolder is listed instead.
pub fn biggest_dirs(sort: &str, desc: bool, limit: usize) -> Result<Value, String> {
    let s = current().ok_or("Scan first.")?;
    let done = s.done.lock().unwrap();
    let d = done.as_ref().ok_or("Scan first.")?;
    let gone = removed_dirs(&d.dirs);
    let mut ids: Vec<u32> = (1..d.dirs.len() as u32)
        .filter(|&i| {
            let x = &d.dirs[i as usize];
            if gone[i as usize] || x.total == 0 {
                return false;
            }
            let top = d.children[i as usize].iter().filter(|&&c| !gone[c as usize]).map(|&c| d.dirs[c as usize].total).max().unwrap_or(0);
            (top as f64) < x.total as f64 * 0.8
        })
        .collect();
    // Keep the biggest few thousand, then sort those the way the user asked.
    ids.sort_by(|a, b| d.dirs[*b as usize].total.cmp(&d.dirs[*a as usize].total));
    ids.truncate(3000);
    let mut items: Vec<Value> = ids
        .into_iter()
        .map(|i| {
            let x = &d.dirs[i as usize];
            let p = dir_path(&d.dirs, i);
            json!({ "id": i, "dir": true, "name": x.name, "path": p.to_string_lossy(), "size": x.total, "own": x.own, "files": x.files,
                "modified": x.modified, "kind": kind_name(x.kind), "locked": protected_reason(&p) })
        })
        .collect();
    if sort == "own" {
        items.sort_by(|a, b| {
            let o = a["own"].as_u64().cmp(&b["own"].as_u64());
            if desc { o.reverse() } else { o }
        });
    } else {
        sort_items(&mut items, sort, desc);
    }
    let n = items.len();
    items.truncate(limit.clamp(1, 3000));
    Ok(json!({ "count": n, "dirs": items }))
}

/// Sorts listed entries by "size", "name", "modified" or "files", biggest (or Z, or newest) first when `desc`.
/// Ties fall back to the name, so the order holds still between refreshes.
fn sort_items(items: &mut [Value], sort: &str, desc: bool) {
    let name = |v: &Value| v["name"].as_str().unwrap_or("").to_lowercase();
    items.sort_by(|a, b| {
        let o = match sort {
            "name" => name(a).cmp(&name(b)),
            "modified" => a["modified"].as_i64().cmp(&b["modified"].as_i64()),
            "files" => a["files"].as_u64().unwrap_or(1).cmp(&b["files"].as_u64().unwrap_or(1)),
            _ => a["size"].as_u64().cmp(&b["size"].as_u64()),
        };
        let o = if desc { o.reverse() } else { o };
        o.then_with(|| name(a).cmp(&name(b)))
    });
}

/// Every file of at least `MIN_FILE` the scan found, optionally of one type or matching `q`, sorted.
pub fn files(kind: &str, q: &str, sort: &str, desc: bool, limit: usize) -> Result<Value, String> {
    let s = current().ok_or("Scan first.")?;
    let done = s.done.lock().unwrap();
    let d = done.as_ref().ok_or("Scan first.")?;
    let gone = removed_dirs(&d.dirs);
    let q = q.trim().to_lowercase();
    let mut idx: Vec<usize> = (0..d.files.len())
        .filter(|&i| {
            let f = &d.files[i];
            !f.removed && !gone[f.dir as usize] && (kind.is_empty() || file_type(&f.name) == kind) && (q.is_empty() || f.name.to_lowercase().contains(&q))
        })
        .collect();
    let total: u64 = idx.iter().map(|&i| d.files[i].size).sum();
    let name = |i: usize| d.files[i].name.to_lowercase();
    idx.sort_by(|&a, &b| {
        let (x, y) = (&d.files[a], &d.files[b]);
        let o = match sort {
            "name" => name(a).cmp(&name(b)),
            "modified" => x.modified.cmp(&y.modified),
            _ => x.size.cmp(&y.size),
        };
        let o = if desc { o.reverse() } else { o };
        o.then_with(|| name(a).cmp(&name(b)))
    });
    let n = idx.len();
    let list: Vec<Value> = idx
        .into_iter()
        .take(limit.clamp(1, 5000))
        .map(|i| {
            let mut v = file_json(d, i);
            v["type"] = json!(file_type(&d.files[i].name));
            v
        })
        .collect();
    Ok(json!({ "count": n, "total": total, "files": list }))
}

/// How much each type of file takes in the scan. Big files are counted as they are now, so
/// deleting one shows at once; small files are counted as they were when scanned.
fn types_json(d: &Done, gone: &[bool]) -> Value {
    let mut t: HashMap<&'static str, (u64, u64)> = d.small.clone();
    for f in d.files.iter().filter(|f| !f.removed && !gone[f.dir as usize]) {
        let e = t.entry(file_type(&f.name)).or_default();
        e.0 += f.size;
        e.1 += 1;
    }
    let mut v: Vec<(&str, u64, u64)> = TYPES.iter().map(|k| (*k, t.get(k).map_or(0, |x| x.0), t.get(k).map_or(0, |x| x.1))).filter(|x| x.2 > 0).collect();
    v.sort_by(|a, b| b.1.cmp(&a.1));
    json!(v.into_iter().map(|(k, size, files)| json!({ "type": k, "size": size, "files": files })).collect::<Vec<_>>())
}

#[derive(serde::Deserialize)]
pub struct Item {
    pub path: String,
    /// The scanned folder this is, when it is a folder.
    pub dir: Option<u32>,
    /// The scanned folder it is in, when it is a file.
    pub parent: Option<u32>,
}

#[derive(Serialize)]
pub struct Deleted {
    pub path: String,
    pub ok: bool,
    pub freed: u64,
    pub error: Option<String>,
}

/// True when a file still has the size and modified time it had when the scan read it.
fn matches_scan(path: &str, (size, modified): (u64, i64)) -> bool {
    std::fs::metadata(path).map(|m| m.len() == size && unix(m.modified()) == modified).unwrap_or(false)
}

/// Deletes what the user picked: to the Recycle Bin, or for good when `permanent`.
/// Refuses protected folders, links, and the last copy of a duplicate.
pub fn delete(items: Vec<Item>, permanent: bool) -> Vec<Deleted> {
    let scan = current();
    // Work out what must be kept first. The lock is not held while files are removed,
    // so the page can still read the scan during a long delete.
    let (keep_last, dup_meta) = {
        let guard = scan.as_ref().map(|s| s.done.lock().unwrap());
        let done = guard.as_ref().and_then(|g| g.as_ref());
        let mut keep_last: HashSet<String> = HashSet::new();
        let mut dup_meta: HashMap<String, (u64, i64)> = HashMap::new();
        if let Some(d) = done {
            let picked: HashSet<String> = items.iter().map(|i| norm(Path::new(&i.path))).collect();
            let gone = removed_dirs(&d.dirs);
            for g in &d.dups {
                let members: Vec<(String, (u64, i64))> = g
                    .files
                    .iter()
                    .filter(|&&i| !d.files[i].removed && !gone[d.files[i].dir as usize])
                    .map(|&i| (norm(&dir_path(&d.dirs, d.files[i].dir).join(&d.files[i].name)), (d.files[i].size, d.files[i].modified)))
                    .collect();
                // Never every copy of a duplicate.
                if members.len() > 1 && members.iter().all(|(p, _)| picked.contains(p)) {
                    // Keep a copy that still matches the scan, so the copy the user keeps is the one that is intact.
                    // When no copy matches, every copy is kept.
                    let intact: Vec<String> = members.iter().filter(|(p, m)| matches_scan(p, *m)).map(|(p, _)| p.clone()).collect();
                    match intact.first() {
                        Some(keep) => { keep_last.insert(keep.clone()); }
                        None => keep_last.extend(members.iter().map(|(p, _)| p.clone())),
                    }
                }
                dup_meta.extend(members);
            }
        }
        (keep_last, dup_meta)
    };

    let mut results: Vec<(Item, PathBuf, Result<u64, String>)> = Vec::new();
    for item in items {
        let path = PathBuf::from(&item.path);
        let res = (|| -> Result<u64, String> {
            if keep_last.contains(&norm(&path)) {
                return Err("This is the last copy of a duplicate, so it was kept.".into());
            }
            let md = check_deletable(&path)?;
            // A copy that changed since the scan is not the one the user looked at, so it is kept.
            if let Some(&(size, modified)) = dup_meta.get(&norm(&path)) {
                if md.len() != size || unix(md.modified()) != modified {
                    return Err("It changed after the scan, so it was kept. Scan again to check it.".into());
                }
            }
            let size = if md.is_dir() { tree_size(&path).0 } else { md.len() };
            if permanent {
                if md.is_dir() { std::fs::remove_dir_all(&path) } else { std::fs::remove_file(&path) }.map_err(|e| match e.kind() {
                    std::io::ErrorKind::PermissionDenied => "Windows did not allow it. A program may have it open, or it needs administrator rights.".to_string(),
                    _ => e.to_string(),
                })?;
            } else {
                to_recycle_bin(&path)?;
            }
            Ok(size)
        })();
        results.push((item, path, res));
    }

    let mut out = Vec::new();
    let mut guard = scan.as_ref().map(|s| s.done.lock().unwrap());
    for (item, path, res) in results {
        match res {
            Ok(freed) => {
                if let Some(d) = guard.as_mut().and_then(|g| g.as_mut()) {
                    forget(d, &item, &path, freed);
                }
                out.push(Deleted { path: item.path, ok: true, freed, error: None });
            }
            Err(e) => out.push(Deleted { path: item.path, ok: false, freed: 0, error: Some(e) }),
        }
    }
    drop(guard);
    super::junk::forget_paths(&out.iter().filter(|d| d.ok).map(|d| PathBuf::from(&d.path)).collect::<Vec<_>>());
    // Keep the saved copy in step, so deleted items don't come back when the results are opened later.
    if let Some(s) = scan.filter(|s| out.iter().any(|d| d.ok) && !s.dups_busy.load(Ordering::SeqCst)) {
        let at = s.saved_at.lock().unwrap().unwrap_or_else(now_secs);
        std::thread::spawn(move || save(&s, at));
    }
    out
}

/// Takes a deleted file or folder out of the scan's totals and lists.
fn forget(d: &mut Done, item: &Item, path: &Path, freed: u64) {
    let mut parent = None;
    if let Some(id) = item.dir.filter(|&id| (id as usize) < d.dirs.len() && norm(&dir_path(&d.dirs, id)) == norm(path)) {
        d.dirs[id as usize].removed = true;
        parent = Some(d.dirs[id as usize].parent);
    } else if let Some(pid) = item.parent.filter(|&p| (p as usize) < d.dirs.len() && Some(norm(&dir_path(&d.dirs, p))) == path.parent().map(norm)) {
        let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        for f in d.files.iter_mut().filter(|f| f.dir == pid && f.name.eq_ignore_ascii_case(&name)) {
            f.removed = true;
        }
        d.dirs[pid as usize].own = d.dirs[pid as usize].own.saturating_sub(freed);
        parent = Some(pid);
    }
    let mut cur = parent.unwrap_or(u32::MAX);
    while cur != u32::MAX {
        let x = &mut d.dirs[cur as usize];
        x.total = x.total.saturating_sub(freed);
        cur = x.parent;
    }
}

/// Shows a file or folder in File Explorer.
pub fn reveal(path: &str) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    let p = Path::new(path);
    let mut c = std::process::Command::new("explorer.exe");
    if p.is_dir() { c.arg(p) } else { c.raw_arg(format!("/select,\"{}\"", path.replace('"', ""))) };
    c.spawn().map(|_| ()).map_err(|e| e.to_string())
}

// ---------- saved results ----------
//
// Each finished scan is saved to disk, one file per scanned folder, so it can be opened again
// later without scanning. The format is a small binary one: a whole drive can hold millions of files.

const SAVE_MAGIC: &[u8; 8] = b"JPUSCAN1";

fn now_secs() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

fn user_saved_dir() -> PathBuf {
    crate::paths::data_dir().join("storage-scans")
}

/// Where scans are saved. A hidden test run saves apart, so its scans never show up as the user's results.
fn saved_dir() -> PathBuf {
    if crate::paths::is_smoke() { crate::paths::data_dir().join("smoke-storage-scans") } else { user_saved_dir() }
}

/// Where saved scans are read from: a test run also reads the user's, without writing there.
fn read_dirs() -> Vec<PathBuf> {
    if crate::paths::is_smoke() { vec![saved_dir(), user_saved_dir()] } else { vec![saved_dir()] }
}

fn scan_name(root: &Path) -> String {
    let h = Sha256::digest(norm(root).as_bytes());
    let name: String = h.iter().take(8).map(|b| format!("{b:02x}")).collect();
    format!("{name}.bin")
}

fn saved_file(root: &Path) -> PathBuf {
    saved_dir().join(scan_name(root))
}

struct W(std::io::BufWriter<std::fs::File>);
impl W {
    fn b(&mut self, v: &[u8]) -> std::io::Result<()> {
        std::io::Write::write_all(&mut self.0, v)
    }
    fn u8(&mut self, v: u8) -> std::io::Result<()> {
        self.b(&[v])
    }
    fn u32(&mut self, v: u32) -> std::io::Result<()> {
        self.b(&v.to_le_bytes())
    }
    fn u64(&mut self, v: u64) -> std::io::Result<()> {
        self.b(&v.to_le_bytes())
    }
    fn i64(&mut self, v: i64) -> std::io::Result<()> {
        self.b(&v.to_le_bytes())
    }
    fn f64(&mut self, v: f64) -> std::io::Result<()> {
        self.b(&v.to_le_bytes())
    }
    fn s(&mut self, v: &str) -> std::io::Result<()> {
        self.u32(v.len() as u32)?;
        self.b(v.as_bytes())
    }
}

struct R<T: Read>(T);
impl<T: Read> R<T> {
    fn n<const N: usize>(&mut self) -> std::io::Result<[u8; N]> {
        let mut b = [0u8; N];
        self.0.read_exact(&mut b)?;
        Ok(b)
    }
    fn u8(&mut self) -> std::io::Result<u8> {
        Ok(self.n::<1>()?[0])
    }
    fn u32(&mut self) -> std::io::Result<u32> {
        Ok(u32::from_le_bytes(self.n()?))
    }
    fn u64(&mut self) -> std::io::Result<u64> {
        Ok(u64::from_le_bytes(self.n()?))
    }
    fn i64(&mut self) -> std::io::Result<i64> {
        Ok(i64::from_le_bytes(self.n()?))
    }
    fn f64(&mut self) -> std::io::Result<f64> {
        Ok(f64::from_le_bytes(self.n()?))
    }
    fn s(&mut self) -> std::io::Result<String> {
        let n = self.u32()? as usize;
        if n > 1 << 20 {
            return Err(std::io::Error::other("bad saved scan"));
        }
        let mut b = vec![0u8; n];
        self.0.read_exact(&mut b)?;
        String::from_utf8(b).map_err(std::io::Error::other)
    }
}

/// What the start of a saved file says, read without loading the rest.
struct SavedHead {
    root: String,
    at: i64,
    total: u64,
    files: u64,
}

fn read_head<T: Read>(r: &mut R<T>) -> std::io::Result<SavedHead> {
    if &r.n::<8>()? != SAVE_MAGIC {
        return Err(std::io::Error::other("not a saved scan"));
    }
    Ok(SavedHead { root: r.s()?, at: r.i64()?, total: r.u64()?, files: r.u64()? })
}

/// Writes the scan's results to its file. Deleted items are left out.
fn save(scan: &Scan, at: i64) {
    let g = scan.done.lock().unwrap();
    let Some(d) = g.as_ref() else { return };
    let _ = std::fs::create_dir_all(saved_dir());
    let path = saved_file(&scan.root);
    let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
    let res = (|| -> std::io::Result<()> {
        let gone = removed_dirs(&d.dirs);
        let mut w = W(std::io::BufWriter::with_capacity(1 << 20, std::fs::File::create(&tmp)?));
        w.b(SAVE_MAGIC)?;
        w.s(&scan.root.to_string_lossy())?;
        w.i64(at)?;
        w.u64(d.dirs[0].total)?;
        w.u64(d.dirs[0].files)?;
        w.u64(d.cloud)?;
        w.u64(d.denied)?;
        w.f64(d.secs)?;
        // Folders keep their ids (children always come after their parents); removed ones are marked.
        w.u32(d.dirs.len() as u32)?;
        for (i, x) in d.dirs.iter().enumerate() {
            w.s(&x.name)?;
            w.u32(x.parent)?;
            w.u64(x.own)?;
            w.u64(x.total)?;
            w.u64(x.files)?;
            w.u8(x.kind)?;
            w.i64(x.modified)?;
            w.u8(gone[i] as u8)?;
        }
        // Files are renumbered without the deleted ones, and duplicate sets follow the new numbers.
        let mut new_id = vec![u32::MAX; d.files.len()];
        let live: Vec<usize> = (0..d.files.len()).filter(|&i| !d.files[i].removed && !gone[d.files[i].dir as usize]).collect();
        w.u32(live.len() as u32)?;
        for (n, &i) in live.iter().enumerate() {
            new_id[i] = n as u32;
            let f = &d.files[i];
            w.u32(f.dir)?;
            w.s(&f.name)?;
            w.u64(f.size)?;
            w.i64(f.modified)?;
        }
        let dups: Vec<(u64, Vec<u32>)> = d
            .dups
            .iter()
            .map(|g| (g.size, g.files.iter().map(|&i| new_id[i]).filter(|&n| n != u32::MAX).collect::<Vec<_>>()))
            .filter(|(_, f)| f.len() > 1)
            .collect();
        w.u32(dups.len() as u32)?;
        for (size, f) in &dups {
            w.u64(*size)?;
            w.u32(f.len() as u32)?;
            for &n in f {
                w.u32(n)?;
            }
        }
        w.u32(d.small.len() as u32)?;
        for (k, (a, b)) in &d.small {
            w.s(k)?;
            w.u64(*a)?;
            w.u64(*b)?;
        }
        std::io::Write::flush(&mut w.0)?;
        Ok(())
    })();
    drop(g);
    if res.is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    } else {
        let _ = std::fs::remove_file(&tmp);
    }
}

fn bad() -> std::io::Error {
    std::io::Error::other("bad saved scan")
}

fn read_saved(path: &Path) -> std::io::Result<(SavedHead, Done)> {
    let mut r = R(std::io::BufReader::with_capacity(1 << 20, std::fs::File::open(path)?));
    let head = read_head(&mut r)?;
    let (cloud, denied, secs) = (r.u64()?, r.u64()?, r.f64()?);
    let nd = r.u32()? as usize;
    let mut dirs: Vec<Dir> = Vec::with_capacity(nd.min(10_000_000));
    for _ in 0..nd {
        let name = r.s()?;
        let parent = r.u32()?;
        let ok = if dirs.is_empty() { parent == u32::MAX } else { (parent as usize) < dirs.len() };
        if !ok {
            return Err(bad());
        }
        dirs.push(Dir { name, parent, own: r.u64()?, total: r.u64()?, files: r.u64()?, kind: r.u8()?, modified: r.i64()?, removed: r.u8()? != 0 });
    }
    if dirs.is_empty() {
        return Err(bad());
    }
    let nf = r.u32()? as usize;
    let mut files = Vec::with_capacity(nf.min(50_000_000));
    for _ in 0..nf {
        let dir = r.u32()?;
        if dir as usize >= dirs.len() {
            return Err(bad());
        }
        files.push(FileRec { dir, name: r.s()?, size: r.u64()?, modified: r.i64()?, removed: false });
    }
    let ng = r.u32()? as usize;
    let mut dups = Vec::with_capacity(ng.min(100_000));
    for _ in 0..ng {
        let size = r.u64()?;
        let n = r.u32()? as usize;
        let mut f = Vec::with_capacity(n.min(10_000));
        for _ in 0..n {
            let i = r.u32()? as usize;
            if i >= files.len() {
                return Err(bad());
            }
            f.push(i);
        }
        dups.push(DupGroup { size, files: f });
    }
    let ns = r.u32()? as usize;
    let mut small: Small = HashMap::new();
    for _ in 0..ns {
        let k = r.s()?;
        let key = TYPES.iter().copied().find(|t| *t == k).unwrap_or("Other");
        let (a, b) = (r.u64()?, r.u64()?);
        let e = small.entry(key).or_default();
        e.0 += a;
        e.1 += b;
    }
    let mut children = vec![Vec::new(); dirs.len()];
    for (i, x) in dirs.iter().enumerate().skip(1) {
        children[x.parent as usize].push(i as u32);
    }
    let newest = newest_changes(&dirs);
    Ok((head, Done { dirs, children, files, dups, newest, small, cloud, denied, secs }))
}

/// Every saved scan, newest first: what was scanned, when, and how much it found.
pub fn saved_list() -> Vec<Value> {
    let mut out: Vec<SavedHead> = read_dirs()
        .into_iter()
        .filter_map(|d| std::fs::read_dir(d).ok())
        .flatten()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "bin"))
        .filter_map(|e| read_head(&mut R(std::io::BufReader::new(std::fs::File::open(e.path()).ok()?))).ok())
        // Results of a folder that is gone are no use to open.
        .filter(|h| Path::new(&h.root).is_dir())
        .collect();
    out.sort_by(|a, b| b.at.cmp(&a.at));
    let mut seen = HashSet::new();
    out.retain(|h| seen.insert(norm(Path::new(&h.root))));
    out.iter().map(|h| json!({ "root": h.root, "at": h.at, "total": h.total, "files": h.files })).collect()
}

/// Opens the saved results of an earlier scan of `root` instead of scanning again.
pub fn open_saved(root: &Path) -> Result<(), String> {
    if current().is_some_and(|s| matches!(*s.phase.lock().unwrap(), "walking" | "comparing") || s.dups_busy.load(Ordering::SeqCst)) {
        return Err("Wait for the scan to finish, or cancel it, first.".into());
    }
    let (head, done) = read_dirs()
        .iter()
        .find_map(|d| read_saved(&d.join(scan_name(root))).ok())
        .ok_or_else(|| "There are no saved results for this folder. Scan it instead.".to_string())?;
    let scan = Arc::new(Scan {
        root: PathBuf::from(&head.root),
        started: Instant::now(),
        phase: Mutex::new("done"),
        cancel: AtomicBool::new(false),
        p: Progress::default(),
        current: Mutex::new(String::new()),
        done: Mutex::new(Some(done)),
        dups_busy: AtomicBool::new(false),
        saved_at: Mutex::new(Some(head.at)),
    });
    *slot().lock().unwrap() = Some(scan);
    Ok(())
}
