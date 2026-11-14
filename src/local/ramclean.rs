//! Background RAM cleaner: when memory is nearly full, asks Windows to move the memory of apps that have
//! been idle for a while out of RAM (empties their working set). Nothing is closed and nothing is lost:
//! an app reads back what it needs from disk when it is used again, so it may pause briefly then.

use std::collections::HashMap;

use sysinfo::System;
use windows_sys::Win32::Foundation::CloseHandle;
use windows_sys::Win32::System::ProcessStatus::{K32EmptyWorkingSet, K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SET_QUOTA};
use windows_sys::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};

use super::procs;

/// A process using less than this is not worth trimming.
const MIN_WORKING_SET: u64 = 30 * 1024 * 1024;
/// A process trimmed once is left alone this long, unless it was used again since.
const RETRIM_AFTER: i64 = 30 * 60;

/// What one pass freed, per app: (app, processes trimmed, bytes moved out of RAM).
pub struct Freed {
    pub app: String,
    pub procs: usize,
    pub bytes: u64,
}

/// Process id of the window in front, so the app the user is looking at is never trimmed.
fn foreground_pid() -> Option<u32> {
    let w = unsafe { GetForegroundWindow() };
    if w.is_null() {
        return None;
    }
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(w, &mut pid) };
    (pid != 0).then_some(pid)
}

/// Empties one process's working set. Returns the bytes it gave back, or None if Windows refused.
fn trim(pid: u32) -> Option<u64> {
    let h = unsafe { OpenProcess(PROCESS_SET_QUOTA | PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if h.is_null() {
        return None;
    }
    let ws = |h| {
        let mut c: PROCESS_MEMORY_COUNTERS = unsafe { std::mem::zeroed() };
        c.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
        (unsafe { K32GetProcessMemoryInfo(h, &mut c, c.cb) } != 0).then_some(c.WorkingSetSize as u64)
    };
    let before = ws(h);
    let ok = unsafe { K32EmptyWorkingSet(h) } != 0;
    let after = ws(h);
    unsafe { CloseHandle(h) };
    if !ok {
        return None;
    }
    Some(before.unwrap_or(0).saturating_sub(after.unwrap_or(0)))
}

/// One pass. `activity` maps (pid, start time) to (cpu ms, last active ts); `trimmed` remembers when each
/// process was last trimmed so it is not trimmed again every minute.
pub fn run(
    sys: &System,
    activity: &HashMap<(u32, i64), (u64, i64)>,
    trimmed: &mut HashMap<(u32, i64), i64>,
    ts: i64,
    idle_for: i64,
    session: Option<u32>,
) -> Vec<Freed> {
    let me = std::process::id();
    // The whole app in front is skipped (all of a browser's processes, not only the one with the window).
    let front_app = foreground_pid()
        .and_then(|pid| sys.process(sysinfo::Pid::from_u32(pid)))
        .map(|p| p.name().to_string_lossy().to_ascii_lowercase());
    trimmed.retain(|k, _| activity.contains_key(k));

    let mut by_app: HashMap<String, Freed> = HashMap::new();
    for (pid, p) in sys.processes() {
        let pid = pid.as_u32();
        if pid <= 4 || pid == me || p.memory() < MIN_WORKING_SET {
            continue;
        }
        // Only the user's own processes.
        if session.is_none() || p.session_id().map(|s| s.as_u32()) != session {
            continue;
        }
        let name = p.name().to_string_lossy().to_string();
        let lower = name.to_ascii_lowercase();
        if front_app.as_deref() == Some(lower.as_str()) {
            continue;
        }
        // Windows' own programs (the shell, the desktop, security) and this app are left alone.
        let exe = p.exe().map(|e| e.display().to_string());
        if procs::is_protected(&name, exe.as_deref()) {
            continue;
        }
        let key = (pid, p.start_time() as i64);
        let Some(&(_, active_at)) = activity.get(&key) else { continue };
        if ts - active_at < idle_for {
            continue;
        }
        if let Some(&at) = trimmed.get(&key) {
            if at > active_at && ts - at < RETRIM_AFTER {
                continue;
            }
        }
        let Some(bytes) = trim(pid) else { continue };
        trimmed.insert(key, ts);
        if bytes == 0 {
            continue;
        }
        let e = by_app.entry(name.clone()).or_insert(Freed { app: name, procs: 0, bytes: 0 });
        e.procs += 1;
        e.bytes += bytes;
    }
    let mut out: Vec<Freed> = by_app.into_values().filter(|f| f.bytes >= 1024 * 1024).collect();
    out.sort_by(|a, b| b.bytes.cmp(&a.bytes));
    out
}
