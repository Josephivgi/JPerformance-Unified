//! The window: a Tauri webview showing `ui/`, and the commands it calls to
//! read history and act. The recorder process writes; this side mostly reads.

use crate::local::{db as ldb, describe as ldesc, insights as linsights, procs};
use crate::servers::{db as sdb, insights as sinsights, sshconf};
use crate::{autostart, collector, paths, update};
use rusqlite::Connection;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use tauri::Manager;

type Res = Result<Value, String>;

struct App {
    /// `--smoke`: the window stays hidden, the page visits every view and reports back.
    smoke: bool,
    local: Pool,
    servers: Pool,
    updater: update::Updater,
    /// Findings by time range. A week of them takes seconds to work out and changes slowly.
    findings: Mutex<HashMap<i64, (Instant, Value)>>,
}

/// SQLite connections to one database. Each command takes its own, so a slow
/// page (a week of findings) does not hold up Settings or the sidebar.
struct Pool {
    path: PathBuf,
    idle: Mutex<Vec<Connection>>,
}

impl Pool {
    fn new(path: PathBuf, first: Connection) -> Pool {
        Pool { path, idle: Mutex::new(vec![first]) }
    }

    fn get(&self) -> Result<Pooled<'_>, String> {
        let conn = match self.idle.lock().unwrap().pop() {
            Some(c) => c,
            None => {
                // The schema was set up by the first connection; this one only reads and writes rows.
                let c = Connection::open(&self.path).map_err(e)?;
                c.busy_timeout(Duration::from_secs(5)).map_err(e)?;
                c
            }
        };
        Ok(Pooled { pool: self, conn: Some(conn) })
    }
}

struct Pooled<'a> {
    pool: &'a Pool,
    conn: Option<Connection>,
}

impl std::ops::Deref for Pooled<'_> {
    type Target = Connection;
    fn deref(&self) -> &Connection {
        self.conn.as_ref().unwrap()
    }
}

impl Drop for Pooled<'_> {
    fn drop(&mut self) {
        if let Some(c) = self.conn.take() {
            let mut idle = self.pool.idle.lock().unwrap();
            if idle.len() < 4 {
                idle.push(c);
            }
        }
    }
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

fn e<E: std::fmt::Display>(err: E) -> String {
    err.to_string()
}

/// Seconds per chart point for a time range, about 150 to 180 points.
fn bucket_for(range: i64) -> i64 {
    match range {
        r if r <= 3_600 => 30,
        r if r <= 6 * 3_600 => 120,
        r if r <= 86_400 => 600,
        r if r <= 7 * 86_400 => 3_600,
        _ => 4 * 3_600,
    }
}

// ---------- this PC ----------

fn host_name() -> String {
    static NAME: OnceLock<String> = OnceLock::new();
    NAME.get_or_init(|| sysinfo::System::host_name().unwrap_or_else(|| "This PC".into())).clone()
}

fn host_info() -> Value {
    // Name, Windows version and CPU do not change while the app runs; only uptime does.
    static FIXED: OnceLock<(String, String, usize)> = OnceLock::new();
    let (os, cpu, cores) = FIXED.get_or_init(|| {
        let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
        (sysinfo::System::long_os_version().unwrap_or_default(), cpu_model(), cores)
    });
    json!({
        "name": host_name(),
        "os": os,
        "cpu_model": cpu,
        "cores": cores,
        "uptime": sysinfo::System::uptime(),
    })
}

fn cpu_model() -> String {
    let mut sys = sysinfo::System::new();
    sys.refresh_cpu_list(sysinfo::CpuRefreshKind::nothing());
    sys.cpus().first().map(|c| c.brand().trim().to_string()).unwrap_or_default()
}

fn disks() -> Vec<Value> {
    let list = sysinfo::Disks::new_with_refreshed_list();
    let mut out: Vec<Value> = list
        .iter()
        .filter(|d| d.total_space() > 0)
        .map(|d| {
            let total = d.total_space() as f64;
            let free = d.available_space() as f64;
            json!({
                "mount": d.mount_point().display().to_string().trim_end_matches('\\').to_string(),
                "name": d.name().to_string_lossy(),
                "total": total,
                "free": free,
                "pct": ((total - free) / total * 100.0).clamp(0.0, 100.0),
            })
        })
        .collect();
    out.sort_by(|a, b| b["pct"].as_f64().unwrap_or(0.0).total_cmp(&a["pct"].as_f64().unwrap_or(0.0)));
    out
}

/// Network in and out (bytes per second) of the latest sample.
fn pc_net(conn: &Connection) -> (f64, f64) {
    conn.query_row(
        "SELECT 1000.0 * net_rx / interval_ms, 1000.0 * net_tx / interval_ms FROM system_samples
         WHERE interval_ms > 0 ORDER BY ts DESC LIMIT 1",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .unwrap_or((0.0, 0.0))
}

fn live_json(rows: &[ldb::LiveRow]) -> Vec<Value> {
    rows.iter()
        .map(|r| {
            json!({
                "app": r.app,
                "name": ldesc::display_name(&r.app, r.description.as_deref()),
                "explain": ldesc::explain(&r.app, r.description.as_deref()),
                "cpu": r.cpu,
                "mem": r.mem,
                "io_rate": r.io_rate,
                "procs": r.procs,
                "titles": r.titles,
                "protected": procs::is_protected(&r.app, r.exe_path.as_deref()),
            })
        })
        .collect()
}

fn pc_summary(conn: &Connection) -> Result<Value, String> {
    let latest = ldb::latest_sample(conn).map_err(e)?;
    let to = now();
    let hour = ldb::system_series(conn, to - 3_600, to, 60).map_err(e)?;
    let live = ldb::live_apps(conn).map_err(e)?;
    let top_mem = live.iter().max_by_key(|r| r.mem);
    let top_cpu = live.iter().max_by(|a, b| a.cpu.total_cmp(&b.cpu));
    let (rx, tx) = pc_net(conn);
    let status = ldb::collector_status(conn).map_err(e)?;
    Ok(json!({
        "host": host_info(),
        "latest": latest.as_ref().map(|(p, total)| json!({ "point": p, "mem_total": total })),
        "cpu_hour": hour.iter().map(|p| [p.ts as f64, p.cpu]).collect::<Vec<_>>(),
        "net_in": rx,
        "net_out": tx,
        "disks": disks(),
        "top_mem": top_mem.map(|r| json!({ "app": r.app, "name": ldesc::display_name(&r.app, r.description.as_deref()), "mem": r.mem })),
        "top_cpu": top_cpu.map(|r| json!({ "app": r.app, "name": ldesc::display_name(&r.app, r.description.as_deref()), "cpu": r.cpu })),
        "heartbeat": status.map(|s| s.heartbeat),
    }))
}

fn severity(s: linsights::Severity) -> &'static str {
    match s {
        linsights::Severity::Critical => "crit",
        linsights::Severity::Warning => "warn",
        linsights::Severity::Info => "info",
    }
}

fn level(l: sinsights::Level) -> &'static str {
    match l {
        sinsights::Level::Danger => "crit",
        sinsights::Level::Warn => "warn",
        sinsights::Level::Info => "info",
    }
}

fn pc_findings(conn: &Connection, from: i64, to: i64, machine: &str) -> Vec<Value> {
    linsights::compute(conn, from, to)
        .unwrap_or_default()
        .into_iter()
        .map(|i| json!({ "level": severity(i.severity), "title": i.title, "detail": i.detail, "at": i.at, "machine": machine, "machine_id": "pc" }))
        .collect()
}

// ---------- servers ----------

fn failed(status: &str) -> bool {
    matches!(status, "timeout" | "hostkey" | "denied" | "refused" | "dns" | "down" | "lost" | "error")
}

fn server_card(conn: &Connection, s: &sdb::Server) -> Result<Value, String> {
    let host = sshconf::lookup(&s.alias);
    let latest = sdb::latest(conn, &s.alias).map_err(e)?;
    let to = now();
    let hour = sdb::series(conn, &s.alias, to - 3_600, to, 60).map_err(e)?;
    let live = sdb::live(conn, &s.alias).map_err(e)?;
    let top = live.iter().max_by(|a, b| a.mem.total_cmp(&b.mem));
    let mounts = sdb::mounts(conn, &s.alias).map_err(e)?;
    let fullest = mounts.iter().max_by(|a, b| a.pct().total_cmp(&b.pct()));
    Ok(json!({
        "alias": s.alias,
        "server": s,
        "address": host.address(),
        "via": host.proxy_jump,
        "failed": failed(&s.status),
        "hint": sinsights::connection_hint(&s.status, &s.alias),
        "latest": latest.as_ref().map(|p| json!({ "point": p, "mem_used": p.mem_used(), "mem_pct": p.mem_pct(), "swap_pct": p.swap_pct() })),
        "cpu_hour": hour.iter().map(|p| [p.ts as f64, p.cpu]).collect::<Vec<_>>(),
        "top": top.map(|g| json!({ "label": g.label, "kind": g.kind, "mem": g.mem })),
        "fullest": fullest.map(|m| json!({ "mount": m.mount, "pct": m.pct(), "avail": m.avail })),
    }))
}

fn server_findings(conn: &Connection, s: &sdb::Server, from: i64, to: i64) -> Vec<Value> {
    sinsights::compute(conn, s, from, to)
        .unwrap_or_default()
        .into_iter()
        .map(|i| {
            let login = failed(&s.status) && i.title.starts_with("Cannot log in");
            json!({
                "level": level(i.level),
                "title": i.title,
                "detail": i.detail,
                "at": if login { json!(s.checked_at) } else { Value::Null },
                "machine": s.alias,
                "machine_id": s.alias,
                "retry": login,
            })
        })
        .collect()
}

fn ssh_on(conn: &Connection) -> bool {
    sdb::monitoring_enabled(conn)
}

/// Servers in ~/.ssh/config that are being watched.
fn watched(conn: &Connection) -> Result<Vec<sdb::Server>, String> {
    let aliases = sshconf::aliases();
    Ok(sdb::servers(conn).map_err(e)?.into_iter().filter(|s| aliases.contains(&s.alias)).collect())
}

fn rank(level: &Value) -> i32 {
    match level.as_str() {
        Some("crit") => 0,
        Some("warn") => 1,
        _ => 2,
    }
}

// ---------- commands ----------

#[tauri::command]
async fn overview(app: tauri::State<'_, App>) -> Res {
    let l = app.local.get()?;
    let s = app.servers.get()?;
    let pc = pc_summary(&l)?;
    let to = now();
    let name = pc["host"]["name"].as_str().unwrap_or("This PC").to_string();
    let mut findings = pc_findings(&l, to - 86_400, to, &name);
    let mut cards = Vec::new();
    let on = ssh_on(&s);
    if on {
        let list = watched(&s)?;
        // Look the addresses up together rather than one `ssh -G` after another.
        sshconf::lookup_all(&list.iter().map(|x| x.alias.clone()).collect::<Vec<_>>());
        for srv in list {
            if !srv.enabled {
                cards.push(server_card(&s, &srv)?);
                continue;
            }
            findings.extend(server_findings(&s, &srv, to - 86_400, to));
            cards.push(server_card(&s, &srv)?);
        }
    }
    findings.sort_by_key(|f| rank(&f["level"]));
    Ok(json!({ "pc": pc, "servers": cards, "ssh": on, "findings": findings, "recorder": collector::is_running() }))
}

#[tauri::command]
async fn sidebar(app: tauri::State<'_, App>) -> Res {
    let l = app.local.get()?;
    let s = app.servers.get()?;
    let latest = ldb::latest_sample(&l).map_err(e)?;
    let on = ssh_on(&s);
    let mut list = Vec::new();
    if on {
        for srv in watched(&s)? {
            let p = sdb::latest(&s, &srv.alias).map_err(e)?;
            list.push(json!({
                "alias": srv.alias, "status": srv.status, "label": srv.label, "enabled": srv.enabled,
                "failed": failed(&srv.status), "checked_at": srv.checked_at,
                "cpu": p.as_ref().map(|p| p.cpu), "mem_pct": p.as_ref().map(|p| p.mem_pct()),
            }));
        }
    }
    Ok(json!({
        "pc": {
            "name": host_name(),
            "cpu": latest.as_ref().map(|(p, _)| p.cpu),
            "mem_pct": latest.as_ref().map(|(p, _)| p.mem_pct),
        },
        "ssh": on,
        "servers": list,
        "recorder": collector::is_running(),
        "autostart": autostart::is_enabled(),
        "retention": ldb::setting_i64(&l, ldb::KEY_RETENTION, ldb::DEFAULT_RETENTION),
        "version": update::CURRENT,
    }))
}

#[tauri::command]
async fn pc_now(app: tauri::State<'_, App>) -> Res {
    let l = app.local.get()?;
    let to = now();
    let recent = ldb::system_series(&l, to - 600, to, 5).map_err(e)?;
    let live = ldb::live_apps(&l).map_err(e)?;
    let (rx, tx) = pc_net(&l);
    let latest = ldb::latest_sample(&l).map_err(e)?;
    Ok(json!({
        "host": host_info(),
        "latest": latest.as_ref().map(|(p, total)| json!({ "point": p, "mem_total": total })),
        "recent": recent,
        "net_in": rx,
        "net_out": tx,
        "apps": live_json(&live),
        "disks": disks(),
        "heartbeat": ldb::collector_status(&l).map_err(e)?.map(|s| s.heartbeat),
    }))
}

#[tauri::command]
async fn pc_history(app: tauri::State<'_, App>, range: i64, which: Option<String>) -> Res {
    let l = app.local.get()?;
    let to = now();
    let from = to - range;
    let bucket = bucket_for(range);
    let heat_from = to - range.max(7 * 86_400);
    match which.filter(|a| !a.is_empty()) {
        Some(a) => Ok(json!({
            "bucket": bucket.max(60),
            "app": a,
            "points": ldb::app_series(&l, &a, from, to, bucket).map_err(e)?,
            "heat": ldb::weekday_hour_app_cpu(&l, &a, heat_from, to).map_err(e)?,
        })),
        None => Ok(json!({
            "bucket": bucket,
            "points": ldb::system_series(&l, from, to, bucket).map_err(e)?,
            "heat": ldb::weekday_hour_cpu(&l, heat_from, to).map_err(e)?,
            "apps": ldb::app_summaries(&l, from, to).map_err(e)?.into_iter().take(40).map(|a| a.app).collect::<Vec<_>>(),
        })),
    }
}

/// The apps using the most CPU and memory around one moment of a chart.
#[tauri::command]
async fn pc_moment(app: tauri::State<'_, App>, ts: i64, bucket: i64) -> Res {
    let l = app.local.get()?;
    let (mut from, to) = (ts - ts.rem_euclid(60), ts + bucket.max(60));
    if ldb::top_apps_in(&l, from, to, 1).map_err(e)?.is_empty() {
        from -= 120;
    }
    let name = |a: &str, d: &Option<String>| ldesc::display_name(a, d.as_deref());
    let cpu: Vec<Value> = ldb::top_apps_in(&l, from, to, 5).map_err(e)?.iter().map(|(a, d, v)| json!({ "name": name(a, d), "v": v })).collect();
    let mem: Vec<Value> = ldb::top_mem_apps_in(&l, from, to, 5).map_err(e)?.iter().map(|(a, d, v)| json!({ "name": name(a, d), "v": v })).collect();
    Ok(json!({ "cpu": cpu, "mem": mem }))
}

#[tauri::command]
async fn pc_apps(app: tauri::State<'_, App>, range: i64) -> Res {
    let l = app.local.get()?;
    let to = now();
    let list: Vec<Value> = ldb::app_summaries(&l, to - range, to)
        .map_err(e)?
        .into_iter()
        .map(|a| {
            let mut v = serde_json::to_value(&a).unwrap_or(Value::Null);
            v["name"] = json!(ldesc::display_name(&a.app, a.description.as_deref()));
            v["explain"] = json!(ldesc::explain(&a.app, a.description.as_deref()));
            v
        })
        .collect();
    let minutes = ldb::minutes_recorded(&l, to - range, to).map_err(e)?;
    Ok(json!({ "apps": list, "minutes": minutes }))
}

#[tauri::command]
async fn stop_app(app_name: String, force: bool) -> Res {
    let r = tauri::async_runtime::spawn_blocking(move || procs::stop(&app_name, force)).await.map_err(e)?;
    Ok(json!(match r {
        procs::StopResult::Closed => json!({ "result": "closed" }),
        procs::StopResult::AskedToClose => json!({ "result": "asked" }),
        procs::StopResult::Ended(n) => json!({ "result": "ended", "count": n }),
        procs::StopResult::NotRunning => json!({ "result": "not_running" }),
        procs::StopResult::Denied => json!({ "result": "denied" }),
        procs::StopResult::Protected => json!({ "result": "protected" }),
    }))
}

#[tauri::command]
async fn recordings(app: tauri::State<'_, App>) -> Res {
    let l = app.local.get()?;
    let list = ldb::sessions(&l, 100).map_err(e)?;
    let live = ldb::live_apps(&l).map_err(e)?;
    let mut apps: Vec<Value> = live
        .iter()
        .filter(|r| !procs::is_protected(&r.app, r.exe_path.as_deref()))
        .map(|r| json!({ "app": r.app, "name": ldesc::display_name(&r.app, r.description.as_deref()), "mem": r.mem }))
        .collect();
    apps.sort_by(|a, b| a["app"].as_str().unwrap_or("").to_lowercase().cmp(&b["app"].as_str().unwrap_or("").to_lowercase()));
    let active: Vec<Value> = list
        .iter()
        .filter(|s| s.ended.is_none())
        .map(|s| json!({ "id": s.id, "series": ldb::session_series(&l, s.id, 1).unwrap_or_default() }))
        .collect();
    Ok(json!({ "sessions": list, "active": active, "apps": apps, "now": now() }))
}

#[tauri::command]
async fn recording_series(app: tauri::State<'_, App>, id: i64) -> Res {
    let l = app.local.get()?;
    Ok(json!(ldb::session_series(&l, id, 1).map_err(e)?))
}

#[tauri::command]
async fn start_recording(app: tauri::State<'_, App>, app_name: String, minutes: Option<i64>) -> Res {
    let l = app.local.get()?;
    let id = ldb::start_session(&l, &app_name, minutes).map_err(e)?;
    let _ = collector::spawn_detached();
    Ok(json!(id))
}

#[tauri::command]
async fn stop_recording(app: tauri::State<'_, App>, id: i64) -> Res {
    let l = app.local.get()?;
    ldb::end_session(&l, id, "stopped").map_err(e)?;
    Ok(Value::Null)
}

#[tauri::command]
async fn delete_recording(app: tauri::State<'_, App>, id: i64) -> Res {
    let l = app.local.get()?;
    ldb::delete_session(&l, id).map_err(e)?;
    Ok(Value::Null)
}

#[tauri::command]
async fn findings(app: tauri::State<'_, App>, range: i64) -> Res {
    if let Some((at, v)) = app.findings.lock().unwrap().get(&range) {
        if at.elapsed() < Duration::from_secs(60) {
            return Ok(v.clone());
        }
    }
    let l = app.local.get()?;
    let s = app.servers.get()?;
    let to = now();
    let name = host_name();
    let mut out = pc_findings(&l, to - range, to, &name);
    if ssh_on(&s) {
        for srv in watched(&s)?.into_iter().filter(|x| x.enabled) {
            out.extend(server_findings(&s, &srv, to - range, to));
        }
    }
    out.sort_by_key(|f| rank(&f["level"]));
    let out = json!(out);
    app.findings.lock().unwrap().insert(range, (Instant::now(), out.clone()));
    Ok(out)
}

#[tauri::command]
async fn server_detail(app: tauri::State<'_, App>, alias: String) -> Res {
    let s = app.servers.get()?;
    let srv = sdb::servers(&s).map_err(e)?.into_iter().find(|x| x.alias == alias).ok_or("This host is no longer in ~/.ssh/config.")?;
    let to = now();
    let mut card = server_card(&s, &srv)?;
    let mut live = sdb::live(&s, &alias).map_err(e)?;
    live.sort_by(|a, b| b.mem.total_cmp(&a.mem));
    card["groups"] = json!(live);
    card["mounts"] = json!(
        sdb::mounts(&s, &alias)
            .map_err(e)?
            .iter()
            .map(|m| json!({ "mount": m.mount, "fs": m.fs, "size": m.size, "used": m.used, "avail": m.avail, "pct": m.pct() }))
            .collect::<Vec<_>>()
    );
    card["recent"] = json!(sdb::series(&s, &alias, to - 600, to, 5).map_err(e)?);
    card["findings"] = json!(server_findings(&s, &srv, to - 86_400, to));
    card["key"] = json!(sshconf::lookup(&alias).identity_files.first().cloned().unwrap_or_default());
    Ok(card)
}

#[tauri::command]
async fn server_history(app: tauri::State<'_, App>, alias: String, range: i64) -> Res {
    let s = app.servers.get()?;
    let to = now();
    let bucket = bucket_for(range);
    let pts = sdb::series(&s, &alias, to - range, to, bucket).map_err(e)?;
    let pts: Vec<Value> = pts
        .iter()
        .map(|p| json!({ "ts": p.ts, "cpu": p.cpu, "mem_pct": p.mem_pct(), "load1": p.load1, "net": p.net_rx + p.net_tx, "disk": p.disk_read + p.disk_write, "swap_pct": p.swap_pct() }))
        .collect();
    Ok(json!({ "bucket": bucket, "points": pts }))
}

#[tauri::command]
async fn server_moment(app: tauri::State<'_, App>, alias: String, ts: i64, bucket: i64) -> Res {
    let s = app.servers.get()?;
    let (mut from, to) = (ts - ts.rem_euclid(60), ts + bucket.max(60));
    if sdb::minutes_recorded(&s, &alias, from, to).map_err(e)? == 0 {
        from -= 120;
    }
    let minutes = sdb::minutes_recorded(&s, &alias, from, to).map_err(e)?.max(1) as f64;
    let list = sdb::group_summaries(&s, &alias, from, to).map_err(e)?;
    let mut by_mem = list.clone();
    by_mem.sort_by(|a, b| b.mem_avg.total_cmp(&a.mem_avg));
    let cpu: Vec<Value> = list.iter().take(5).map(|g| json!({ "name": g.label, "kind": g.kind, "v": g.cpu_load / minutes })).collect();
    let mem: Vec<Value> = by_mem.iter().take(5).map(|g| json!({ "name": g.label, "kind": g.kind, "v": g.mem_avg })).collect();
    Ok(json!({ "cpu": cpu, "mem": mem }))
}

#[tauri::command]
async fn server_retry(app: tauri::State<'_, App>, alias: Option<String>) -> Res {
    app.findings.lock().unwrap().clear();
    let s = app.servers.get()?;
    match alias {
        Some(a) => sdb::bump_server_retry(&s, &a).map_err(e)?,
        None => sdb::bump_retry(&s).map_err(e)?,
    }
    let _ = collector::spawn_detached();
    Ok(Value::Null)
}

#[tauri::command]
async fn server_watch(app: tauri::State<'_, App>, alias: String, on: bool) -> Res {
    app.findings.lock().unwrap().clear();
    let s = app.servers.get()?;
    sdb::ensure_servers(&s, std::slice::from_ref(&alias)).map_err(e)?;
    sdb::set_enabled(&s, &alias, on).map_err(e)?;
    Ok(Value::Null)
}

#[tauri::command]
async fn ssh_keys() -> Res {
    let scan = tauri::async_runtime::spawn_blocking(sshconf::scan).await.map_err(e)?;
    Ok(json!(scan))
}

#[tauri::command]
async fn settings(app: tauri::State<'_, App>, rescan: Option<bool>) -> Res {
    if rescan == Some(true) {
        sshconf::forget();
    }
    let l = app.local.get()?;
    let s = app.servers.get()?;
    let known: Vec<sdb::Server> = sdb::servers(&s).map_err(e)?;
    let hosts: Vec<Value> = sshconf::lookup_all(&sshconf::aliases())
        .into_iter()
        .map(|h| {
            let enabled = known.iter().find(|k| k.alias == h.alias).is_none_or(|k| k.enabled);
            json!({ "alias": h.alias, "address": h.address(), "via": h.proxy_jump, "enabled": enabled })
        })
        .collect();
    let size: u64 = [paths::local_db(), paths::servers_db()].iter().filter_map(|p| std::fs::metadata(p).ok()).map(|m| m.len()).sum();
    Ok(json!({
        "ssh": ssh_on(&s),
        "ssh_config": paths::ssh_dir().join("config").display().to_string(),
        "hosts": hosts,
        "autostart": autostart::is_enabled(),
        "retention": ldb::setting_i64(&l, ldb::KEY_RETENTION, ldb::DEFAULT_RETENTION),
        "theme": ldb::get_setting(&l, "theme").ok().flatten().unwrap_or_else(|| "system".into()),
        "data_dir": paths::data_dir().display().to_string(),
        "size": size,
        "imported": { "jperformance": paths::import_result("JPerformance"), "jserver": paths::import_result("JServer") },
        "version": update::CURRENT,
        "recorder": collector::is_running(),
    }))
}

#[tauri::command]
async fn set_ssh(app: tauri::State<'_, App>, on: bool) -> Res {
    app.findings.lock().unwrap().clear();
    let s = app.servers.get()?;
    sdb::set_setting(&s, sdb::KEY_MONITORING, if on { "1" } else { "0" }).map_err(e)?;
    if on {
        sdb::ensure_servers(&s, &sshconf::aliases()).map_err(e)?;
    }
    let _ = collector::spawn_detached();
    Ok(Value::Null)
}

#[tauri::command]
async fn set_autostart(on: bool) -> Res {
    autostart::set_enabled(on).map_err(|err| format!("Could not change Start with Windows: {err}"))?;
    Ok(json!(autostart::is_enabled()))
}

#[tauri::command]
async fn set_retention(app: tauri::State<'_, App>, days: i64) -> Res {
    let days = days.clamp(1, 365).to_string();
    ldb::set_setting(&*app.local.get()?, ldb::KEY_RETENTION, &days).map_err(e)?;
    sdb::set_setting(&*app.servers.get()?, sdb::KEY_RETENTION, &days).map_err(e)?;
    Ok(Value::Null)
}

#[tauri::command]
async fn set_theme(app: tauri::State<'_, App>, theme: String) -> Res {
    let theme = if matches!(theme.as_str(), "light" | "dark") { theme } else { "system".into() };
    ldb::set_setting(&*app.local.get()?, "theme", &theme).map_err(e)?;
    Ok(Value::Null)
}

#[tauri::command]
async fn start_recorder() -> Res {
    collector::spawn_detached().map_err(|err| format!("Could not start the recorder: {err}"))?;
    Ok(Value::Null)
}

fn update_json(s: update::State) -> Value {
    match s {
        update::State::Idle => json!({ "state": "idle" }),
        update::State::Checking => json!({ "state": "checking" }),
        update::State::UpToDate => json!({ "state": "up_to_date" }),
        update::State::Available(r) => json!({ "state": "available", "version": r.version, "notes": r.notes, "page": r.page }),
        update::State::Downloading(done, total) => json!({ "state": "downloading", "done": done, "total": total }),
        update::State::Restarting => json!({ "state": "restarting" }),
        update::State::Failed(m) => json!({ "state": "failed", "message": m }),
    }
}

#[tauri::command]
async fn update_state(app: tauri::State<'_, App>) -> Res {
    Ok(update_json(app.updater.state()))
}

#[tauri::command]
async fn check_update(app: tauri::State<'_, App>) -> Res {
    app.updater.check();
    Ok(update_json(app.updater.state()))
}

#[tauri::command]
async fn install_update(app: tauri::State<'_, App>) -> Res {
    app.updater.install();
    Ok(update_json(app.updater.state()))
}

/// The page has drawn its first view; show the window (it starts hidden to avoid a blank flash).
#[tauri::command]
async fn ready(app: tauri::State<'_, App>, window: tauri::WebviewWindow) -> Res {
    if !app.smoke {
        let _ = window.show();
        let _ = window.set_focus();
    }
    // JPU_SMOKE_HOLD=<port> keeps a hidden smoke window idle so a test can drive it over the WebView2 debug port.
    Ok(json!({ "smoke": app.smoke, "hold": app.smoke && std::env::var_os("JPU_SMOKE_HOLD").is_some() }))
}

#[tauri::command]
async fn smoke_report(app: tauri::State<'_, App>, handle: tauri::AppHandle, report: String) -> Res {
    if app.smoke {
        std::fs::write(paths::data_dir().join("smoke.json"), report).map_err(e)?;
        handle.exit(0);
    }
    Ok(Value::Null)
}

#[tauri::command]
fn quit(handle: tauri::AppHandle) {
    handle.exit(0);
}

pub fn run(smoke: bool) -> Result<(), String> {
    let local = ldb::open(&paths::local_db()).map_err(e)?;
    let servers = sdb::open(&paths::servers_db()).map_err(e)?;
    let state = App {
        smoke,
        local: Pool::new(paths::local_db(), local),
        servers: Pool::new(paths::servers_db(), servers),
        updater: update::Updater::default(),
        findings: Mutex::default(),
    };
    let mut builder = tauri::Builder::default();
    // A hidden smoke run must not hand off to (or block) the window the user has open.
    if !smoke {
        builder = builder.plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.unminimize();
                let _ = w.show();
                let _ = w.set_focus();
            }
        }));
    }
    builder
        .manage(state)
        .setup(move |app| {
            let mut window = tauri::WebviewWindowBuilder::from_config(app.handle(), &app.config().app.windows[0])?;
            // A held smoke run opens the WebView2 debug port so a test can click through the hidden window.
            if let Some(port) = std::env::var("JPU_SMOKE_HOLD").ok().filter(|_| smoke) {
                window = window.additional_browser_args(&format!("--remote-debugging-port={port}"));
            }
            window.build()?;
            // Show the window even if the page never calls ready(); a smoke run gives up after 3 minutes instead.
            let handle = app.handle().clone();
            std::thread::spawn(move || {
                if smoke {
                    std::thread::sleep(std::time::Duration::from_secs(180));
                    let _ = std::fs::write(paths::data_dir().join("smoke.json"), r#"{"error":"timed out"}"#);
                    handle.exit(2);
                    return;
                }
                std::thread::sleep(std::time::Duration::from_secs(4));
                if let Some(w) = handle.get_webview_window("main") {
                    let _ = w.show();
                }
            });
            // Close as soon as an update has started the new version, so it can take over.
            let handle = app.handle().clone();
            std::thread::spawn(move || {
                loop {
                    std::thread::sleep(std::time::Duration::from_millis(300));
                    if matches!(handle.state::<App>().updater.state(), update::State::Restarting) {
                        handle.exit(0);
                        return;
                    }
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            overview,
            sidebar,
            pc_now,
            pc_history,
            pc_moment,
            pc_apps,
            stop_app,
            recordings,
            recording_series,
            start_recording,
            stop_recording,
            delete_recording,
            findings,
            server_detail,
            server_history,
            server_moment,
            server_retry,
            server_watch,
            ssh_keys,
            settings,
            set_ssh,
            set_autostart,
            set_retention,
            set_theme,
            start_recorder,
            update_state,
            check_update,
            install_update,
            ready,
            smoke_report,
            quit,
        ])
        .run(tauri::generate_context!())
        .map_err(e)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chart_buckets_keep_about_150_points() {
        for (range, bucket) in [(3_600, 30), (21_600, 120), (86_400, 600), (604_800, 3_600), (2_592_000, 14_400)] {
            assert_eq!(bucket_for(range), bucket);
            let points = range / bucket;
            assert!((100..=200).contains(&points), "{range}: {points} points");
        }
    }

    #[test]
    fn only_login_problems_count_as_failed() {
        for s in ["timeout", "hostkey", "denied", "refused", "dns", "down", "lost", "error"] {
            assert!(failed(s), "{s}");
        }
        for s in ["up", "connecting", "paused", ""] {
            assert!(!failed(s), "{s}");
        }
    }
}
