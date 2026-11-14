use rusqlite::{Connection, OptionalExtension, params};
use std::path::Path;

pub type Result<T> = rusqlite::Result<T>;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS system_samples (
    ts          INTEGER PRIMARY KEY,
    cpu         REAL    NOT NULL,
    mem_used    INTEGER NOT NULL,
    mem_total   INTEGER NOT NULL,
    io_read     INTEGER NOT NULL,
    io_write    INTEGER NOT NULL,
    net_rx      INTEGER NOT NULL,
    net_tx      INTEGER NOT NULL,
    interval_ms INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS app_minutes (
    minute   INTEGER NOT NULL,
    app      TEXT    NOT NULL,
    cpu_avg  REAL    NOT NULL,
    cpu_max  REAL    NOT NULL,
    mem_avg  INTEGER NOT NULL,
    mem_max  INTEGER NOT NULL,
    io_bytes INTEGER NOT NULL,
    procs    INTEGER NOT NULL,
    PRIMARY KEY (minute, app)
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS app_minutes_app ON app_minutes(app, minute);
CREATE TABLE IF NOT EXISTS apps (
    app         TEXT PRIMARY KEY,
    exe_path    TEXT,
    description TEXT,
    last_seen   INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS live_apps (
    app      TEXT PRIMARY KEY,
    cpu      REAL    NOT NULL,
    mem      INTEGER NOT NULL,
    io_rate  REAL    NOT NULL,
    procs    INTEGER NOT NULL,
    ts       INTEGER NOT NULL,
    last_active INTEGER NOT NULL DEFAULT 0,
    titles   TEXT,
    private  INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS sessions (
    id         INTEGER PRIMARY KEY,
    app        TEXT    NOT NULL,
    started    INTEGER NOT NULL,
    ended      INTEGER,
    end_reason TEXT,
    planned_end INTEGER
);
CREATE TABLE IF NOT EXISTS session_samples (
    session_id INTEGER NOT NULL,
    ts         INTEGER NOT NULL,
    cpu        REAL    NOT NULL,
    mem        INTEGER NOT NULL,
    io_rate    REAL    NOT NULL,
    procs      INTEGER NOT NULL,
    PRIMARY KEY (session_id, ts)
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS proc_activity (
    pid       INTEGER NOT NULL,
    started   INTEGER NOT NULL,
    active_at INTEGER NOT NULL,
    PRIMARY KEY (pid, started)
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS cleanup_log (
    id     INTEGER PRIMARY KEY,
    ts     INTEGER NOT NULL,
    name   TEXT    NOT NULL,
    procs  INTEGER NOT NULL,
    mem    INTEGER NOT NULL,
    reason TEXT    NOT NULL,
    auto   INTEGER NOT NULL,
    run_id INTEGER
);
CREATE INDEX IF NOT EXISTS cleanup_log_reason ON cleanup_log(reason, name, ts);
CREATE TABLE IF NOT EXISTS ram_runs (
    id        INTEGER PRIMARY KEY,
    ts        INTEGER NOT NULL,
    mem_used  INTEGER NOT NULL,
    mem_total INTEGER NOT NULL,
    mem_after INTEGER NOT NULL,
    level     INTEGER NOT NULL,
    apps      INTEGER NOT NULL,
    procs     INTEGER NOT NULL,
    freed     INTEGER NOT NULL,
    auto      INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS ram_runs_ts ON ram_runs(ts);
CREATE TABLE IF NOT EXISTS leak_finds (
    app      TEXT PRIMARY KEY,
    first_ts INTEGER NOT NULL,
    last_ts  INTEGER NOT NULL,
    gone_ts  INTEGER NOT NULL DEFAULT 0,
    from_mem INTEGER NOT NULL,
    peak_mem INTEGER NOT NULL,
    rate     REAL    NOT NULL,
    hours    REAL    NOT NULL,
    cpu      REAL    NOT NULL,
    series   TEXT,
    resolved INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS storage_cleans (
    id    INTEGER PRIMARY KEY,
    ts    INTEGER NOT NULL,
    what  TEXT    NOT NULL,
    freed INTEGER NOT NULL,
    files INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS collector_status (
    id        INTEGER PRIMARY KEY CHECK (id = 1),
    pid       INTEGER NOT NULL,
    started   INTEGER NOT NULL,
    heartbeat INTEGER NOT NULL
);
"#;

pub fn open(path: &Path) -> Result<Connection> {
    let conn = Connection::open(path)?;
    conn.busy_timeout(std::time::Duration::from_secs(15))?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.execute_batch(SCHEMA)?;
    migrate(&conn)?;
    Ok(conn)
}

/// Adds columns introduced after the first release.
fn migrate(conn: &Connection) -> Result<()> {
    let cols: Vec<String> = conn
        .prepare("SELECT name FROM pragma_table_info('system_samples')")?
        .query_map([], |r| r.get(0))?
        .collect::<Result<_>>()?;
    // live_apps only holds the latest sample, so it can be rebuilt freely.
    let live_ok = conn
        .prepare("SELECT 1 FROM pragma_table_info('live_apps') WHERE name = 'private'")?
        .exists([])?;
    if !live_ok {
        conn.execute_batch(
            "DROP TABLE live_apps;
             CREATE TABLE live_apps (
                app TEXT PRIMARY KEY, cpu REAL NOT NULL, mem INTEGER NOT NULL, io_rate REAL NOT NULL,
                procs INTEGER NOT NULL, ts INTEGER NOT NULL, last_active INTEGER NOT NULL DEFAULT 0, titles TEXT,
                private INTEGER NOT NULL DEFAULT 0);",
        )?;
    }
    let has_title = conn
        .prepare("SELECT 1 FROM pragma_table_info('apps') WHERE name = 'window_title'")?
        .exists([])?;
    if !has_title {
        conn.execute_batch("ALTER TABLE apps ADD COLUMN window_title TEXT")?;
    }
    // Apps that run on a shared runtime (Electron, Java, Python...) all have the
    // runtime's description, so their taskbar window title names them better.
    conn.execute_batch(
        "DROP VIEW IF EXISTS app_names;
         CREATE VIEW app_names AS
         SELECT app, exe_path, window_title,
                CASE WHEN window_title IS NOT NULL AND window_title <> ''
                          AND (description IS NULL OR lower(description) IN ('electron', 'java(tm) platform se binary', 'openjdk platform binary', 'java', 'javaw', 'python', 'pythonw', 'node.js', 'node.js javascript runtime', 'nw.js', 'chromium', 'microsoft edge webview2', 'tauri app', 'cefsharp.browsersubprocess', 'qtwebengineprocess'))
                     THEN window_title ELSE description END AS description
         FROM apps;",
    )?;
    let has_planned = conn
        .prepare("SELECT 1 FROM pragma_table_info('sessions') WHERE name = 'planned_end'")?
        .exists([])?;
    if !has_planned {
        conn.execute_batch("ALTER TABLE sessions ADD COLUMN planned_end INTEGER")?;
    }
    let has_run_id = conn
        .prepare("SELECT 1 FROM pragma_table_info('cleanup_log') WHERE name = 'run_id'")?
        .exists([])?;
    if !has_run_id {
        conn.execute_batch("ALTER TABLE cleanup_log ADD COLUMN run_id INTEGER")?;
    }
    // Before 1.2.0 the RAM cleaner wrote its passes only to cleanup_log. Turn each of those passes into
    // a ram_runs row, without a memory reading (0), so the page's totals and its per-app list agree.
    conn.execute_batch(
        "INSERT INTO ram_runs(ts, mem_used, mem_total, mem_after, level, apps, procs, freed, auto)
         SELECT ts, 0, 0, 0, 0, COUNT(*), SUM(procs), SUM(mem), MAX(auto)
         FROM cleanup_log WHERE reason = 'Freed memory while idle'
         GROUP BY ts HAVING ts NOT IN (SELECT ts FROM ram_runs);
         UPDATE cleanup_log SET run_id = (SELECT MIN(r.id) FROM ram_runs r WHERE r.ts = cleanup_log.ts)
         WHERE reason = 'Freed memory while idle' AND run_id IS NULL
           AND ts IN (SELECT ts FROM ram_runs);",
    )?;
    for col in ["proc_count", "app_count", "commit_used", "commit_total"] {
        if !cols.iter().any(|c| c == col) {
            conn.execute_batch(&format!(
                "ALTER TABLE system_samples ADD COLUMN {col} INTEGER NOT NULL DEFAULT 0"
            ))?;
        }
    }
    Ok(())
}

// ---------- settings ----------

pub fn get_setting(conn: &Connection, key: &str) -> Result<Option<String>> {
    conn.query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| r.get(0))
        .optional()
}

pub fn set_setting(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO settings(key, value) VALUES(?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )?;
    Ok(())
}

pub fn setting_i64(conn: &Connection, key: &str, default: i64) -> i64 {
    get_setting(conn, key)
        .ok()
        .flatten()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

pub const KEY_TRACKING: &str = "tracking_enabled";
pub const KEY_INTERVAL: &str = "sample_interval_secs";
pub const KEY_RETENTION: &str = "retention_days";

pub const KEY_AUTO_CLEANUP: &str = "auto_cleanup";
pub const KEY_AUTO_CLEANUP_MIN: &str = "auto_cleanup_minutes";
pub const DEFAULT_AUTO_CLEANUP_MIN: i64 = 15;

/// Background RAM cleaner: frees the memory of idle apps when memory in use reaches a level. On by default.
pub const KEY_RAM_CLEAN: &str = "ram_clean";
pub const KEY_RAM_CLEAN_PCT: &str = "ram_clean_pct";
pub const DEFAULT_RAM_CLEAN_PCT: i64 = 85;
/// Reason written to the clean-up log for memory the RAM cleaner freed.
pub const RAM_CLEAN_REASON: &str = "Freed memory while idle";
/// How many days of RAM cleaner passes the RAM page keeps.
pub const RAM_RUN_HISTORY_DAYS: i64 = 30;
/// When the leak finder last looked, so the page can say that instead of the newest leak's time.
pub const KEY_LEAKS_CHECKED: &str = "leaks_checked";

/// Developer option: update checks go through the user's signed-in GitHub CLI. Off by default.
pub const KEY_USE_GH: &str = "update_use_gh";

pub const DEFAULT_INTERVAL: i64 = 5;
pub const DEFAULT_RETENTION: i64 = 30;

pub fn tracking_enabled(conn: &Connection) -> bool {
    setting_i64(conn, KEY_TRACKING, 1) != 0
}

// ---------- collector writes ----------

pub struct SystemSample {
    /// Memory promised to programs (RAM plus page file) and the most Windows can promise.
    pub commit_used: u64,
    pub commit_total: u64,
    pub ts: i64,
    pub cpu: f64,
    pub mem_used: u64,
    pub mem_total: u64,
    pub io_read: u64,
    pub io_write: u64,
    pub net_rx: u64,
    pub net_tx: u64,
    pub interval_ms: i64,
    pub proc_count: i64,
    pub app_count: i64,
}

pub struct AppMinute {
    pub app: String,
    pub cpu_avg: f64,
    pub cpu_max: f64,
    pub mem_avg: u64,
    pub mem_max: u64,
    pub io_bytes: u64,
    pub procs: i64,
}

pub struct LiveApp {
    pub app: String,
    pub cpu: f64,
    pub mem: u64,
    pub io_rate: f64,
    pub procs: i64,
    /// Taskbar window titles, joined with " | ".
    pub titles: Option<String>,
    /// Memory the app has been promised and cannot be shared (private bytes).
    pub private: u64,
}

pub fn write_sample(conn: &mut Connection, s: &SystemSample, live: &[LiveApp]) -> Result<()> {
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    tx.execute(
        "INSERT OR REPLACE INTO system_samples
         (ts, cpu, mem_used, mem_total, io_read, io_write, net_rx, net_tx, interval_ms, proc_count, app_count, commit_used, commit_total)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        params![
            s.ts,
            s.cpu,
            s.mem_used as i64,
            s.mem_total as i64,
            s.io_read as i64,
            s.io_write as i64,
            s.net_rx as i64,
            s.net_tx as i64,
            s.interval_ms,
            s.proc_count,
            s.app_count,
            s.commit_used as i64,
            s.commit_total as i64
        ],
    )?;
    tx.execute("DELETE FROM live_apps", [])?;
    {
        let mut stmt = tx.prepare(
            "INSERT INTO live_apps(app, cpu, mem, io_rate, procs, ts, titles, private) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        )?;
        for a in live {
            stmt.execute(params![a.app, a.cpu, a.mem as i64, a.io_rate, a.procs, s.ts, a.titles, a.private as i64])?;
        }
    }
    tx.execute(
        "UPDATE collector_status SET heartbeat = ?1 WHERE id = 1",
        [s.ts],
    )?;
    tx.commit()
}

pub fn write_minute(conn: &mut Connection, minute: i64, rows: &[AppMinute]) -> Result<()> {
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    {
        let mut stmt = tx.prepare(
            "INSERT OR REPLACE INTO app_minutes
             (minute, app, cpu_avg, cpu_max, mem_avg, mem_max, io_bytes, procs)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        )?;
        for r in rows {
            stmt.execute(params![
                minute,
                r.app,
                r.cpu_avg,
                r.cpu_max,
                r.mem_avg as i64,
                r.mem_max as i64,
                r.io_bytes as i64,
                r.procs
            ])?;
        }
    }
    tx.commit()
}

pub fn upsert_app(conn: &Connection, app: &str, exe: Option<&str>, desc: Option<&str>, ts: i64) -> Result<()> {
    conn.execute(
        "INSERT INTO apps(app, exe_path, description, last_seen) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(app) DO UPDATE SET
            exe_path = COALESCE(excluded.exe_path, apps.exe_path),
            description = COALESCE(excluded.description, apps.description),
            last_seen = excluded.last_seen",
        params![app, exe, desc, ts],
    )?;
    Ok(())
}

pub fn set_window_title(conn: &Connection, app: &str, title: &str) -> Result<()> {
    conn.execute("UPDATE apps SET window_title = ?2 WHERE app = ?1", params![app, title])?;
    Ok(())
}

pub fn mark_collector_started(conn: &Connection, pid: u32, ts: i64) -> Result<()> {
    conn.execute(
        "INSERT INTO collector_status(id, pid, started, heartbeat) VALUES (1, ?1, ?2, ?2)
         ON CONFLICT(id) DO UPDATE SET pid = excluded.pid, started = excluded.started, heartbeat = excluded.heartbeat",
        params![pid as i64, ts],
    )?;
    Ok(())
}

pub fn prune(conn: &Connection, older_than: i64) -> Result<usize> {
    let a = conn.execute("DELETE FROM system_samples WHERE ts < ?1", [older_than])?;
    let b = conn.execute("DELETE FROM app_minutes WHERE minute < ?1", [older_than])?;
    let c = conn.execute("DELETE FROM cleanup_log WHERE ts < ?1", [older_than])?;
    let d = conn.execute("DELETE FROM ram_runs WHERE ts < ?1", [older_than])?;
    // A leak that is still growing is kept however old it is; one that stopped is history.
    let e = conn.execute("DELETE FROM leak_finds WHERE resolved = 1 AND gone_ts < ?1", [older_than])?;
    Ok(a + b + c + d + e)
}

// ---------- process activity and clean-up ----------

/// Replaces the table of when each running process last used the CPU.
pub fn write_activity(conn: &mut Connection, rows: &[(u32, i64, i64)]) -> Result<()> {
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    tx.execute("DELETE FROM proc_activity", [])?;
    {
        let mut stmt = tx.prepare("INSERT OR REPLACE INTO proc_activity(pid, started, active_at) VALUES (?1, ?2, ?3)")?;
        for (pid, started, at) in rows {
            stmt.execute(params![pid, started, at])?;
        }
    }
    tx.commit()
}

/// When each process (pid, start time) last used the CPU.
pub fn activity(conn: &Connection) -> Result<std::collections::HashMap<(u32, i64), i64>> {
    let mut stmt = conn.prepare("SELECT pid, started, active_at FROM proc_activity")?;
    let rows = stmt.query_map([], |r| Ok(((r.get::<_, u32>(0)?, r.get::<_, i64>(1)?), r.get::<_, i64>(2)?)))?;
    rows.collect()
}

pub fn log_cleanup(conn: &Connection, ts: i64, name: &str, procs: usize, mem: u64, reason: &str, auto: bool) -> Result<()> {
    conn.execute(
        "INSERT INTO cleanup_log(ts, name, procs, mem, reason, auto) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![ts, name, procs as i64, mem as i64, reason, auto as i64],
    )?;
    Ok(())
}

#[derive(serde::Serialize)]
pub struct CleanupEntry {
    pub ts: i64,
    pub name: String,
    pub procs: i64,
    pub mem: i64,
    pub reason: String,
    pub auto: bool,
}

pub fn cleanup_log(conn: &Connection, limit: usize) -> Result<Vec<CleanupEntry>> {
    let mut stmt = conn.prepare("SELECT ts, name, procs, mem, reason, auto FROM cleanup_log ORDER BY ts DESC, id DESC LIMIT ?1")?;
    let rows = stmt.query_map([limit as i64], |r| {
        Ok(CleanupEntry { ts: r.get(0)?, name: r.get(1)?, procs: r.get(2)?, mem: r.get(3)?, reason: r.get(4)?, auto: r.get::<_, i64>(5)? != 0 })
    })?;
    rows.collect()
}

// ---------- RAM cleaner passes and memory leaks ----------

/// One pass of the RAM cleaner: memory in use before and after, and what moved out of RAM.
#[derive(serde::Serialize, Clone, Debug)]
pub struct RamRun {
    pub ts: i64,
    pub mem_used: i64,
    pub mem_total: i64,
    pub mem_after: i64,
    pub apps: i64,
    pub procs: i64,
    pub freed: i64,
    pub auto: bool,
    /// What was trimmed in this pass, biggest first.
    pub items: Vec<RamRunItem>,
}

#[derive(serde::Serialize, Clone, Debug)]
pub struct RamRunItem {
    pub app: String,
    pub procs: i64,
    pub mem: i64,
}

/// Records a pass of the RAM cleaner and returns its id.
#[allow(clippy::too_many_arguments)]
pub fn log_ram_run(conn: &Connection, ts: i64, mem_used: u64, mem_total: u64, mem_after: u64, level: i64, apps: usize, procs: usize, freed: u64, auto: bool) -> Result<i64> {
    conn.execute(
        "INSERT INTO ram_runs(ts, mem_used, mem_total, mem_after, level, apps, procs, freed, auto) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![ts, mem_used as i64, mem_total as i64, mem_after as i64, level, apps as i64, procs as i64, freed as i64, auto as i64],
    )?;
    Ok(conn.last_insert_rowid())
}

/// The last `limit` RAM cleaner passes, newest first, with the apps each one trimmed.
pub fn ram_runs(conn: &Connection, limit: usize, since: i64) -> Result<Vec<RamRun>> {
    let mut stmt = conn.prepare(
        "SELECT id, ts, mem_used, mem_total, mem_after, apps, procs, freed, auto FROM ram_runs
         WHERE ts >= ?2 ORDER BY ts DESC LIMIT ?1",
    )?;
    let mut runs: Vec<(i64, RamRun)> = stmt
        .query_map(params![limit as i64, since], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                RamRun {
                    ts: r.get(1)?,
                    mem_used: r.get(2)?,
                    mem_total: r.get(3)?,
                    mem_after: r.get(4)?,
                    apps: r.get(5)?,
                    procs: r.get(6)?,
                    freed: r.get(7)?,
                    auto: r.get::<_, i64>(8)? != 0,
                    items: Vec::new(),
                },
            ))
        })?
        .collect::<Result<_>>()?;
    // Each pass writes its own rows with the id of the pass, so two passes in the same second stay apart.
    let mut items: std::collections::HashMap<i64, Vec<RamRunItem>> = std::collections::HashMap::new();
    let mut stmt = conn.prepare(
        "SELECT l.run_id, l.name, l.procs, l.mem FROM cleanup_log l JOIN ram_runs r ON r.id = l.run_id
         WHERE l.reason = ?1 AND r.ts >= (SELECT COALESCE(MIN(ts), 0) FROM (SELECT ts FROM ram_runs WHERE ts >= ?3 ORDER BY ts DESC LIMIT ?2))
         ORDER BY l.mem DESC",
    )?;
    let rows = stmt.query_map(params![RAM_CLEAN_REASON, limit as i64, since], |r| {
        Ok((r.get::<_, i64>(0)?, RamRunItem { app: r.get(1)?, procs: r.get(2)?, mem: r.get(3)? }))
    })?;
    for row in rows {
        let (id, item) = row?;
        items.entry(id).or_default().push(item);
    }
    for (id, run) in &mut runs {
        run.items = items.remove(id).unwrap_or_default();
    }
    Ok(runs.into_iter().map(|(_, run)| run).collect())
}

/// All the memory the RAM cleaner has freed since `since`: (bytes, how many passes, when the last one was).
pub fn ram_run_totals(conn: &Connection, since: i64) -> Result<(i64, i64, Option<i64>)> {
    conn.query_row("SELECT COALESCE(SUM(freed), 0), COUNT(*), MAX(ts) FROM ram_runs WHERE ts >= ?1", [since], |r| {
        Ok((r.get(0)?, r.get(1)?, r.get(2)?))
    })
}

/// Notes one app's share of a pass, linked to the pass it belongs to.
pub fn log_ram_item(conn: &Connection, run_id: i64, ts: i64, name: &str, procs: usize, mem: u64, auto: bool) -> Result<()> {
    conn.execute(
        "INSERT INTO cleanup_log(ts, name, procs, mem, reason, auto, run_id) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![ts, name, procs as i64, mem as i64, RAM_CLEAN_REASON, auto as i64, run_id],
    )?;
    Ok(())
}

/// What the RAM cleaner freed per app, biggest first: (app, bytes, times, when last).
pub fn ram_freed_by_app(conn: &Connection, since: i64) -> Result<Vec<(String, i64, i64, i64)>> {
    let mut stmt = conn.prepare(
        "SELECT name, SUM(mem), COUNT(*), MAX(ts) FROM cleanup_log WHERE reason = ?1 AND ts >= ?2 GROUP BY name ORDER BY SUM(mem) DESC LIMIT 200",
    )?;
    let rows = stmt.query_map(params![RAM_CLEAN_REASON, since], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
    rows.collect()
}

/// An app whose memory kept climbing and never came back down.
#[derive(serde::Serialize, Clone, Debug)]
pub struct Leak {
    pub app: String,
    pub name: String,
    /// When this climb started, and the last reading that was still climbing.
    pub first_ts: i64,
    pub last_ts: i64,
    /// Memory it had when this climb started, and the most it has reached since.
    pub from_mem: i64,
    pub peak_mem: i64,
    /// Bytes an hour it was gaining, and how long it was watched.
    pub rate: f64,
    pub hours: f64,
    /// Average CPU while it grew: low means the growth was not work.
    pub cpu: f64,
    /// Its memory every few minutes, for the chart on the page.
    pub series: Vec<(i64, i64)>,
    /// True once it stopped climbing (it was closed, or it let the memory go), and when that was.
    pub resolved: bool,
    pub gone_ts: i64,
    /// The last time memory was freed from it by the RAM cleaner.
    pub freed_at: Option<i64>,
    pub freed: i64,
}

pub struct LeakRow {
    pub app: String,
    pub first_ts: i64,
    pub last_ts: i64,
    pub from_mem: i64,
    pub peak_mem: i64,
    pub rate: f64,
    pub hours: f64,
    pub cpu: f64,
    pub series: String,
}

/// Writes the climb the detector found. A row always describes the climb happening now: when it started,
/// where it started, the most it has reached, and its series, so nothing on the page mixes two of them.
pub fn upsert_leak(conn: &Connection, l: &LeakRow) -> Result<()> {
    conn.execute(
        "INSERT INTO leak_finds(app, first_ts, last_ts, gone_ts, from_mem, peak_mem, rate, hours, cpu, series, resolved)
         VALUES (?1, ?2, ?3, 0, ?4, ?5, ?6, ?7, ?8, ?9, 0)
         ON CONFLICT(app) DO UPDATE SET first_ts = excluded.first_ts, last_ts = excluded.last_ts, gone_ts = 0,
             from_mem = excluded.from_mem, peak_mem = excluded.peak_mem,
             rate = excluded.rate, hours = excluded.hours, cpu = excluded.cpu, series = excluded.series, resolved = 0",
        params![l.app, l.first_ts, l.last_ts, l.from_mem, l.peak_mem, l.rate, l.hours, l.cpu, l.series],
    )?;
    Ok(())
}

/// Marks a leak as no longer growing, keeping when it stopped.
pub fn resolve_leak(conn: &Connection, app: &str, ts: i64) -> Result<()> {
    conn.execute("UPDATE leak_finds SET resolved = 1, gone_ts = ?2 WHERE app = ?1 AND resolved = 0", params![app, ts])?;
    Ok(())
}

/// Apps still listed as leaking, to check again for one that has stopped.
pub fn unresolved_leaks(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn.prepare("SELECT app FROM leak_finds WHERE resolved = 0")?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
    rows.collect()
}

/// The leaks found, still growing first, with what the cleaner freed from each since `since`.
pub fn leaks(conn: &Connection, since: i64) -> Result<Vec<Leak>> {
    let mut stmt = conn.prepare(
        "SELECT l.app, l.first_ts, l.last_ts, l.from_mem, l.peak_mem, l.rate, l.hours, l.cpu, l.series, l.resolved, l.gone_ts,
                (SELECT MAX(ts) FROM cleanup_log WHERE reason = ?1 AND name = l.app AND ts >= MAX(?2, l.first_ts)),
                (SELECT COALESCE(SUM(mem), 0) FROM cleanup_log WHERE reason = ?1 AND name = l.app AND ts >= MAX(?2, l.first_ts))
         FROM leak_finds l ORDER BY l.resolved",
    )?;
    let mut out: Vec<Leak> = stmt.query_map(params![RAM_CLEAN_REASON, since], |r| {
        let series: Option<String> = r.get(8)?;
        // The page names each app from its description; this is the program file until it does.
        let app: String = r.get(0)?;
        let (first_ts, last_ts): (i64, i64) = (r.get(1)?, r.get(2)?);
        let (from_mem, peak_mem): (i64, i64) = (r.get(3)?, r.get(4)?);
        // Worked out from the stored window, so a row written by several detections stays consistent.
        let hours = ((last_ts - first_ts) as f64 / 3600.0).max(0.01);
        Ok(Leak {
            name: app.clone(),
            app,
            first_ts,
            last_ts,
            from_mem,
            peak_mem,
            rate: (peak_mem - from_mem) as f64 / hours,
            hours,
            cpu: r.get(7)?,
            series: series.and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default(),
            resolved: r.get::<_, i64>(9)? != 0,
            gone_ts: r.get(10)?,
            freed_at: r.get(11)?,
            freed: r.get(12)?,
        })
    })?
    .collect::<Result<_>>()?;
    // Still growing first, then the fastest: the same numbers the page shows.
    out.sort_by(|a, b| a.resolved.cmp(&b.resolved).then(b.rate.total_cmp(&a.rate)));
    Ok(out)
}

/// Number of leaks still growing, for the sidebar.
pub fn leak_count(conn: &Connection) -> Result<i64> {
    conn.query_row("SELECT COUNT(*) FROM leak_finds WHERE resolved = 0", [], |r| r.get(0))
}

/// Notes disk space freed by clearing caches or emptying the Recycle Bin.
pub fn log_storage_clean(conn: &Connection, ts: i64, what: &str, freed: u64, files: u64) -> Result<()> {
    conn.execute("INSERT INTO storage_cleans(ts, what, freed, files) VALUES (?1, ?2, ?3, ?4)", params![ts, what, freed as i64, files as i64])?;
    Ok(())
}

/// Disk space freed so far: (bytes in all, how many times, when last, what last, bytes last).
pub fn storage_clean_stats(conn: &Connection) -> Result<(i64, i64, Option<(i64, String, i64)>)> {
    let (total, n): (i64, i64) = conn.query_row("SELECT COALESCE(SUM(freed), 0), COUNT(*) FROM storage_cleans", [], |r| Ok((r.get(0)?, r.get(1)?)))?;
    let last = conn
        .query_row("SELECT ts, what, freed FROM storage_cleans ORDER BY ts DESC, id DESC LIMIT 1", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .optional()?;
    Ok((total, n, last))
}

// ---------- reads ----------

#[derive(serde::Serialize, Clone, Debug)]
pub struct SysPoint {
    pub ts: i64,
    pub cpu: f64,
    pub cpu_max: f64,
    pub mem_pct: f64,
    pub mem_used: f64,
    /// bytes per second
    pub io_rate: f64,
    /// bytes per second
    pub net_rate: f64,
    /// processes running; 0 when not recorded
    pub procs: f64,
    /// distinct apps running; 0 when not recorded
    pub apps: f64,
    /// share of the commit limit in use; 0 when not recorded
    pub commit_pct: f64,
}

/// System samples between `from` and `to`, averaged into `bucket`-second buckets.
pub fn system_series(conn: &Connection, from: i64, to: i64, bucket: i64) -> Result<Vec<SysPoint>> {
    let bucket = bucket.max(1);
    let mut stmt = conn.prepare(
        "SELECT (ts / ?3) * ?3 AS b,
                AVG(cpu), MAX(cpu),
                AVG(100.0 * mem_used / mem_total), AVG(mem_used),
                1000.0 * SUM(io_read + io_write) / SUM(interval_ms),
                1000.0 * SUM(net_rx + net_tx) / SUM(interval_ms),
                COALESCE(AVG(NULLIF(proc_count, 0)), 0), COALESCE(AVG(NULLIF(app_count, 0)), 0),
                COALESCE(AVG(CASE WHEN commit_total > 0 THEN 100.0 * commit_used / commit_total END), 0)
         FROM system_samples
         WHERE ts >= ?1 AND ts <= ?2 AND mem_total > 0 AND interval_ms > 0
         GROUP BY b ORDER BY b",
    )?;
    let rows = stmt.query_map(params![from, to, bucket], |r| {
        Ok(SysPoint {
            ts: r.get(0)?,
            cpu: r.get(1)?,
            cpu_max: r.get(2)?,
            mem_pct: r.get(3)?,
            mem_used: r.get(4)?,
            io_rate: r.get(5)?,
            net_rate: r.get(6)?,
            procs: r.get(7)?,
            apps: r.get(8)?,
            commit_pct: r.get(9)?,
        })
    })?;
    rows.collect()
}

pub fn latest_sample(conn: &Connection) -> Result<Option<(SysPoint, u64)>> {
    conn.query_row(
        "SELECT ts, cpu, 100.0 * mem_used / mem_total, mem_used, mem_total,
                1000.0 * (io_read + io_write) / interval_ms,
                1000.0 * (net_rx + net_tx) / interval_ms, proc_count, app_count,
                CASE WHEN commit_total > 0 THEN 100.0 * commit_used / commit_total ELSE 0 END
         FROM system_samples WHERE mem_total > 0 AND interval_ms > 0
         ORDER BY ts DESC LIMIT 1",
        [],
        |r| {
            let cpu: f64 = r.get(1)?;
            let mem_used: i64 = r.get(3)?;
            let mem_total: i64 = r.get(4)?;
            Ok((
                SysPoint {
                    ts: r.get(0)?,
                    cpu,
                    cpu_max: cpu,
                    mem_pct: r.get(2)?,
                    mem_used: mem_used as f64,
                    io_rate: r.get(5)?,
                    net_rate: r.get(6)?,
                    procs: r.get::<_, i64>(7)? as f64,
                    apps: r.get::<_, i64>(8)? as f64,
                    commit_pct: r.get(9)?,
                },
                mem_total as u64,
            ))
        },
    )
    .optional()
}

#[derive(serde::Serialize, Clone, Debug)]
pub struct LiveRow {
    pub app: String,
    pub description: Option<String>,
    pub cpu: f64,
    pub mem: u64,
    pub io_rate: f64,
    pub procs: i64,
    pub exe_path: Option<String>,
    /// Taskbar window titles right now, joined with " | ".
    pub titles: Option<String>,
    pub private: u64,
}

/// Running apps with their raw file description (not the view's last-window-title swap); the window titles
/// open right now are in `titles`.
pub fn live_apps(conn: &Connection) -> Result<Vec<LiveRow>> {
    let mut stmt = conn.prepare(
        "SELECT l.app, a.description, l.cpu, l.mem, l.io_rate, l.procs, a.exe_path, l.titles, l.private
         FROM live_apps l LEFT JOIN apps a ON a.app = l.app
         ORDER BY l.cpu DESC, l.mem DESC",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(LiveRow {
            app: r.get(0)?,
            description: r.get(1)?,
            cpu: r.get(2)?,
            mem: r.get::<_, i64>(3)? as u64,
            io_rate: r.get(4)?,
            procs: r.get(5)?,
            exe_path: r.get(6)?,
            titles: r.get(7)?,
            private: r.get::<_, i64>(8)? as u64,
        })
    })?;
    rows.collect()
}

#[derive(serde::Serialize, Clone, Debug)]
pub struct AppSummary {
    pub app: String,
    pub description: Option<String>,
    /// average share of the whole machine's CPU over the minutes it was running
    pub cpu_avg: f64,
    pub cpu_peak: f64,
    /// CPU-minutes of one full machine, used to rank total cost
    pub cpu_load: f64,
    pub mem_avg: f64,
    pub mem_peak: u64,
    pub io_total: u64,
    pub minutes: i64,
    /// average number of processes while running
    pub procs: f64,
    /// last taskbar window title recorded for this program, if it ever had one
    #[serde(skip_serializing)]
    pub window_title: Option<String>,
}

/// Where an app's program file lives, when it has been seen running.
pub fn app_exe(conn: &Connection, app: &str) -> Result<Option<String>> {
    conn.query_row("SELECT exe_path FROM apps WHERE app = ?1", [app], |r| r.get(0))
        .optional()
        .map(Option::flatten)
}

/// The raw file description of one app (not the view's window-title swap).
pub fn app_description(conn: &Connection, app: &str) -> Result<Option<String>> {
    conn.query_row("SELECT description FROM apps WHERE app = ?1", params![app], |r| r.get(0))
        .optional()
        .map(Option::flatten)
}

/// Totals over a time range read the raw file description (not the app_names view, which swaps in a window
/// title for runtimes); describe::summary_name decides the name.
pub fn app_summaries(conn: &Connection, from: i64, to: i64) -> Result<Vec<AppSummary>> {
    let mut stmt = conn.prepare(
        "SELECT m.app, a.description, AVG(m.cpu_avg), MAX(m.cpu_max), SUM(m.cpu_avg) / 100.0,
                AVG(m.mem_avg), MAX(m.mem_max), SUM(m.io_bytes), COUNT(*), AVG(m.procs), a.window_title
         FROM app_minutes m LEFT JOIN apps a ON a.app = m.app
         WHERE m.minute >= ?1 AND m.minute <= ?2
         GROUP BY m.app
         ORDER BY SUM(m.cpu_avg) DESC",
    )?;
    let rows = stmt.query_map(params![from, to], |r| {
        Ok(AppSummary {
            app: r.get(0)?,
            description: r.get(1)?,
            cpu_avg: r.get(2)?,
            cpu_peak: r.get(3)?,
            cpu_load: r.get(4)?,
            mem_avg: r.get(5)?,
            mem_peak: r.get::<_, i64>(6)? as u64,
            io_total: r.get::<_, i64>(7)? as u64,
            minutes: r.get(8)?,
            procs: r.get(9)?,
            window_title: r.get(10)?,
        })
    })?;
    rows.collect()
}

#[derive(serde::Serialize, Clone, Debug)]
pub struct AppPoint {
    pub ts: i64,
    pub cpu: f64,
    pub mem: f64,
    /// bytes per second
    pub io_rate: f64,
    pub procs: f64,
}

pub fn app_series(conn: &Connection, app: &str, from: i64, to: i64, bucket: i64) -> Result<Vec<AppPoint>> {
    let bucket = bucket.max(60);
    let mut stmt = conn.prepare(
        "SELECT (minute / ?4) * ?4 AS b, AVG(cpu_avg), AVG(mem_avg),
                SUM(io_bytes) / (COUNT(*) * 60.0), AVG(procs)
         FROM app_minutes WHERE app = ?1 AND minute >= ?2 AND minute <= ?3
         GROUP BY b ORDER BY b",
    )?;
    let rows = stmt.query_map(params![app, from, to, bucket], |r| {
        Ok(AppPoint { ts: r.get(0)?, cpu: r.get(1)?, mem: r.get(2)?, io_rate: r.get(3)?, procs: r.get(4)? })
    })?;
    rows.collect()
}

/// Average CPU of one app by (weekday 0 = Sunday, local hour), counting minutes it was running.
pub fn weekday_hour_app_cpu(conn: &Connection, app: &str, from: i64, to: i64) -> Result<[[Option<f64>; 24]; 7]> {
    let mut grid = [[None; 24]; 7];
    let mut stmt = conn.prepare(
        "SELECT CAST(strftime('%w', minute, 'unixepoch', 'localtime') AS INTEGER) AS d,
                CAST(strftime('%H', minute, 'unixepoch', 'localtime') AS INTEGER) AS h,
                AVG(cpu_avg)
         FROM app_minutes WHERE app = ?1 AND minute >= ?2 AND minute <= ?3
         GROUP BY d, h",
    )?;
    let rows = stmt.query_map(params![app, from, to], |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, f64>(2)?))
    })?;
    for row in rows {
        let (d, h, v) = row?;
        if (0..7).contains(&d) && (0..24).contains(&h) {
            grid[d as usize][h as usize] = Some(v);
        }
    }
    Ok(grid)
}

/// Top apps by CPU between `from` and `to` (inclusive).
pub fn top_apps_in(conn: &Connection, from: i64, to: i64, n: usize) -> Result<Vec<(String, Option<String>, f64)>> {
    let mut stmt = conn.prepare(
        "SELECT m.app, a.description, SUM(m.cpu_avg) / MAX(1, (SELECT COUNT(DISTINCT minute) FROM app_minutes WHERE minute >= ?1 AND minute <= ?2))
         FROM app_minutes m LEFT JOIN apps a ON a.app = m.app
         WHERE m.minute >= ?1 AND m.minute <= ?2
         GROUP BY m.app ORDER BY SUM(m.cpu_avg) DESC LIMIT ?3",
    )?;
    let rows = stmt.query_map(params![from, to, n as i64], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
    rows.collect()
}

/// Top apps by memory between `from` and `to`.
pub fn top_mem_apps_in(conn: &Connection, from: i64, to: i64, n: usize) -> Result<Vec<(String, Option<String>, f64)>> {
    let mut stmt = conn.prepare(
        "SELECT m.app, a.description, AVG(m.mem_avg)
         FROM app_minutes m LEFT JOIN apps a ON a.app = m.app
         WHERE m.minute >= ?1 AND m.minute <= ?2
         GROUP BY m.app ORDER BY AVG(m.mem_avg) DESC LIMIT ?3",
    )?;
    let rows = stmt.query_map(params![from, to, n as i64], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
    rows.collect()
}

/// Average CPU by (weekday 0 = Sunday, local hour).
pub fn weekday_hour_cpu(conn: &Connection, from: i64, to: i64) -> Result<[[Option<f64>; 24]; 7]> {
    let mut grid = [[None; 24]; 7];
    let mut stmt = conn.prepare(
        "SELECT CAST(strftime('%w', ts, 'unixepoch', 'localtime') AS INTEGER) AS d,
                CAST(strftime('%H', ts, 'unixepoch', 'localtime') AS INTEGER) AS h,
                AVG(cpu)
         FROM system_samples WHERE ts >= ?1 AND ts <= ?2
         GROUP BY d, h",
    )?;
    let rows = stmt.query_map(params![from, to], |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, f64>(2)?))
    })?;
    for row in rows {
        let (d, h, v) = row?;
        if (0..7).contains(&d) && (0..24).contains(&h) {
            grid[d as usize][h as usize] = Some(v);
        }
    }
    Ok(grid)
}

/// Number of minutes with per-app data between `from` and `to`.
pub fn minutes_recorded(conn: &Connection, from: i64, to: i64) -> Result<i64> {
    conn.query_row(
        "SELECT COUNT(DISTINCT minute) FROM app_minutes WHERE minute >= ?1 AND minute <= ?2",
        params![from, to],
        |r| r.get(0),
    )
}

pub struct Coverage {
    pub first: Option<i64>,
    pub last: Option<i64>,
    pub samples: i64,
}

pub fn coverage(conn: &Connection, from: i64, to: i64) -> Result<Coverage> {
    conn.query_row(
        "SELECT MIN(ts), MAX(ts), COUNT(*) FROM system_samples WHERE ts >= ?1 AND ts <= ?2",
        params![from, to],
        |r| Ok(Coverage { first: r.get(0)?, last: r.get(1)?, samples: r.get(2)? }),
    )
}

pub struct CollectorStatus {
    pub heartbeat: i64,
}

pub fn collector_status(conn: &Connection) -> Result<Option<CollectorStatus>> {
    conn.query_row(
        "SELECT heartbeat FROM collector_status WHERE id = 1",
        [],
        |r| Ok(CollectorStatus { heartbeat: r.get(0)? }),
    )
    .optional()
}

/// Process id of the last collector that started.
pub fn collector_pid(conn: &Connection) -> Result<Option<u32>> {
    conn.query_row("SELECT pid FROM collector_status WHERE id = 1", [], |r| r.get::<_, i64>(0))
        .optional()
        .map(|p| p.map(|p| p as u32))
}

// ---------- app recordings ("Track an app") ----------

/// Starts recording `app`. With `minutes`, the recording stops by itself after that long.
pub fn start_session(conn: &Connection, app: &str, minutes: Option<i64>) -> Result<i64> {
    let now = chrono::Utc::now().timestamp();
    conn.execute(
        "INSERT INTO sessions(app, started, planned_end) VALUES (?1, ?2, ?3)",
        params![app, now, minutes.map(|m| now + m * 60)],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn end_session(conn: &Connection, id: i64, reason: &str) -> Result<()> {
    conn.execute(
        "UPDATE sessions SET ended = ?2, end_reason = ?3 WHERE id = ?1 AND ended IS NULL",
        params![id, chrono::Utc::now().timestamp(), reason],
    )?;
    Ok(())
}

pub fn delete_session(conn: &Connection, id: i64) -> Result<()> {
    conn.execute_batch("BEGIN")?;
    let r = (|| {
        conn.execute("DELETE FROM session_samples WHERE session_id = ?1", [id])?;
        conn.execute("DELETE FROM sessions WHERE id = ?1", [id])?;
        Ok(())
    })();
    conn.execute_batch(if r.is_ok() { "COMMIT" } else { "ROLLBACK" })?;
    r
}

/// (id, app, planned end) of recordings still running.
pub fn active_sessions(conn: &Connection) -> Result<Vec<(i64, String, Option<i64>)>> {
    let mut stmt = conn.prepare("SELECT id, app, planned_end FROM sessions WHERE ended IS NULL")?;
    let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
    rows.collect()
}

pub struct SessionSample {
    pub ts: i64,
    pub cpu: f64,
    pub mem: u64,
    pub io_rate: f64,
    pub procs: i64,
}

pub fn write_session_samples(conn: &mut Connection, rows: &[(i64, SessionSample)]) -> Result<()> {
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    {
        let mut stmt = tx.prepare(
            "INSERT OR REPLACE INTO session_samples(session_id, ts, cpu, mem, io_rate, procs) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )?;
        for (id, s) in rows {
            stmt.execute(params![id, s.ts, s.cpu, s.mem as i64, s.io_rate, s.procs])?;
        }
    }
    tx.commit()
}

#[derive(serde::Serialize, Clone, Debug)]
pub struct SessionSummary {
    pub id: i64,
    pub app: String,
    pub description: Option<String>,
    pub started: i64,
    pub ended: Option<i64>,
    pub end_reason: Option<String>,
    pub planned_end: Option<i64>,
    pub samples: i64,
    pub cpu_avg: f64,
    pub cpu_peak: f64,
    pub mem_avg: f64,
    pub mem_peak: u64,
    /// bytes read and written during the recording
    pub io_total: f64,
    pub procs_peak: i64,
}

pub fn sessions(conn: &Connection, limit: usize) -> Result<Vec<SessionSummary>> {
    let mut stmt = conn.prepare(
        "SELECT s.id, s.app, a.description, s.started, s.ended, s.end_reason, s.planned_end,
                COUNT(x.ts), COALESCE(AVG(x.cpu), 0), COALESCE(MAX(x.cpu), 0),
                COALESCE(AVG(x.mem), 0), COALESCE(MAX(x.mem), 0), COALESCE(SUM(x.io_rate), 0),
                COALESCE(MAX(x.procs), 0)
         FROM sessions s
         LEFT JOIN app_names a ON a.app = s.app
         LEFT JOIN session_samples x ON x.session_id = s.id
         GROUP BY s.id ORDER BY (s.ended IS NULL) DESC, s.started DESC LIMIT ?1",
    )?;
    let rows = stmt.query_map([limit as i64], |r| {
        Ok(SessionSummary {
            id: r.get(0)?,
            app: r.get(1)?,
            description: r.get(2)?,
            started: r.get(3)?,
            ended: r.get(4)?,
            end_reason: r.get(5)?,
            planned_end: r.get(6)?,
            samples: r.get(7)?,
            cpu_avg: r.get(8)?,
            cpu_peak: r.get(9)?,
            mem_avg: r.get(10)?,
            mem_peak: r.get::<_, i64>(11)? as u64,
            io_total: r.get(12)?,
            procs_peak: r.get(13)?,
        })
    })?;
    rows.collect()
}

/// Samples of one recording, averaged into `bucket`-second buckets.
pub fn session_series(conn: &Connection, id: i64, bucket: i64) -> Result<Vec<AppPoint>> {
    let mut stmt = conn.prepare(
        "SELECT (ts / ?2) * ?2 AS b, AVG(cpu), AVG(mem), AVG(io_rate), AVG(procs)
         FROM session_samples WHERE session_id = ?1 GROUP BY b ORDER BY b",
    )?;
    let rows = stmt.query_map(params![id, bucket.max(1)], |r| {
        Ok(AppPoint { ts: r.get(0)?, cpu: r.get(1)?, mem: r.get(2)?, io_rate: r.get(3)?, procs: r.get(4)? })
    })?;
    rows.collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    const MB: i64 = 1_048_576;

    /// A throwaway database, deleted when the test ends.
    struct Temp(PathBuf, Connection);

    impl Temp {
        fn new(name: &str) -> Temp {
            let path = std::env::temp_dir().join(format!("jpu-{}-{name}.db", std::process::id()));
            let _ = std::fs::remove_file(&path);
            Temp(path.clone(), open(&path).expect("open"))
        }
        /// Closes and reopens, which runs the migration and the backfill again.
        fn reopen(&mut self) {
            let fresh = open(&self.0).expect("reopen");
            self.1 = fresh;
        }
    }

    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    fn pass(db: &Temp, ts: i64, apps: &[(&str, usize, i64)]) {
        let freed: u64 = apps.iter().map(|(_, _, m)| *m as u64).sum();
        let procs: usize = apps.iter().map(|(_, p, _)| *p).sum();
        let run = log_ram_run(&db.1, ts, 30_000 * MB as u64, 32_000 * MB as u64, 27_000 * MB as u64, 85, apps.len(), procs, freed, true).unwrap();
        for (name, procs, mem) in apps {
            log_ram_item(&db.1, run, ts, name, *procs, *mem as u64, true).unwrap();
        }
    }

    #[test]
    fn a_pass_keeps_the_apps_it_trimmed() {
        let db = Temp::new("pass-items");
        pass(&db, 1_000, &[("chrome.exe", 8, 280 * MB), ("slack.exe", 3, 110 * MB)]);
        let runs = ram_runs(&db.1, 10, 0).unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].items.len(), 2);
        assert_eq!(runs[0].items[0].app, "chrome.exe", "biggest first");
        assert_eq!(runs[0].procs, 11);
        assert_eq!(runs[0].freed, 390 * MB);
        let (freed, passes, last) = ram_run_totals(&db.1, 0).unwrap();
        assert_eq!((freed, passes, last), (390 * MB, 1, Some(1_000)));
    }

    #[test]
    fn two_passes_in_the_same_second_stay_apart() {
        let db = Temp::new("same-second");
        pass(&db, 2_000, &[("chrome.exe", 2, 100 * MB)]);
        pass(&db, 2_000, &[("node.exe", 1, 50 * MB)]);
        let runs = ram_runs(&db.1, 10, 0).unwrap();
        assert_eq!(runs.len(), 2);
        for run in &runs {
            assert_eq!(run.items.len(), 1, "each pass keeps its own app");
        }
    }

    #[test]
    fn totals_only_count_the_window_asked_for() {
        let db = Temp::new("totals-window");
        pass(&db, 1_000, &[("chrome.exe", 1, 100 * MB)]);
        pass(&db, 9_000, &[("chrome.exe", 1, 300 * MB)]);
        assert_eq!(ram_run_totals(&db.1, 0).unwrap(), (400 * MB, 2, Some(9_000)));
        assert_eq!(ram_run_totals(&db.1, 5_000).unwrap(), (300 * MB, 1, Some(9_000)));
    }

    #[test]
    fn an_upgrade_fills_in_the_passes_it_never_recorded() {
        // Before 1.2.0 the cleaner wrote only to cleanup_log. Those passes become ram_runs rows, so the
        // page's totals agree with its per-app list instead of saying "Nothing freed yet" above GBs.
        let mut db = Temp::new("upgrade-backfill");
        for (ts, name, procs, mem) in [(1_000, "chrome.exe", 8, 1_600 * MB), (1_000, "node.exe", 4, 700 * MB), (1_000, "slack.exe", 2, 300 * MB)] {
            log_cleanup(&db.1, ts, name, procs, mem as u64, RAM_CLEAN_REASON, true).unwrap();
        }
        db.reopen();
        let runs = ram_runs(&db.1, 10, 0).unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].apps, 3);
        assert_eq!(runs[0].freed, 2_600 * MB);
        assert_eq!(runs[0].mem_total, 0, "no memory reading for an old pass");
        assert_eq!(runs[0].items.len(), 3, "an old pass still names the apps it emptied");
        assert_eq!(runs[0].items[0].app, "chrome.exe");
        let by_app = ram_freed_by_app(&db.1, 0).unwrap();
        let (freed, passes, _) = ram_run_totals(&db.1, 0).unwrap();
        assert_eq!(freed, by_app.iter().map(|a| a.1).sum::<i64>(), "the two lists agree");
        assert_eq!(passes, 1);
        // Opening again must not add it twice.
        db.reopen();
        assert_eq!(ram_runs(&db.1, 10, 0).unwrap().len(), 1);
    }

    #[test]
    fn a_leak_that_stopped_growing_is_not_still_growing() {
        let db = Temp::new("leak-resolve");
        let row = LeakRow {
            app: "sawtooth.exe".into(),
            first_ts: 1_000,
            last_ts: 5_000,
            from_mem: 159 * MB,
            peak_mem: 508 * MB,
            rate: 87.0,
            hours: 4.0,
            cpu: 2.0,
            series: "[]".into(),
        };
        upsert_leak(&db.1, &row).unwrap();
        assert_eq!(leak_count(&db.1).unwrap(), 1);
        resolve_leak(&db.1, "sawtooth.exe", 9_000).unwrap();
        let found = super::leaks(&db.1, 0).unwrap();
        assert!(found[0].resolved);
        assert_eq!(found[0].gone_ts, 9_000);
        assert_eq!(leak_count(&db.1).unwrap(), 0);
        // It starts again: the row is live once more, and the window is the new climb's.
        let again = LeakRow { first_ts: 20_000, last_ts: 26_000, from_mem: 200 * MB, peak_mem: 700 * MB, ..row };
        upsert_leak(&db.1, &again).unwrap();
        let found = super::leaks(&db.1, 0).unwrap();
        assert!(!found[0].resolved);
        assert_eq!((found[0].first_ts, found[0].peak_mem), (20_000, 700 * MB));
        assert!((found[0].hours - 6_000.0 / 3600.0).abs() < 0.001, "hours come from the stored window");
    }

    #[test]
    fn leaks_come_back_fastest_first_with_the_stopped_ones_last() {
        let db = Temp::new("leak-order");
        let row = |app: &str, rate: f64| LeakRow {
            app: app.into(),
            first_ts: 1_000,
            last_ts: 9_000,
            from_mem: 100 * MB,
            peak_mem: 100 * MB + (rate * 8.0) as i64 * MB,
            rate,
            hours: 8.0,
            cpu: 1.0,
            series: "[]".into(),
        };
        upsert_leak(&db.1, &row("slow.exe", 20.0)).unwrap();
        upsert_leak(&db.1, &row("stopped.exe", 500.0)).unwrap();
        upsert_leak(&db.1, &row("fast.exe", 300.0)).unwrap();
        resolve_leak(&db.1, "stopped.exe", 9_500).unwrap();
        let order: Vec<String> = super::leaks(&db.1, 0).unwrap().into_iter().map(|l| l.app).collect();
        assert_eq!(order, ["fast.exe", "slow.exe", "stopped.exe"], "fastest first, stopped last");
    }

    #[test]
    fn a_leak_row_describes_the_climb_happening_now() {
        let db = Temp::new("leak-merge");
        let first = LeakRow {
            app: "sawtooth.exe".into(),
            first_ts: 1_000,
            last_ts: 5_000,
            from_mem: 159 * MB,
            peak_mem: 508 * MB,
            rate: 87.0,
            hours: 4.0,
            cpu: 2.0,
            series: "[]".into(),
        };
        upsert_leak(&db.1, &first).unwrap();
        // Seen again after it let its memory go and started over: the row is the new climb, so the
        // numbers on the page never mix two of them.
        let next = LeakRow { first_ts: 8_000, last_ts: 9_000, from_mem: 140 * MB, peak_mem: 620 * MB, ..first };
        upsert_leak(&db.1, &next).unwrap();
        let leak = &super::leaks(&db.1, 0).unwrap()[0];
        assert_eq!(leak.first_ts, 8_000, "this climb started here");
        assert_eq!(leak.peak_mem, 620 * MB);
        assert_eq!(leak.from_mem, 140 * MB);
        assert_eq!(leak.last_ts, 9_000);
        // Rate and span always come from that stored window, never from two different climbs.
        let hours = (9_000 - 8_000) as f64 / 3600.0;
        assert!((leak.hours - hours).abs() < 0.001);
        assert!((leak.rate - (620 - 140) as f64 * MB as f64 / hours).abs() < 1.0);
    }
}
