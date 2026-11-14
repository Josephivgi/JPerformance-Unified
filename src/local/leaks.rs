//! Finds memory leaks: an app whose memory keeps climbing for hours and never comes back down.
//! A block of work makes memory go up and then down again; a leak only goes up. The recorder writes one
//! minute of memory per app, so this reads that history, adds back what the RAM cleaner took out of RAM
//! (that memory came back, which is the leak), and looks at the climb that is still going on.

use std::collections::HashMap;

use rusqlite::Connection;

use super::{db, procs};

/// A day is looked at, so a slow leak of a few hundred MB a day still shows.
const WINDOW: i64 = 24 * 3600;
/// One point every ten minutes, taking the highest reading in each, so one busy moment is not a leak.
const BUCKET: i64 = 600;
/// Longer than this between readings and the app was not running: a climb stops there.
const GAP: i64 = 2 * BUCKET;
/// It has to have been climbing for this long, over at least this many readings.
const MIN_HOURS: f64 = 2.0;
const MIN_POINTS: usize = 13;
/// Memory it must still be holding more of, and how fast it was gaining it.
const MIN_GROWTH: i64 = 200 * 1024 * 1024;
const MIN_RATE: f64 = 30.0 * 1_048_576.0;
/// Above this much CPU, the memory is going into work the app is doing, not leaking.
const MAX_CPU: f64 = 25.0;
/// A fall of this share of everything it gained ends one climb and starts the next: the app let the
/// memory go, or it was restarted. Falls smaller than the floor are the ordinary noise of a running app.
const BREAK: f64 = 0.25;
const BREAK_FLOOR: i64 = 24 * 1024 * 1024;
/// A climb only counts as still going while it is being fed: a reading this recent, and this much
/// gained in the last hour.
const LIVE: i64 = 20 * 60;
const RECENT: i64 = 3600;
const RECENT_GROWTH: i64 = 10 * 1024 * 1024;

/// One app's memory every ten minutes: highest reading in the bucket, and the CPU it used there.
#[derive(Clone, Copy)]
struct Point {
    ts: i64,
    mem: i64,
    cpu: f64,
}

/// What each app had in memory over the window, from its minute samples.
fn history(conn: &Connection, from: i64, to: i64) -> db::Result<HashMap<String, Vec<Point>>> {
    let mut stmt = conn.prepare(
        "SELECT app, (minute / ?3) * ?3 AS b, MAX(mem_max), AVG(cpu_avg)
         FROM app_minutes WHERE minute >= ?1 AND minute <= ?2 GROUP BY app, b ORDER BY app, b",
    )?;
    let rows = stmt.query_map(rusqlite::params![from, to, BUCKET], |r| {
        Ok((r.get::<_, String>(0)?, Point { ts: r.get(1)?, mem: r.get(2)?, cpu: r.get(3)? }))
    })?;
    let mut out: HashMap<String, Vec<Point>> = HashMap::new();
    for row in rows {
        let (app, p) = row?;
        out.entry(app).or_default().push(p);
    }
    Ok(out)
}

/// What the RAM cleaner moved out of RAM, per app, in the window: (when, bytes).
fn trimmed(conn: &Connection, from: i64) -> db::Result<HashMap<String, Vec<(i64, i64)>>> {
    let mut stmt = conn.prepare("SELECT name, ts, mem FROM cleanup_log WHERE reason = ?1 AND ts >= ?2 ORDER BY ts")?;
    let rows = stmt.query_map(rusqlite::params![db::RAM_CLEAN_REASON, from], |r| {
        Ok((r.get::<_, String>(0)?, (r.get::<_, i64>(1)?, r.get::<_, i64>(2)?)))
    })?;
    let mut out: HashMap<String, Vec<(i64, i64)>> = HashMap::new();
    for row in rows {
        let (app, t) = row?;
        out.entry(app).or_default().push(t);
    }
    Ok(out)
}

/// Puts back what the cleaner took. Emptying an app's working set drops the figure the recorder reads,
/// and the app then reads that memory back in — which is the leak. Without this, every pass would look
/// like the app letting go of its memory, and a leak the cleaner keeps emptying would never be found.
fn with_trims(points: &[Point], trims: &[(i64, i64)]) -> Vec<Point> {
    let mut out = points.to_vec();
    for &(ts, bytes) in trims {
        // Only the readings after the pass: the bucket the pass fell in still holds what was there before it.
        for p in out.iter_mut().filter(|p| p.ts > ts) {
            p.mem += bytes;
        }
    }
    out
}

/// The stretches the app was actually running in. A gap means it was closed or the recorder was off.
fn stretches(points: &[Point]) -> Vec<&[Point]> {
    let mut out = Vec::new();
    let mut start = 0;
    for i in 1..points.len() {
        if points[i].ts - points[i - 1].ts > GAP {
            out.push(&points[start..i]);
            start = i;
        }
    }
    if start < points.len() {
        out.push(&points[start..]);
    }
    out
}

/// The climbs inside one stretch: each ends where memory falls back by a good part of what it gained.
fn climbs(stretch: &[Point]) -> Vec<&[Point]> {
    let mut out = Vec::new();
    if stretch.is_empty() {
        return out;
    }
    let (mut start, mut base, mut top) = (0, stretch[0].mem, stretch[0].mem);
    for (i, p) in stretch.iter().enumerate().skip(1) {
        if p.mem >= top {
            top = p.mem;
        } else if top > base && (top - p.mem) as f64 > BREAK * (top - base) as f64 && top - p.mem > BREAK_FLOOR {
            out.push(&stretch[start..i]);
            start = i;
            base = p.mem;
            top = p.mem;
        } else {
            base = base.min(p.mem);
        }
    }
    out.push(&stretch[start..]);
    out
}

/// The longest recent climb with the most memory still held, if any of them is a leak.
fn worst_climb(points: &[Point], now: i64) -> Option<&[Point]> {
    let mut best: Option<&[Point]> = None;
    for stretch in stretches(points) {
        for climb in climbs(stretch) {
            if climb.len() < MIN_POINTS {
                continue;
            }
            let (first, last) = (climb[0], climb[climb.len() - 1]);
            // Still being fed: a reading from just now, and memory still going in over the last hour.
            // A climb that stopped hours ago is not "still growing", and a plateau is not either.
            if now - last.ts > LIVE {
                continue;
            }
            let hour_ago = climb.iter().rev().find(|p| p.ts <= last.ts - RECENT).map_or(first.mem, |p| p.mem);
            if last.mem - hour_ago < RECENT_GROWTH {
                continue;
            }
            let hours = (last.ts - first.ts) as f64 / 3600.0;
            let growth = last.mem - first.mem;
            let peak = climb.iter().map(|p| p.mem).max().unwrap_or(last.mem);
            let cpu = climb.iter().map(|p| p.cpu).sum::<f64>() / climb.len() as f64;
            // Still near its highest, gaining fast enough, and not just busy with work.
            if hours >= MIN_HOURS
                && growth >= MIN_GROWTH
                && growth as f64 / hours.max(0.01) >= MIN_RATE
                && (last.mem as f64) >= peak as f64 * 0.9
                && cpu <= MAX_CPU
                && best.is_none_or(|b| growth > b[b.len() - 1].mem - b[0].mem)
            {
                best = Some(climb);
            }
        }
    }
    best
}

/// Looks at the last day and writes what it finds. Apps that were climbing and no longer are get marked
/// as such, so the page can tell a leak that is still there from one that went away.
pub fn detect(conn: &Connection, now: i64) -> db::Result<Vec<String>> {
    let from = now - WINDOW;
    let hist = history(conn, from, now)?;
    let trims = trimmed(conn, from)?;
    let me = std::env::current_exe()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
        .unwrap_or_else(|| "jperformanceunified.exe".into());
    let mut found: Vec<String> = Vec::new();

    for (app, points) in &hist {
        if app.eq_ignore_ascii_case(&me) || procs::is_protected(app, None) {
            continue;
        }
        let points = match trims.get(app) {
            Some(list) => with_trims(points, list),
            None => points.clone(),
        };
        let Some(climb) = worst_climb(&points, now) else { continue };
        let (first, last) = (climb[0], climb[climb.len() - 1]);
        let hours = (last.ts - first.ts) as f64 / 3600.0;
        let row = db::LeakRow {
            app: app.clone(),
            first_ts: first.ts,
            last_ts: last.ts,
            from_mem: first.mem,
            peak_mem: climb.iter().map(|p| p.mem).max().unwrap_or(last.mem),
            rate: (last.mem - first.mem) as f64 / hours.max(0.01),
            hours,
            cpu: climb.iter().map(|p| p.cpu).sum::<f64>() / climb.len() as f64,
            series: serde_json::to_string(&climb.iter().map(|p| (p.ts, p.mem)).collect::<Vec<_>>()).unwrap_or_else(|_| "[]".into()),
        };
        db::upsert_leak(conn, &row)?;
        found.push(app.clone());
    }

    // Everything else that was climbing has either been closed or let the memory go. A recorder that has
    // stopped writes nothing, so its silence is not read as every app having stopped growing.
    let recorder_quiet = conn
        .query_row("SELECT COALESCE(MAX(ts), 0) FROM system_samples", [], |r| r.get::<_, i64>(0))?
        < now - 10 * 60;
    for app in db::unresolved_leaks(conn)? {
        if found.iter().any(|a| a.eq_ignore_ascii_case(&app)) {
            continue;
        }
        let latest = hist.get(&app).and_then(|p| p.last().map(|x| x.mem)).unwrap_or(0);
        let quiet = hist.get(&app).and_then(|p| p.last().map(|x| x.ts)).unwrap_or(0) < now - 30 * 60;
        let (peak, from_mem) = conn
            .query_row("SELECT peak_mem, from_mem FROM leak_finds WHERE app = ?1", [&app], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))
            .unwrap_or((0, 0));
        let fell = latest <= from_mem + (peak - from_mem) / 2;
        if fell || (quiet && !recorder_quiet) {
            db::resolve_leak(conn, &app, now)?;
        }
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MB: i64 = 1_048_576;
    const NOW: i64 = 20 * 24 * 3600;

    /// A reading every ten minutes for `hours`, memory given in MB at each step.
    fn series(hours: i64, cpu: f64, mem: impl Fn(i64) -> i64) -> Vec<Point> {
        (0..hours * 6).map(|i| Point { ts: NOW - hours * 3600 + i * BUCKET, mem: mem(i) * MB, cpu }).collect()
    }

    #[test]
    fn a_steady_climb_is_a_leak() {
        // 40 MB an hour for six hours, using almost no processor.
        let points = series(6, 1.0, |i| 200 + i * 40 / 6);
        let climb = worst_climb(&points, NOW).expect("a leak");
        assert_eq!(climb.first().unwrap().mem, 200 * MB);
        assert!(climb.last().unwrap().mem >= 400 * MB);
    }

    #[test]
    fn filling_up_and_staying_flat_is_not_a_leak() {
        let points = series(6, 1.0, |i| if i < 3 { 100 + i * 60 } else { 280 });
        assert!(worst_climb(&points, NOW).is_none());
    }

    #[test]
    fn work_that_uses_the_processor_is_not_a_leak() {
        // The same climb, but the app is busy: that is memory going into work, not leaking.
        let points = series(6, 60.0, |i| 200 + i * 40 / 6);
        assert!(worst_climb(&points, NOW).is_none());
    }

    #[test]
    fn letting_the_memory_go_ends_the_climb() {
        // It climbs for three hours, then drops back and stays there.
        let points = series(8, 1.0, |i| if i < 18 { 200 + i * 40 / 6 } else { 210 });
        assert!(worst_climb(&points, NOW).is_none());
    }

    #[test]
    fn a_leak_after_a_restart_is_still_a_leak() {
        // Restarted halfway: it starts over at 150 MB and climbs again.
        let points = series(8, 1.0, |i| if i < 24 { 150 + i * 30 / 6 } else { 150 + (i - 24) * 60 / 6 });
        let climb = worst_climb(&points, NOW).expect("a leak");
        assert_eq!(climb.first().unwrap().mem, 150 * MB);
        assert!(climb.last().unwrap().mem >= 350 * MB);
    }

    #[test]
    fn small_dips_do_not_break_a_climb() {
        // A browser letting go of a tab, then carrying on: still one climb.
        let points = series(6, 2.0, |i| if i == 12 { 265 } else { 200 + i * 40 / 6 });
        let climb = worst_climb(&points, NOW).expect("a leak");
        assert_eq!(climb.first().unwrap().mem, 200 * MB);
        assert!(climb.last().unwrap().mem >= 400 * MB);
    }

    #[test]
    fn a_closed_app_is_not_followed_across_the_gap() {
        // Two short climbs either side of a long gap, neither long enough on its own.
        let mut points = series(2, 1.0, |i| 100 + i * 60 / 6);
        points.extend((0..12).map(|i| Point { ts: NOW - 8 * 3600 + i * BUCKET, mem: (100 + i * 60 / 6) * MB, cpu: 1.0 }));
        assert!(worst_climb(&points, NOW).is_none());
    }

    #[test]
    fn a_climb_that_stopped_hours_ago_is_not_still_growing() {
        // It climbed all evening and was closed three hours ago: it is history, not a live leak.
        let points = series(6, 1.0, |i| 200 + i * 40 / 6)
            .into_iter()
            .map(|p| Point { ts: p.ts - 3 * 3600, ..p })
            .collect::<Vec<_>>();
        assert!(worst_climb(&points, NOW).is_none());
    }

    #[test]
    fn a_plateau_is_not_still_growing() {
        // It climbs for two hours and then holds flat for four: nothing is going in any more.
        let points = series(6, 1.0, |i| if i < 12 { 200 + i * 20 } else { 440 });
        assert!(worst_climb(&points, NOW).is_none());
    }

    #[test]
    fn a_slow_leak_with_noise_is_still_a_leak() {
        // 40 MB an hour, with a 15 MB dip every hour from a cache being flushed.
        let points = series(8, 1.0, |i| {
            let climb = 200 + i * 40 / 6;
            if i % 6 == 3 { climb - 15 } else { climb }
        });
        let climb = worst_climb(&points, NOW).expect("a leak");
        assert!(climb.last().unwrap().mem >= 500 * MB);
    }

    #[test]
    fn memory_the_cleaner_freed_is_added_back() {
        // The cleaner empties the app's working set twice; the app reads it all back. Without the
        // add-back this looks like the app letting go of its memory and no leak is found.
        let mut points = series(6, 1.0, |i| 200 + i * 40 / 6);
        let trims = vec![(points[12].ts, 90 * MB), (points[24].ts, 90 * MB)];
        for &(ts, bytes) in &trims {
            for p in points.iter_mut().filter(|p| p.ts > ts) {
                p.mem -= bytes;
            }
        }
        // What the cleaner left behind: a sawtooth that never recovers on its own.
        assert!(worst_climb(&points, NOW).is_none());
        let fixed = with_trims(&points, &trims);
        let climb = worst_climb(&fixed, NOW).expect("a leak");
        assert!(climb.last().unwrap().mem >= 400 * MB);
    }

    #[test]
    fn protected_programs_are_skipped() {
        // The detector never names something the cleaner itself refuses to touch.
        assert!(procs::is_protected("svchost.exe", None));
        assert!(!procs::is_protected("sawtooth.exe", None));
    }
}
