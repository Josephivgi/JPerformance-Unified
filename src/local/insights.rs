//! Turns recorded samples into plain-language findings.

use super::{db, describe};
use crate::fmt;
use rusqlite::Connection;

#[derive(serde::Serialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    Info,
    Warning,
    Critical,
}

#[derive(serde::Serialize, Clone, Debug)]
pub struct Insight {
    pub severity: Severity,
    pub title: String,
    pub detail: String,
    /// Optional moment the finding points at.
    pub at: Option<i64>,
    /// What to do about it, in one sentence.
    pub action: Option<String>,
    /// An app (exe name) the user may want to stop, if it is still running.
    pub app: Option<String>,
}

const SPIKE_CPU: f64 = 85.0;
const SPIKE_MIN_SECS: i64 = 60;
const MEM_PRESSURE: f64 = 90.0;
const COMMIT_PRESSURE: f64 = 90.0;

fn name(app: &str, desc: &Option<String>) -> String {
    describe::display_name(app, desc.as_deref())
}

fn culprits(conn: &Connection, from: i64, to: i64) -> String {
    let from = from - from.rem_euclid(60);
    match db::top_apps_in(conn, from, to, 3) {
        Ok(list) if !list.is_empty() => list
            .iter()
            .filter(|(_, _, cpu)| *cpu >= 0.5)
            .map(|(a, d, cpu)| format!("{} ({})", name(a, d), fmt::pct(*cpu)))
            .collect::<Vec<_>>()
            .join(", "),
        _ => String::new(),
    }
}

/// Runs of consecutive samples where `pred` holds for at least `min_secs`.
fn runs(
    points: &[db::SysPoint],
    min_secs: i64,
    pred: impl Fn(&db::SysPoint) -> bool,
    val: impl Fn(&db::SysPoint) -> f64,
) -> Vec<(i64, i64, f64)> {
    let mut out = Vec::new();
    let mut start: Option<(i64, f64)> = None;
    let mut prev_ts = 0;
    for p in points {
        let gap = start.is_some() && p.ts - prev_ts > 90; // collector was off
        if pred(p) && !gap {
            start = Some(match start {
                Some((s, peak)) => (s, peak.max(val(p))),
                None => (p.ts, val(p)),
            });
        } else {
            if let Some((s, peak)) = start.take() {
                if prev_ts - s >= min_secs {
                    out.push((s, prev_ts, peak));
                }
            }
            if pred(p) {
                start = Some((p.ts, val(p)));
            }
        }
        prev_ts = p.ts;
    }
    if let Some((s, peak)) = start {
        if prev_ts - s >= min_secs {
            out.push((s, prev_ts, peak));
        }
    }
    out
}

pub fn compute(conn: &Connection, from: i64, to: i64) -> db::Result<Vec<Insight>> {
    let mut out = Vec::new();
    let cov = db::coverage(conn, from, to)?;
    let (Some(first), Some(last)) = (cov.first, cov.last) else {
        out.push(Insight {
            severity: Severity::Info,
            title: "No data for this time range yet".into(),
            detail: "Background tracking records a sample every few seconds. Leave it running and come back later.".into(),
            at: None,
            action: None,
            app: None,
        });
        return Ok(out);
    };

    let raw = db::system_series(conn, from, to, 1)?;
    let avg_cpu = raw.iter().map(|p| p.cpu).sum::<f64>() / raw.len().max(1) as f64;
    let avg_mem = raw.iter().map(|p| p.mem_pct).sum::<f64>() / raw.len().max(1) as f64;
    out.push(Insight {
        severity: Severity::Info,
        title: format!("Your PC averaged {} CPU and {} memory", fmt::pct(avg_cpu), fmt::pct(avg_mem)),
        detail: format!(
            "Based on {} samples covering {} (from {} to {}).",
            cov.samples,
            fmt::duration(last - first),
            fmt::day_time(first),
            fmt::day_time(last)
        ),
        at: None,
        action: None,
        app: None,
    });

    // Busiest hour.
    let hourly = db::system_series(conn, from, to, 3600)?;
    if let Some(h) = hourly.iter().max_by(|a, b| a.cpu.total_cmp(&b.cpu)) {
        let who = culprits(conn, h.ts, h.ts + 3599);
        let severity = if h.cpu >= 70.0 { Severity::Warning } else { Severity::Info };
        out.push(Insight {
            severity,
            title: format!("Busiest hour: {} (CPU averaged {})", fmt::day_time(h.ts), fmt::pct(h.cpu)),
            detail: if who.is_empty() {
                format!("Peak sample in that hour reached {}.", fmt::pct(h.cpu_max))
            } else {
                format!("Peak sample reached {}. Most CPU went to {who}.", fmt::pct(h.cpu_max))
            },
            at: Some(h.ts),
            action: None,
            app: None,
        });
    }

    // Apps.
    let apps = db::app_summaries(conn, from, to)?;
    let total_load: f64 = apps.iter().map(|a| a.cpu_load).sum();
    if let Some(a) = apps.first() {
        if a.cpu_load > 0.0 {
            out.push(Insight {
                severity: Severity::Info,
                title: format!(
                    "{} used the most CPU ({:.0}% of all app CPU use)",
                    name(&a.app, &a.description),
                    100.0 * a.cpu_load / total_load.max(f64::EPSILON)
                ),
                detail: format!(
                    "It averaged {} of your CPU over {} of activity and peaked at {}. {}",
                    fmt::pct(a.cpu_avg),
                    fmt::duration(a.minutes * 60),
                    fmt::pct(a.cpu_peak),
                    describe::explain(&a.app, a.description.as_deref())
                ),
                at: None,
                action: None,
                app: None,
            });
        }
    }
    if let Some(a) = apps.iter().max_by(|x, y| x.mem_avg.total_cmp(&y.mem_avg)) {
        out.push(Insight {
            severity: Severity::Info,
            title: format!("{} used the most memory ({} on average)", name(&a.app, &a.description), fmt::bytes(a.mem_avg)),
            detail: format!(
                "Its peak was {}. {}",
                fmt::bytes(a.mem_peak as f64),
                describe::explain(&a.app, a.description.as_deref())
            ),
            at: None,
            action: None,
            app: None,
        });
    }
    if let Some(a) = apps.iter().max_by_key(|x| x.io_total) {
        if a.io_total > 100 * 1024 * 1024 {
            out.push(Insight {
                severity: Severity::Info,
                title: format!("{} read and wrote the most data ({})", name(&a.app, &a.description), fmt::bytes(a.io_total as f64)),
                detail: "This counts disk and other file I/O. Heavy I/O can make a PC with a hard drive feel slow.".into(),
                at: None,
                action: None,
                app: None,
            });
        }
    }

    // CPU spikes.
    let spikes = runs(&raw, SPIKE_MIN_SECS, |p| p.cpu >= SPIKE_CPU, |p| p.cpu);
    if !spikes.is_empty() {
        let total: i64 = spikes.iter().map(|(s, e, _)| e - s).sum();
        out.push(Insight {
            severity: Severity::Warning,
            title: format!(
                "CPU was above {SPIKE_CPU:.0}% for {} across {} stretch{}",
                fmt::duration(total),
                spikes.len(),
                if spikes.len() == 1 { "" } else { "es" }
            ),
            detail: "When the CPU stays this high, apps respond slowly. The longest ones are listed below.".into(),
            at: None,
            action: None,
            app: None,
        });
        let mut longest = spikes.clone();
        longest.sort_by_key(|(s, e, _)| -(e - s));
        for (s, e, peak) in longest.into_iter().take(3) {
            let who = culprits(conn, s, e);
            let top = db::top_apps_in(conn, s - s.rem_euclid(60), e, 1).unwrap_or_default().into_iter().next();
            out.push(Insight {
                severity: Severity::Warning,
                title: format!("High CPU for {} starting {}", fmt::duration(e - s), fmt::day_time(s)),
                detail: if who.is_empty() {
                    format!("Peaked at {}.", fmt::pct(peak))
                } else {
                    format!("Peaked at {}. Caused mostly by {who}.", fmt::pct(peak))
                },
                at: Some(s),
                action: top.as_ref().map(|(a, d, _)| format!("If {} is still busy and you are not using it, stop it. To see exactly when it gets busy, record it from Recordings.", name(a, d))),
                app: top.map(|(a, _, _)| a),
            });
        }
    }

    // Memory pressure.
    let pressure = runs(&raw, 30, |p| p.mem_pct >= MEM_PRESSURE, |p| p.mem_pct);
    if !pressure.is_empty() {
        let total: i64 = pressure.iter().map(|(s, e, _)| e - s).sum();
        let (s, e, _) = *pressure.iter().max_by_key(|(s, e, _)| e - s).unwrap();
        let from_m = s - s.rem_euclid(60);
        let top_mem = db::top_mem_apps_in(conn, from_m, e, 3).unwrap_or_default();
        let who = top_mem
            .iter()
            .map(|(a, d, m)| format!("{} ({})", name(a, d), fmt::bytes(*m)))
            .collect::<Vec<_>>()
            .join(", ");
        out.push(Insight {
            severity: Severity::Critical,
            title: format!("Memory was over {MEM_PRESSURE:.0}% full for {}", fmt::duration(total)),
            detail: format!(
                "Windows starts moving data to disk when memory is this full, which slows everything down. The longest stretch began {}{}.",
                fmt::day_time(s),
                if who.is_empty() { String::new() } else { format!(", when the biggest users were {who}") }
            ),
            at: Some(s),
            action: Some(format!(
                "Close apps you are not using{}. Clean up also lists helpers left running by apps that already closed.",
                top_mem.first().map(|(a, d, _)| format!(", starting with {}", name(a, d))).unwrap_or_default()
            )),
            app: top_mem.first().map(|(a, _, _)| a.clone()),
        });
    }

    // Committed memory near the limit: apps crash or refuse to start even if RAM looks free.
    let commit = runs(&raw, 60, |p| p.commit_pct >= COMMIT_PRESSURE, |p| p.commit_pct);
    if !commit.is_empty() {
        let total: i64 = commit.iter().map(|(s, e, _)| e - s).sum();
        let (s, _, peak) = *commit.iter().max_by_key(|(s, e, _)| e - s).unwrap();
        let mut live = db::live_apps(conn).unwrap_or_default();
        live.retain(|r| !super::procs::is_protected(&r.app, r.exe_path.as_deref()));
        live.sort_by(|a, b| b.private.cmp(&a.private));
        let who = live
            .iter()
            .take(3)
            .filter(|r| r.private > 0)
            .map(|r| format!("{} ({})", name(&r.app, &r.description), fmt::bytes(r.private as f64)))
            .collect::<Vec<_>>()
            .join(", ");
        out.push(Insight {
            severity: Severity::Critical,
            title: format!("Committed memory was over {COMMIT_PRESSURE:.0}% of the limit for {}", fmt::duration(total)),
            detail: format!(
                "Windows had promised programs almost all of its RAM plus page file (peak {}). When that runs out, apps freeze, crash or will not open, even if Task Manager shows free memory.{}",
                fmt::pct(peak),
                if who.is_empty() { String::new() } else { format!(" Promised the most right now: {who}.") }
            ),
            at: Some(s),
            action: Some("Close the apps that hold the most, or let Windows grow the page file: Settings > System > About > Advanced system settings > Performance Settings > Advanced > Virtual memory > Change, then pick System managed size.".into()),
            app: live.first().filter(|r| r.private > 0).map(|r| r.app.clone()),
        });
    }

    // Time-of-day pattern.
    if last - first >= 2 * 86_400 {
        let grid = db::weekday_hour_cpu(conn, from, to)?;
        let mut by_hour = [(0.0f64, 0u32); 24];
        for day in grid.iter() {
            for (h, v) in day.iter().enumerate() {
                if let Some(v) = v {
                    by_hour[h].0 += v;
                    by_hour[h].1 += 1;
                }
            }
        }
        if let Some((h, (sum, n))) = by_hour
            .iter()
            .enumerate()
            .filter(|(_, (_, n))| *n > 0)
            .max_by(|a, b| (a.1.0 / a.1.1 as f64).total_cmp(&(b.1.0 / b.1.1 as f64)))
        {
            out.push(Insight {
                severity: Severity::Info,
                title: format!("Your PC is usually busiest around {h:02}:00"),
                detail: format!("CPU averages {} in that hour across the days recorded.", fmt::pct(sum / *n as f64)),
                at: None,
                action: None,
                app: None,
            });
        }
    }

    Ok(out)
}
