//! Background collector. Runs as `JPerformance.exe --collector`, samples the whole
//! system every few seconds and rolls per-app usage up into one row per minute.

use super::{db, describe, procs};
use crate::paths;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use sysinfo::{Networks, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

/// Apps below all of these in a minute are not stored, to keep the database small.
const MIN_CPU_PCT: f64 = 0.05;
const MIN_MEM_BYTES: u64 = 40 * 1024 * 1024;
const MIN_IO_BYTES: u64 = 1024 * 1024;
const MAX_APPS_PER_MINUTE: usize = 60;

#[derive(Default)]
struct Acc {
    cpu_sum: f64,
    cpu_max: f64,
    mem_sum: u64,
    mem_max: u64,
    io: u64,
    procs: i64,
}

#[derive(Default)]
struct Now {
    cpu: f64,
    mem: u64,
    io: u64,
    procs: i64,
}

pub fn log(msg: &str) {
    crate::log(msg);
}

/// Records this PC until `stop` is set or recording is turned off.
pub fn run(stop: &AtomicBool) -> Result<(), String> {
    let mut conn = db::open(&paths::local_db()).map_err(|e| e.to_string())?;
    let started = chrono::Utc::now().timestamp();
    db::mark_collector_started(&conn, std::process::id(), started).map_err(|e| e.to_string())?;
    super::recorder::spawn();

    let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1) as f64;
    let mut sys = System::new();
    let mut nets = Networks::new_with_refreshed_list();
    let kind = ProcessRefreshKind::nothing()
        .with_cpu()
        .with_memory()
        .with_disk_usage()
        .with_exe(UpdateKind::OnlyIfNotSet);
    sys.refresh_cpu_usage();
    sys.refresh_processes_specifics(ProcessesToUpdate::All, true, kind);
    let mut last = Instant::now();
    std::thread::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL.max(Duration::from_secs(1)));

    let mut described: HashSet<String> = HashSet::new();
    // Last window title saved per app, so the apps table is only written on change.
    let mut saved_titles: HashMap<String, String> = HashMap::new();
    let mut minute_acc: HashMap<String, Acc> = HashMap::new();
    let mut minute_samples: u32 = 0;
    let mut current_minute: Option<i64> = None;
    let mut last_prune = Instant::now() - Duration::from_secs(3600);
    let mut last_settings_check = Instant::now();
    let mut interval = db::setting_i64(&conn, db::KEY_INTERVAL, db::DEFAULT_INTERVAL).clamp(1, 60);

    loop {
        if stop.load(Ordering::SeqCst) {
            break;
        }
        if last_settings_check.elapsed() >= Duration::from_secs(5) {
            last_settings_check = Instant::now();
            if !db::tracking_enabled(&conn) {
                log("tracking turned off, collector exiting");
                break;
            }
            interval = db::setting_i64(&conn, db::KEY_INTERVAL, db::DEFAULT_INTERVAL).clamp(1, 60);
        }

        sys.refresh_cpu_usage();
        sys.refresh_memory();
        sys.refresh_processes_specifics(ProcessesToUpdate::All, true, kind);
        nets.refresh(true);
        let elapsed_ms = last.elapsed().as_millis().max(1) as i64;
        last = Instant::now();
        let ts = chrono::Utc::now().timestamp();

        // Group processes by executable name (all chrome.exe processes count as one app).
        let mut apps: HashMap<String, Now> = HashMap::new();
        let mut io_read = 0u64;
        let mut io_write = 0u64;
        for (pid, p) in sys.processes() {
            if pid.as_u32() == 0 {
                continue; // System Idle Process
            }
            let name = p.name().to_string_lossy().to_string();
            let du = p.disk_usage();
            io_read += du.read_bytes;
            io_write += du.written_bytes;
            let e = apps.entry(name.clone()).or_default();
            e.cpu += p.cpu_usage() as f64 / cores;
            e.mem += p.memory();
            e.io += du.read_bytes + du.written_bytes;
            e.procs += 1;

            if !described.contains(&name) {
                described.insert(name.clone());
                let exe = p.exe().map(|e| e.to_path_buf());
                let desc = exe.as_deref().and_then(describe::file_description);
                let exe_s = exe.as_ref().map(|e| e.display().to_string());
                if let Err(e) = db::upsert_app(&conn, &name, exe_s.as_deref(), desc.as_deref(), ts) {
                    log(&format!("upsert_app failed: {e}"));
                }
            }
        }

        let (net_rx, net_tx) = nets
            .iter()
            .fold((0u64, 0u64), |(r, t), (_, n)| (r + n.received(), t + n.transmitted()));

        let sample = db::SystemSample {
            ts,
            cpu: sys.global_cpu_usage() as f64,
            mem_used: sys.used_memory(),
            mem_total: sys.total_memory(),
            io_read,
            io_write,
            net_rx,
            net_tx,
            interval_ms: elapsed_ms,
            proc_count: apps.values().map(|n| n.procs).sum(),
            app_count: apps.len() as i64,
        };

        // Taskbar window titles per app.
        let mut titles: HashMap<String, Vec<String>> = HashMap::new();
        for (pid, list) in procs::window_titles() {
            if let Some(p) = sys.process(sysinfo::Pid::from_u32(pid)) {
                let entry = titles.entry(p.name().to_string_lossy().to_string()).or_default();
                for t in list {
                    if !entry.contains(&t) {
                        entry.push(t);
                    }
                }
            }
        }
        for (app, list) in &titles {
            let first = &list[0];
            if saved_titles.get(app) != Some(first) {
                if let Err(e) = db::set_window_title(&conn, app, first) {
                    log(&format!("set_window_title failed: {e}"));
                }
                saved_titles.insert(app.clone(), first.clone());
            }
        }

        // Every running app, for the live views and the Running apps page.
        let live: Vec<db::LiveApp> = apps
            .iter()
            .map(|(name, n)| db::LiveApp {
                app: name.clone(),
                cpu: n.cpu,
                mem: n.mem,
                io_rate: n.io as f64 * 1000.0 / elapsed_ms as f64,
                procs: n.procs,
                titles: titles.get(name).map(|t| t.join(" | ").chars().take(400).collect()),
            })
            .collect();

        if let Err(e) = db::write_sample(&mut conn, &sample, &live) {
            log(&format!("write_sample failed: {e}"));
        }

        // Per-minute rollup.
        let minute = ts - ts.rem_euclid(60);
        if current_minute.is_some_and(|m| m != minute) {
            flush_minute(&mut conn, current_minute.unwrap(), &minute_acc, minute_samples);
            minute_acc.clear();
            minute_samples = 0;
        }
        current_minute = Some(minute);
        minute_samples += 1;
        for (name, n) in apps {
            let a = minute_acc.entry(name).or_default();
            a.cpu_sum += n.cpu;
            a.cpu_max = a.cpu_max.max(n.cpu);
            a.mem_sum += n.mem;
            a.mem_max = a.mem_max.max(n.mem);
            a.io += n.io;
            a.procs = a.procs.max(n.procs);
        }

        if last_prune.elapsed() >= Duration::from_secs(3600) {
            last_prune = Instant::now();
            let days = db::setting_i64(&conn, db::KEY_RETENTION, db::DEFAULT_RETENTION).max(1);
            match db::prune(&conn, ts - days * 86_400) {
                Ok(n) if n > 0 => log(&format!("removed {n} rows older than {days} days")),
                Err(e) => log(&format!("prune failed: {e}")),
                _ => {}
            }
        }

        let target = Duration::from_secs(interval as u64);
        let end = Instant::now() + target.saturating_sub(last.elapsed());
        while Instant::now() < end && !stop.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    if let Some(m) = current_minute {
        flush_minute(&mut conn, m, &minute_acc, minute_samples);
    }
    Ok(())
}

fn flush_minute(conn: &mut rusqlite::Connection, minute: i64, acc: &HashMap<String, Acc>, samples: u32) {
    if samples == 0 {
        return;
    }
    let n = samples as f64;
    let mut rows: Vec<db::AppMinute> = acc
        .iter()
        .map(|(app, a)| db::AppMinute {
            app: app.clone(),
            cpu_avg: a.cpu_sum / n,
            cpu_max: a.cpu_max,
            mem_avg: (a.mem_sum as f64 / n) as u64,
            mem_max: a.mem_max,
            io_bytes: a.io,
            procs: a.procs,
        })
        .filter(|r| r.cpu_avg >= MIN_CPU_PCT || r.mem_avg >= MIN_MEM_BYTES || r.io_bytes >= MIN_IO_BYTES)
        .collect();
    // Keep the heaviest apps by CPU, then fill with the heaviest by memory.
    if rows.len() > MAX_APPS_PER_MINUTE {
        rows.sort_by(|a, b| b.cpu_avg.total_cmp(&a.cpu_avg));
        let mut keep: Vec<db::AppMinute> = rows.drain(..MAX_APPS_PER_MINUTE / 2).collect();
        rows.sort_by(|a, b| b.mem_avg.cmp(&a.mem_avg));
        keep.extend(rows.into_iter().take(MAX_APPS_PER_MINUTE / 2));
        rows = keep;
    }
    if let Err(e) = db::write_minute(conn, minute, &rows) {
        log(&format!("write_minute failed: {e}"));
    }
}
