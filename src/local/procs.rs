//! Stopping a running app. This only ends its processes; nothing is uninstalled or deleted.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, HWND, LPARAM, WAIT_TIMEOUT};
use windows_sys::Win32::System::Threading::{
    OpenProcess, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE, TerminateProcess, WaitForSingleObject,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GW_OWNER, GetWindow, GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId, IsWindowVisible,
    PostMessageW, WM_CLOSE,
};

/// Processes Windows needs, or that would break the desktop if ended.
const PROTECTED: &[&str] = &[
    "system", "registry", "idle", "system idle process", "memory compression", "smss", "csrss", "wininit",
    "winlogon", "services", "lsass", "lsaiso", "svchost", "dwm", "explorer", "fontdrvhost", "sihost", "ctfmon",
    "taskhostw", "conhost", "dllhost", "runtimebroker", "applicationframehost", "shellexperiencehost",
    "startmenuexperiencehost", "searchhost", "searchindexer", "textinputhost", "spoolsv", "audiodg", "msmpeng",
    "mpdefendercoreservice", "nissrv", "securityhealthservice", "securityhealthsystray", "smartscreen", "wudfhost",
    "wmiprvse", "jperformance",
];

/// True for apps that cannot be stopped from JPerformance.
pub fn is_protected(app: &str, exe_path: Option<&str>) -> bool {
    let name = app.to_ascii_lowercase();
    let name = name.trim_end_matches(".exe");
    // This app, its recorder and anything with its name.
    if PROTECTED.contains(&name) || name.starts_with("jperformance") {
        return true;
    }
    let windir = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into()).to_ascii_lowercase();
    exe_path.is_some_and(|p| p.to_ascii_lowercase().starts_with(&format!("{windir}\\")))
}

/// Programs that live in the Windows folder but are only ever started as helpers
/// (a console host, a shell). They may be ended as part of a tree whose owner is being stopped.
const WINDOWS_HELPERS: &[&str] = &[
    "cmd", "conhost", "powershell", "timeout", "ping", "findstr", "where", "more", "choice", "tasklist", "reg",
    "wsl", "wslhost", "curl", "tar", "certutil", "sort", "whoami", "hostname", "chcp",
];

/// True if `app` may be ended because something that started it is being stopped.
pub fn tree_killable(app: &str, exe_path: Option<&str>) -> bool {
    let name = app.to_ascii_lowercase();
    let name = name.trim_end_matches(".exe");
    WINDOWS_HELPERS.contains(&name) || !is_protected(app, exe_path)
}

/// Visible top-level windows grouped by process id.
pub fn visible_windows() -> HashMap<u32, Vec<HWND>> {
    unsafe extern "system" fn cb(hwnd: HWND, lparam: LPARAM) -> i32 {
        unsafe {
            let out = &mut *(lparam as *mut HashMap<u32, Vec<HWND>>);
            if IsWindowVisible(hwnd) != 0 && GetWindow(hwnd, GW_OWNER).is_null() {
                let mut pid = 0u32;
                GetWindowThreadProcessId(hwnd, &mut pid);
                out.entry(pid).or_default().push(hwnd);
            }
        }
        1
    }
    let mut out: HashMap<u32, Vec<HWND>> = HashMap::new();
    unsafe {
        EnumWindows(Some(cb), &mut out as *mut _ as LPARAM);
    }
    out
}

/// Titles of visible top-level windows (what the taskbar shows), by process id.
pub fn window_titles() -> HashMap<u32, Vec<String>> {
    let mut out: HashMap<u32, Vec<String>> = HashMap::new();
    for (pid, hwnds) in visible_windows() {
        for hwnd in hwnds {
            let title = unsafe {
                let len = GetWindowTextLengthW(hwnd);
                if len <= 0 {
                    continue;
                }
                let mut buf = vec![0u16; len as usize + 1];
                let n = GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32);
                String::from_utf16_lossy(&buf[..n.max(0) as usize])
            };
            let title = title.trim().to_string();
            if !title.is_empty() {
                out.entry(pid).or_default().push(title);
            }
        }
    }
    out
}

pub enum StopResult {
    /// The app closed itself after being asked, like clicking X.
    Closed,
    /// It was asked to close but still has a window open, often a "save changes?" prompt.
    AskedToClose,
    /// Its processes were ended.
    Ended(usize),
    NotRunning,
    /// Windows refused, usually because the app runs as administrator.
    Denied,
    Protected,
}

/// How long an app gets to close its windows when asked.
const CLOSE_WAIT: Duration = Duration::from_secs(6);
/// How long processes may keep running after their windows are gone before they are ended.
/// Apps that hide in the tray on close (Discord, Lunar Client, most Electron apps) never exit on their own.
const EXIT_GRACE: Duration = Duration::from_secs(2);

/// A process of the app being stopped, held open so a reused process id cannot be hit by mistake.
pub struct Proc {
    pid: u32,
    parent: Option<u32>,
    handle: HANDLE,
}

// The handle is only used to wait on and end the process, which is safe from any thread.
unsafe impl Send for Proc {}

impl Proc {
    /// Opens a process so it can be ended. None if Windows refuses (usually an elevated process).
    pub fn open(pid: u32, parent: Option<u32>) -> Option<Proc> {
        let handle = unsafe { OpenProcess(PROCESS_TERMINATE | PROCESS_SYNCHRONIZE, 0, pid) };
        (!handle.is_null()).then_some(Proc { pid, parent, handle })
    }

    fn alive(&self) -> bool {
        unsafe { WaitForSingleObject(self.handle, 0) == WAIT_TIMEOUT }
    }
}

impl Drop for Proc {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.handle);
        }
    }
}

/// Stops every process of `app`. Without `force`, apps with windows are asked
/// to close first so they can save. If the windows go away but the processes keep
/// running (the app went to the tray), or the app has no windows, its processes are ended.
/// Blocks for up to a few seconds, so call it off the UI thread.
pub fn stop(app: &str, force: bool) -> StopResult {
    let mut sys = System::new();
    sys.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing().with_exe(UpdateKind::OnlyIfNotSet),
    );
    let named: Vec<&sysinfo::Process> = sys
        .processes()
        .values()
        .filter(|p| p.name().to_string_lossy() == app)
        .collect();
    if named.is_empty() {
        return StopResult::NotRunning;
    }
    // A process started by JPerformance Unified itself (its own window's WebView2) is not stopped.
    let me = std::process::id();
    let inside_us = |p: &sysinfo::Process| {
        let mut cur = p.parent();
        for _ in 0..16 {
            match cur {
                Some(pp) if pp.as_u32() == me => return true,
                Some(pp) => cur = sys.process(pp).and_then(|x| x.parent()),
                None => return false,
            }
        }
        false
    };
    let matching: Vec<&sysinfo::Process> = named.iter().copied().filter(|p| !inside_us(p)).collect();
    if matching.is_empty() {
        return StopResult::Protected;
    }
    let exe = matching.iter().find_map(|p| p.exe()).map(|e| e.display().to_string());
    if is_protected(app, exe.as_deref()) {
        return StopResult::Protected;
    }
    // Processes Windows will not open for us (usually elevated ones) are skipped.
    let procs: Vec<Proc> = matching
        .iter()
        .filter_map(|p| Proc::open(p.pid().as_u32(), p.parent().map(|p| p.as_u32())))
        .collect();
    stop_procs(procs, force)
}

/// Stops the given processes. Without `force`, the ones with windows are asked to close
/// first; if a window is still open after a few seconds nothing is ended.
pub fn stop_procs(procs: Vec<Proc>, force: bool) -> StopResult {
    if procs.is_empty() {
        return StopResult::Denied;
    }
    if !force {
        let windows = visible_windows();
        let mut asked = false;
        for p in &procs {
            for &hwnd in windows.get(&p.pid).into_iter().flatten() {
                unsafe {
                    PostMessageW(hwnd, WM_CLOSE, 0, 0);
                }
                asked = true;
            }
        }
        if asked {
            let start = Instant::now();
            let mut hidden_since: Option<Instant> = None;
            loop {
                std::thread::sleep(Duration::from_millis(250));
                if !procs.iter().any(Proc::alive) {
                    return StopResult::Closed;
                }
                let windows = visible_windows();
                let has_window = procs.iter().any(|p| p.alive() && windows.contains_key(&p.pid));
                if has_window {
                    hidden_since = None;
                    if start.elapsed() >= CLOSE_WAIT {
                        return StopResult::AskedToClose;
                    }
                } else if hidden_since.get_or_insert_with(Instant::now).elapsed() >= EXIT_GRACE {
                    break;
                }
            }
        }
    }

    end_all(&procs)
}

/// Ends the processes, top-level ones first so a parent cannot restart its children.
fn end_all(procs: &[Proc]) -> StopResult {
    let pids: HashSet<u32> = procs.iter().map(|p| p.pid).collect();
    let (roots, children): (Vec<&Proc>, Vec<&Proc>) =
        procs.iter().partition(|p| p.parent.is_none_or(|parent| !pids.contains(&parent)));
    for p in roots.into_iter().chain(children) {
        if p.alive() {
            unsafe {
                TerminateProcess(p.handle, 1);
            }
        }
    }
    let deadline = Instant::now() + Duration::from_secs(3);
    for p in procs {
        let left = deadline.saturating_duration_since(Instant::now()).as_millis() as u32;
        unsafe {
            WaitForSingleObject(p.handle, left);
        }
    }
    match procs.iter().filter(|p| !p.alive()).count() {
        0 => StopResult::Denied,
        n => StopResult::Ended(n),
    }
}
