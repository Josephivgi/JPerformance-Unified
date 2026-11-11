//! Installed apps: how much disk each one takes (program folder plus its data folders),
//! and uninstalling one with a single click. Uninstalling runs the app's own uninstaller,
//! quietly when it has a quiet mode, and never deletes anything by itself. Folders an
//! uninstaller leaves behind are listed so the user can move them to the Recycle Bin.

use super::storage::{env_path, norm, tree_size};
use crate::cmd;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use winreg::RegKey;
use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_32KEY, KEY_WOW64_64KEY};

const UNINSTALL: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall";

#[derive(Clone, Default)]
struct Entry {
    id: String,
    /// Where its uninstall entry lives, to tell when it is gone.
    hive_cu: bool,
    view: u32,
    key: String,
    name: String,
    publisher: String,
    version: String,
    installed: String,
    location: Option<PathBuf>,
    reg_size: u64,
    uninstall: String,
    quiet: String,
    msi: Option<String>,
    machine: bool,
    /// Microsoft Store package full name.
    store: Option<String>,
    data: Vec<PathBuf>,
}

struct Job {
    name: String,
    state: &'static str,
    msg: String,
    before: u64,
    leftovers: Vec<(String, u64)>,
}

#[derive(Default)]
struct State {
    store: Vec<Entry>,
    /// Program and data size by app id.
    sizes: HashMap<String, (u64, u64)>,
    measuring: bool,
    at: Option<Instant>,
    jobs: HashMap<String, Job>,
}

fn state() -> &'static Mutex<State> {
    static S: OnceLock<Mutex<State>> = OnceLock::new();
    S.get_or_init(Default::default)
}

// ---------- reading what is installed ----------

/// Folders too broad to count as one app's folder, such as Program Files itself.
fn too_broad(p: &Path) -> bool {
    let s = norm(p);
    if s.split('\\').count() < 2 || s.len() <= 3 {
        return true;
    }
    let mut broad: Vec<String> = Vec::new();
    for k in ["SystemRoot", "ProgramFiles", "ProgramFiles(x86)", "ProgramW6432", "ProgramData", "USERPROFILE", "LOCALAPPDATA", "APPDATA", "TEMP", "CommonProgramFiles", "CommonProgramFiles(x86)"] {
        if let Some(v) = env_path(k) {
            broad.push(norm(&v));
        }
    }
    if let Some(l) = env_path("LOCALAPPDATA") {
        for sub in ["Programs", "Packages", "Microsoft", "Temp"] {
            broad.push(norm(&l.join(sub)));
        }
    }
    if let Some(h) = env_path("USERPROFILE") {
        for sub in ["AppData", r"AppData\LocalLow", "Desktop", "Documents", "Downloads"] {
            broad.push(norm(&h.join(sub)));
        }
        if let Some(u) = h.parent() {
            broad.push(norm(u));
        }
    }
    if let Some(w) = env_path("SystemRoot") {
        if s.starts_with(&format!("{}\\", norm(&w))) {
            return true;
        }
    }
    broad.contains(&s)
}

fn unquote(s: &str) -> String {
    s.trim().trim_matches('"').trim_end_matches('\\').to_string()
}

/// `%ProgramFiles%\x` and similar.
fn expand(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find('%') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        match after.find('%') {
            Some(j) if j > 0 => {
                match std::env::var(&after[..j]) {
                    Ok(v) => out.push_str(&v),
                    Err(_) => out.push_str(&rest[i..i + j + 2]),
                }
                rest = &after[j + 1..];
            }
            _ => {
                out.push('%');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Splits a command line into the program and its arguments.
fn split_cmd(s: &str) -> (String, String) {
    let s = expand(s.trim());
    if let Some(r) = s.strip_prefix('"') {
        if let Some(i) = r.find('"') {
            return (r[..i].to_string(), r[i + 1..].trim().to_string());
        }
    }
    let low = s.to_lowercase();
    if let Some(i) = low.find(".exe") {
        let e = i + 4;
        return (s[..e].to_string(), s[e..].trim().to_string());
    }
    match s.split_once(' ') {
        Some((a, b)) => (a.to_string(), b.trim().to_string()),
        None => (s, String::new()),
    }
}

/// The product code in `MsiExec.exe /X{...}`.
fn msi_code(key: &str, uninstall: &str, windows_installer: bool) -> Option<String> {
    let is_guid = |s: &str| s.len() == 38 && s.starts_with('{') && s.ends_with('}');
    if windows_installer && is_guid(key) {
        return Some(key.to_string());
    }
    let low = uninstall.to_lowercase();
    if low.contains("msiexec") {
        let a = uninstall.find('{')?;
        let b = uninstall[a..].find('}')? + a;
        let code = &uninstall[a..=b];
        if is_guid(code) {
            return Some(code.to_string());
        }
    }
    None
}

/// The app's folder: InstallLocation, or the folder of its icon or uninstaller.
fn find_location(install_location: &str, icon: &str, uninstall: &str) -> Option<PathBuf> {
    let ok = |p: PathBuf| (p.is_dir() && !too_broad(&p)).then_some(p);
    let loc = unquote(&expand(install_location));
    if !loc.is_empty() {
        if let Some(p) = ok(PathBuf::from(&loc)) {
            return Some(p);
        }
    }
    let icon = expand(icon);
    let icon = icon.rsplit_once(',').filter(|(_, n)| n.trim().parse::<i32>().is_ok()).map(|(a, _)| a).unwrap_or(&icon);
    for exe in [unquote(icon), split_cmd(uninstall).0] {
        let low = exe.to_lowercase();
        if !low.ends_with(".exe") || low.contains("msiexec") || low.contains(r"\package cache\") || low.contains(r"\installer\") || low.contains(r"\temp\") {
            continue;
        }
        let p = PathBuf::from(&exe);
        if let Some(dir) = p.parent() {
            // Uninstallers often live in an "uninstall" subfolder of the app.
            let dir = match dir.file_name().map(|n| n.to_string_lossy().to_lowercase()) {
                Some(n) if n.starts_with("uninst") => dir.parent().unwrap_or(dir),
                _ => dir,
            };
            if let Some(p) = ok(dir.to_path_buf()) {
                return Some(p);
            }
        }
    }
    None
}

const GENERIC: &[&str] = &["application", "app", "apps", "bin", "program", "programs", "microsoft", "google", "common files", "packages", "temp", "x64", "x86", "current", "data", "cache"];

/// Its name without a version or architecture at the end: "7-Zip 24.08 (x64)" is "7-Zip".
fn base_name(name: &str) -> String {
    let n = name.split(" (").next().unwrap_or(name);
    let mut words: Vec<&str> = n.split_whitespace().collect();
    while words.len() > 1 {
        let w = words[words.len() - 1].to_lowercase();
        if w.starts_with(|c: char| c.is_ascii_digit()) || w.starts_with('v') && w[1..].starts_with(|c: char| c.is_ascii_digit()) || ["x64", "x86", "64-bit", "32-bit", "-"].contains(&w.as_str()) {
            words.pop();
        } else {
            break;
        }
    }
    words.join(" ")
}

/// Folders in AppData named after the app. Only measured and offered after an uninstall,
/// never deleted on their own.
fn data_folders(name: &str, publisher: &str, location: Option<&Path>) -> Vec<PathBuf> {
    let base = base_name(name);
    let mut names: Vec<PathBuf> = vec![PathBuf::from(&base)];
    if let Some(n) = location.and_then(|l| l.file_name()).map(|n| n.to_string_lossy().to_string()) {
        names.push(PathBuf::from(n));
    }
    let pubw = publisher.split_whitespace().next().unwrap_or("").trim_end_matches(',');
    if pubw.len() >= 3 {
        if let Some(rest) = base.strip_prefix(&format!("{pubw} ")) {
            names.push(Path::new(pubw).join(rest));
        }
        names.push(Path::new(pubw).join(&base));
    }
    let names: Vec<PathBuf> = names
        .into_iter()
        .filter(|n| {
            let last = n.file_name().map(|x| x.to_string_lossy().to_lowercase()).unwrap_or_default();
            last.len() >= 3 && !GENERIC.contains(&last.as_str())
        })
        .collect();
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let loc = location.map(norm);
    let mut roots = vec![env_path("LOCALAPPDATA"), env_path("APPDATA")];
    roots.push(env_path("USERPROFILE").map(|h| h.join(r"AppData\LocalLow")));
    for root in roots.into_iter().flatten() {
        for n in &names {
            let p = root.join(n);
            if !p.is_dir() || too_broad(&p) {
                continue;
            }
            let s = norm(&p);
            if let Some(l) = &loc {
                if l == &s || l.starts_with(&format!("{s}\\")) || s.starts_with(&format!("{l}\\")) {
                    continue;
                }
            }
            if seen.insert(s) {
                out.push(p);
            }
        }
    }
    out
}

fn read_registry() -> Vec<Entry> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for (cu, view) in [(false, KEY_WOW64_64KEY), (false, KEY_WOW64_32KEY), (true, KEY_WOW64_64KEY)] {
        let hive = RegKey::predef(if cu { HKEY_CURRENT_USER } else { HKEY_LOCAL_MACHINE });
        let Ok(root) = hive.open_subkey_with_flags(UNINSTALL, KEY_READ | view) else { continue };
        for key in root.enum_keys().flatten() {
            let Ok(k) = root.open_subkey_with_flags(&key, KEY_READ | view) else { continue };
            let s = |n: &str| k.get_value::<String, _>(n).unwrap_or_default();
            let d = |n: &str| k.get_value::<u32, _>(n).unwrap_or(0);
            let name = s("DisplayName").trim().to_string();
            let uninstall = s("UninstallString");
            let quiet = s("QuietUninstallString");
            if name.is_empty() || (uninstall.trim().is_empty() && quiet.trim().is_empty()) || d("SystemComponent") == 1 || !s("ParentKeyName").is_empty() {
                continue;
            }
            if ["update", "hotfix", "security update", "service pack"].contains(&s("ReleaseType").to_lowercase().as_str()) {
                continue;
            }
            let version = s("DisplayVersion");
            let location = find_location(&s("InstallLocation"), &s("DisplayIcon"), &uninstall);
            let dedup = format!("{}|{}|{}", name.to_lowercase(), version, location.as_deref().map(norm).unwrap_or_default());
            if !seen.insert(dedup) {
                continue;
            }
            let publisher = s("Publisher");
            let data = data_folders(&name, &publisher, location.as_deref());
            out.push(Entry {
                id: format!("{}{}\\{key}", if cu { "HKCU" } else { "HKLM" }, if view == KEY_WOW64_32KEY { "32" } else { "" }),
                hive_cu: cu,
                view,
                msi: msi_code(&key, &uninstall, d("WindowsInstaller") == 1),
                key,
                name,
                publisher,
                version,
                installed: s("InstallDate"),
                location,
                reg_size: d("EstimatedSize") as u64 * 1024,
                uninstall,
                quiet,
                machine: !cu,
                store: None,
                data,
            });
        }
    }
    out
}

fn registry_has(e: &Entry) -> bool {
    let hive = RegKey::predef(if e.hive_cu { HKEY_CURRENT_USER } else { HKEY_LOCAL_MACHINE });
    hive.open_subkey_with_flags(format!(r"{UNINSTALL}\{}", e.key), KEY_READ | e.view).is_ok()
}

/// "Microsoft.WindowsCalculator" reads as "Windows Calculator".
fn pretty_package(name: &str) -> String {
    let last = name.rsplit('.').next().unwrap_or(name);
    let mut out = String::new();
    let chars: Vec<char> = last.chars().collect();
    for (i, c) in chars.iter().enumerate() {
        if i > 0 && c.is_uppercase() && (chars[i - 1].is_lowercase() || chars.get(i + 1).is_some_and(|n| n.is_lowercase())) {
            out.push(' ');
        }
        out.push(*c);
    }
    out
}

const STORE_PS: &str = "$ErrorActionPreference='SilentlyContinue'; Get-AppxPackage | Where-Object { -not $_.IsFramework -and -not $_.NonRemovable -and $_.SignatureKind -eq 'Store' } | ForEach-Object { $d = ''; try { $d = (Get-AppxPackageManifest $_).Package.Properties.DisplayName } catch {}; [pscustomobject]@{ n=$_.Name; f=$_.PackageFullName; fam=$_.PackageFamilyName; d=[string]$d; p=[string]$_.Publisher; v=[string]$_.Version; l=[string]$_.InstallLocation } } | ConvertTo-Json -Compress";

/// Microsoft Store apps the user can remove.
fn read_store() -> Vec<Entry> {
    let o = cmd::run("powershell.exe", &["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", STORE_PS], Duration::from_secs(90));
    let v: Value = serde_json::from_str(o.stdout.trim()).unwrap_or(Value::Null);
    let items = match v {
        Value::Array(a) => a,
        Value::Object(_) => vec![v],
        _ => Vec::new(),
    };
    let local = env_path("LOCALAPPDATA");
    items
        .iter()
        .filter_map(|x| {
            let g = |k: &str| x[k].as_str().unwrap_or("").to_string();
            let full = g("f");
            if full.is_empty() {
                return None;
            }
            let d = g("d");
            let name = if d.is_empty() || d.starts_with("ms-resource") { pretty_package(&g("n")) } else { d };
            let publisher = g("p").split(',').next().unwrap_or("").trim_start_matches("CN=").trim_matches('"').to_string();
            let loc = PathBuf::from(g("l"));
            Some(Entry {
                id: format!("STORE\\{full}"),
                name,
                publisher,
                version: g("v"),
                location: loc.is_dir().then_some(loc),
                store: Some(full),
                data: local.iter().map(|l| l.join("Packages").join(g("fam"))).filter(|p| p.is_dir()).collect(),
                ..Default::default()
            })
        })
        .collect()
}

fn all_entries() -> Vec<Entry> {
    let mut v = read_registry();
    v.extend(state().lock().unwrap().store.iter().cloned());
    v
}

/// Measures every app's folders in the background, biggest first, unless done recently.
fn refresh(force: bool) {
    {
        let mut s = state().lock().unwrap();
        if s.measuring || (!force && s.at.is_some_and(|t| t.elapsed() < Duration::from_secs(600))) {
            return;
        }
        s.measuring = true;
    }
    std::thread::spawn(|| {
        let store = read_store();
        state().lock().unwrap().store = store;
        let mut list = all_entries();
        list.sort_by(|a, b| b.reg_size.cmp(&a.reg_size));
        for e in &list {
            // Store app folders often cannot be read; then the size Windows was told is used.
            let program = e.location.as_deref().map(|l| tree_size(l).0).filter(|&n| n > 0).unwrap_or(e.reg_size);
            let data: u64 = e.data.iter().map(|p| tree_size(p).0).sum();
            state().lock().unwrap().sizes.insert(e.id.clone(), (program, data));
        }
        let mut s = state().lock().unwrap();
        s.measuring = false;
        s.at = Some(Instant::now());
    });
}

fn day(yyyymmdd: &str) -> String {
    let d = yyyymmdd.trim();
    if d.len() == 8 && d.chars().all(|c| c.is_ascii_digit()) { format!("{}-{}-{}", &d[..4], &d[4..6], &d[6..]) } else { String::new() }
}

/// Every installed app with its size, and any uninstall that is running or just finished.
pub fn status() -> Value {
    refresh(false);
    let list = all_entries();
    let s = state().lock().unwrap();
    let mut apps: Vec<Value> = list
        .iter()
        .map(|e| {
            let (program, data, measured) = match s.sizes.get(&e.id) {
                Some(&(p, d)) => (p, d, true),
                None => (e.reg_size, 0, false),
            };
            json!({
                "id": e.id, "name": e.name, "publisher": e.publisher, "version": e.version, "installed": day(&e.installed),
                "program": program, "data": data, "total": program + data, "measured": measured,
                "location": e.location.as_ref().map(|p| p.to_string_lossy()),
                "data_paths": e.data.iter().map(|p| p.to_string_lossy()).collect::<Vec<_>>(),
                "quiet": e.store.is_some() || e.msi.is_some() || !e.quiet.trim().is_empty(),
                "admin": e.machine && e.msi.is_none(), "store": e.store.is_some(),
            })
        })
        .collect();
    apps.sort_by(|a, b| b["total"].as_u64().cmp(&a["total"].as_u64()));
    let measured = list.iter().filter(|e| s.sizes.contains_key(&e.id)).count();
    let jobs: Vec<Value> = s
        .jobs
        .iter()
        .map(|(id, j)| json!({ "id": id, "name": j.name, "state": j.state, "msg": j.msg, "before": j.before,
            "leftovers": j.leftovers.iter().map(|(p, n)| json!({ "path": p, "size": n })).collect::<Vec<_>>() }))
        .collect();
    json!({ "measuring": s.measuring, "measured": measured, "count": list.len(), "apps": apps, "jobs": jobs })
}

pub fn rescan() {
    refresh(true);
}

pub fn dismiss(id: &str) {
    state().lock().unwrap().jobs.remove(id);
}

// ---------- uninstalling ----------

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}

/// Starts a program through the shell (so Windows can ask for administrator rights) and waits for it.
/// Err("cancelled") when the user said no to the permission prompt.
fn shell_run_wait(exe: &str, args: &str, admin: bool) -> Result<u32, String> {
    use windows_sys::Win32::Foundation::{CloseHandle, GetLastError};
    use windows_sys::Win32::System::Threading::{GetExitCodeProcess, INFINITE, WaitForSingleObject};
    use windows_sys::Win32::UI::Shell::{SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW};
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    let verb = wide(if admin { "runas" } else { "open" });
    let file = wide(exe);
    let params = wide(args);
    let mut info: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
    info.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
    info.fMask = SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC;
    info.lpVerb = verb.as_ptr();
    info.lpFile = file.as_ptr();
    info.lpParameters = params.as_ptr();
    info.nShow = SW_SHOWNORMAL as i32;
    if unsafe { ShellExecuteExW(&mut info) } == 0 {
        let err = unsafe { GetLastError() };
        return Err(if err == 1223 { "cancelled".into() } else { format!("Windows could not start its uninstaller (error {err}).") });
    }
    if info.hProcess.is_null() {
        return Ok(0);
    }
    let mut code = 0u32;
    unsafe {
        WaitForSingleObject(info.hProcess, INFINITE);
        GetExitCodeProcess(info.hProcess, &mut code);
        CloseHandle(info.hProcess);
    }
    Ok(code)
}

fn set_job(id: &str, state_: &'static str, msg: String, leftovers: Vec<(String, u64)>) {
    if let Some(j) = state().lock().unwrap().jobs.get_mut(id) {
        j.state = state_;
        j.msg = msg;
        j.leftovers = leftovers;
    }
}

fn run_uninstall(e: Entry) {
    let id = e.id.clone();
    let result: Result<(), String> = if let Some(full) = &e.store {
        let o = cmd::run("powershell.exe", &["-NoProfile", "-NonInteractive", "-Command", &format!("Remove-AppxPackage -Package '{}'", full.replace('\'', "''"))], Duration::from_secs(600));
        if o.ok { Ok(()) } else { Err(o.stderr.lines().find(|l| !l.trim().is_empty()).unwrap_or("Windows could not remove it.").trim().to_string()) }
    } else {
        let (exe, args, admin) = if let Some(code) = &e.msi {
            ("msiexec.exe".to_string(), format!("/x {code} /passive /norestart"), false)
        } else {
            let (x, a) = split_cmd(if e.quiet.trim().is_empty() { &e.uninstall } else { &e.quiet });
            (x, a, e.machine)
        };
        match shell_run_wait(&exe, &args, admin) {
            Err(m) if m == "cancelled" => {
                set_job(&id, "cancelled", "You said no to Windows' permission prompt, so nothing was removed.".into(), Vec::new());
                return;
            }
            Err(m) => Err(m),
            // 1602 is "the user cancelled" from Windows Installer.
            Ok(1602) => {
                set_job(&id, "cancelled", "The uninstall was cancelled.".into(), Vec::new());
                return;
            }
            Ok(_) => {
                // Many uninstallers hand off to a copy of themselves and return at once, so wait for the entry to go.
                let t = Instant::now();
                while registry_has(&e) && t.elapsed() < Duration::from_secs(90) {
                    std::thread::sleep(Duration::from_millis(1500));
                }
                if registry_has(&e) { Err("still".into()) } else { Ok(()) }
            }
        }
    };
    match result {
        Ok(()) => {
            let mut left = Vec::new();
            for p in e.location.iter().chain(e.data.iter()) {
                if p.is_dir() && super::storage::protected_reason(p).is_none() {
                    let n = tree_size(p).0;
                    if n > 0 {
                        left.push((p.to_string_lossy().to_string(), n));
                    }
                }
            }
            let mut s = state().lock().unwrap();
            s.sizes.remove(&id);
            if e.store.is_some() {
                s.store.retain(|x| x.id != id);
            }
            drop(s);
            set_job(&id, "done", String::new(), left);
        }
        Err(m) if m == "still" => set_job(&id, "still", "It is still installed. If its uninstaller is open, finish the steps there. It leaves this list once it is gone.".into(), Vec::new()),
        Err(m) => set_job(&id, "failed", m, Vec::new()),
    }
}

/// Starts uninstalling one app. Returns at once; `status` shows how it goes.
pub fn uninstall(id: &str) -> Result<(), String> {
    let e = all_entries().into_iter().find(|e| e.id == id).ok_or("That app is no longer installed.")?;
    let exe = std::env::current_exe().ok().and_then(|x| x.parent().map(norm));
    if let (Some(l), Some(x)) = (e.location.as_deref().map(norm), exe) {
        if l == x {
            return Err("JPerformance Unified cannot uninstall itself from here. Use Windows Settings > Apps.".into());
        }
    }
    {
        let mut s = state().lock().unwrap();
        if s.jobs.get(id).is_some_and(|j| j.state == "running") {
            return Err("It is already being uninstalled.".into());
        }
        let before = s.sizes.get(id).map(|(p, d)| p + d).unwrap_or(e.reg_size);
        s.jobs.insert(id.to_string(), Job { name: e.name.clone(), state: "running", msg: String::new(), before, leftovers: Vec::new() });
    }
    std::thread::spawn(move || run_uninstall(e));
    Ok(())
}
