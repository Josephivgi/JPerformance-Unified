//! Plain findings about one server, worked out from what the recorder stored.

use super::describe::Kind;
use super::db;
use crate::fmt;
use rusqlite::Connection;

#[derive(serde::Serialize, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Info,
    Warn,
    Danger,
}

#[derive(serde::Serialize, Clone, Debug)]
pub struct Insight {
    pub level: Level,
    pub title: String,
    pub detail: String,
    /// Read-only commands to run on the server to look closer.
    pub commands: Vec<String>,
    /// The moment the finding is about, if it is about a stretch of time.
    pub at: Option<i64>,
}

fn add(out: &mut Vec<Insight>, level: Level, title: impl Into<String>, detail: impl Into<String>) {
    out.push(Insight { level, title: title.into(), detail: detail.into(), commands: Vec::new(), at: None });
}

/// Attaches read-only commands to the finding just added.
fn cmds(out: &mut [Insight], list: &[&str]) {
    if let Some(i) = out.last_mut() {
        i.commands = list.iter().map(|s| s.to_string()).collect();
    }
}

/// The longest run of minutes where `pred` holds: (start, end, peak).
fn longest_run(series: &[db::Point], pred: impl Fn(&db::Point) -> bool, val: impl Fn(&db::Point) -> f64) -> Option<(i64, i64, f64)> {
    let mut best: Option<(i64, i64, f64)> = None;
    let mut cur: Option<(i64, i64, f64)> = None;
    for p in series {
        let gap = cur.is_some_and(|(_, e, _)| p.ts - e > 150);
        if pred(p) && !gap {
            cur = Some(match cur {
                Some((s, _, pk)) => (s, p.ts, pk.max(val(p))),
                None => (p.ts, p.ts, val(p)),
            });
        } else {
            if let Some(c) = cur.take() {
                if best.is_none_or(|b| c.1 - c.0 > b.1 - b.0) {
                    best = Some(c);
                }
            }
            if pred(p) {
                cur = Some((p.ts, p.ts, val(p)));
            }
        }
    }
    if let Some(c) = cur {
        if best.is_none_or(|b| c.1 - c.0 > b.1 - b.0) {
            best = Some(c);
        }
    }
    best
}

/// The groups that used the most CPU (or memory) between `from` and `to`, for "caused by".
fn behind(conn: &Connection, alias: &str, from: i64, to: i64, by_mem: bool) -> String {
    let mut list = db::group_summaries(conn, alias, from - from.rem_euclid(60), to + 60).unwrap_or_default();
    list.retain(|g| g.kind != Kind::Kernel);
    let minutes = db::minutes_recorded(conn, alias, from - from.rem_euclid(60), to + 60).unwrap_or(1).max(1) as f64;
    if by_mem {
        list.sort_by(|a, b| b.mem_avg.total_cmp(&a.mem_avg));
    }
    list.iter()
        .take(3)
        .map(|g| if by_mem { format!("{} ({})", label_of(g), fmt::bytes(g.mem_avg)) } else { format!("{} ({})", label_of(g), fmt::pct(g.cpu_load / minutes)) })
        .collect::<Vec<_>>()
        .join(", ")
}

/// What to do about a login that failed, by status.
pub fn connection_hint(status: &str, alias: &str) -> &'static str {
    let _ = alias;
    match status {
        "timeout" => "Port 22 did not answer in time. The server may be off, or a firewall or your network is blocking SSH.",
        "down" => "There is no route to the server from this PC. Check that it is on and that your network can reach it.",
        "refused" => "The server answered but nothing is listening for SSH on that port.",
        "dns" => "The host name in ~/.ssh/config could not be looked up.",
        "denied" => "The server did not accept your key. Check that the public key is in ~/.ssh/authorized_keys on the server for this user. Fix that, then press Retry.",
        "hostkey" => "The server's host key is not in known_hosts, or it changed. JPerformance never accepts host keys by itself. Run ssh once in a terminal to check and accept it.",
        "lost" => "The connection dropped and logging in again did not work yet. It tries again on its own; Retry now does it at once.",
        _ => "",
    }
}

fn label_of(g: &db::GroupSummary) -> String {
    match g.kind {
        Kind::Service | Kind::UserService => format!("{} (service)", g.label),
        Kind::Docker => format!("{} (Docker)", g.label),
        _ => g.label.clone(),
    }
}

/// Findings for one server over `from..to`, worst first.
pub fn compute(conn: &Connection, server: &db::Server, from: i64, to: i64) -> db::Result<Vec<Insight>> {
    let mut out = Vec::new();
    let alias = server.alias.as_str();
    let now = chrono::Utc::now().timestamp();

    if !matches!(server.status.as_str(), "up" | "connecting" | "paused" | "") {
        let hint = connection_hint(&server.status, alias);
        // Network trouble never reached the login, and the recorder retries it on its own.
        let verb = if matches!(server.status.as_str(), "timeout" | "lost" | "down" | "dns" | "refused") { "Cannot reach" } else { "Cannot log in" };
        let title = if server.status == "down" { "Cannot reach the server".to_string() } else { format!("{verb}: {}", server.label.to_lowercase()) };
        add(&mut out, Level::Danger, title, if hint.is_empty() { server.detail.clone() } else { hint.to_string() });
    }

    let latest = db::latest(conn, alias)?.filter(|p| now - p.ts < 120);
    let groups = db::live(conn, alias)?;
    if let Some(p) = &latest {
        // ZFS cache that would be given back counts as free, so ZFS hosts do not raise false alarms.
        let avail_pct = if p.mem_total > 0.0 { p.mem_avail_eff() / p.mem_total * 100.0 } else { 100.0 };
        let top_mem = groups.iter().filter(|g| g.kind != Kind::Kernel).max_by(|a, b| a.mem.total_cmp(&b.mem));
        let who = top_mem.map(|g| format!(" The biggest user is {} with {}.", g.label, fmt::bytes(g.mem))).unwrap_or_default();
        if avail_pct < 5.0 {
            add(&mut out, Level::Danger, "Memory is almost full", format!("Only {} of {} is free for programs.{who} Linux may start stopping programs.", fmt::bytes(p.mem_avail_eff()), fmt::bytes(p.mem_total)));
            cmds(&mut out, &["free -h", "ps aux --sort=-rss | head -15"]);
        } else if avail_pct < 15.0 {
            add(&mut out, Level::Warn, "Memory is running low", format!("{} of {} is free for programs.{who}", fmt::bytes(p.mem_avail_eff()), fmt::bytes(p.mem_total)));
            cmds(&mut out, &["free -h", "ps aux --sort=-rss | head -15"]);
        }
        if p.arc > p.mem_total * 0.2 {
            add(
                &mut out,
                Level::Info,
                format!("ZFS is caching {} in memory", fmt::bytes(p.arc)),
                "ZFS keeps recently used disk data in RAM (the ARC). It gives memory back when programs need it, so it is counted as free here. If programs still run short, cap it with zfs_arc_max.",
            );
            cmds(&mut out, &["arc_summary | head -40", "cat /sys/module/zfs/parameters/zfs_arc_max"]);
        }
        if p.swap_total > 0.0 && p.swap_used > 256.0 * 1048576.0 && p.swap_pct() > 20.0 {
            add(&mut out, Level::Warn, format!("{} of swap in use", fmt::bytes(p.swap_used)), "Some memory has been moved to disk, which is much slower. This usually means memory ran short at some point.");
            cmds(&mut out, &["free -h", "vmstat 1 5", "for f in /proc/*/status; do awk '/^Name|^VmSwap/{printf $2\" \"}END{print \"\"}' $f; done 2>/dev/null | sort -k2 -n -r | head -10"]);
        }
        for m in db::mounts(conn, alias)? {
            let pct = m.pct();
            if pct >= 90.0 {
                add(&mut out, Level::Danger, format!("Disk {} is {}% full", m.mount, pct), format!("{} left of {}. Programs fail to write when it fills up.", fmt::bytes(m.avail), fmt::bytes(m.size)));
                let du = format!("du -xh --max-depth=1 {} 2>/dev/null | sort -h | tail -15", m.mount);
                cmds(&mut out, &[du.as_str(), "journalctl --disk-usage", "docker system df 2>/dev/null"]);
            } else if pct >= 80.0 {
                add(&mut out, Level::Warn, format!("Disk {} is {}% full", m.mount, pct), format!("{} left of {}.", fmt::bytes(m.avail), fmt::bytes(m.size)));
                let du = format!("du -xh --max-depth=1 {} 2>/dev/null | sort -h | tail -15", m.mount);
                cmds(&mut out, &[du.as_str()]);
            }
        }
    }

    if let Some((min, max, first)) = db::oom_range(conn, alias, from, to)? {
        if max > min {
            add(
                &mut out,
                Level::Danger,
                format!("Linux stopped {} program{} for lack of memory", max - min, if max - min == 1 { "" } else { "s" }),
                format!("Out-of-memory kills since {}. Check which program grew in the memory chart around then.", fmt::day_time(first)),
            );
            cmds(&mut out, &["journalctl -k --since '-2 days' | grep -i -E 'out of memory|killed process' | tail -10"]);
            if let Some(i) = out.last_mut() {
                i.at = Some(first);
            }
        }
    }

    let series = db::series(conn, alias, from, to, 60)?;
    if !series.is_empty() {
        let n = series.len() as f64;
        let avg = |f: fn(&db::Point) -> f64| series.iter().map(f).sum::<f64>() / n;
        let cores = server.cores.max(1) as f64;
        let cpu = avg(|p| p.cpu);
        let iowait = avg(|p| p.iowait);
        let steal = avg(|p| p.steal);
        let busy_minutes = series.iter().filter(|p| p.cpu >= 90.0).count();
        if cpu >= 80.0 {
            add(&mut out, Level::Danger, format!("CPU averaged {} over this range", fmt::pct(cpu)), "The server is busy most of the time. Work queues up and pages and jobs get slower.");
            cmds(&mut out, &["top -b -n 1 -o %CPU | head -20"]);
        } else if busy_minutes >= 15 {
            add(&mut out, Level::Warn, format!("CPU was above 90% for {} minutes", busy_minutes), "Look at the CPU chart to see when, and hover it to see what was running.");
            cmds(&mut out, &["top -b -n 1 -o %CPU | head -20"]);
        }
        // The longest slowdowns, with when they started and what was behind them.
        if let Some((s, e, peak)) = longest_run(&series, |p| p.cpu >= 90.0, |p| p.cpu).filter(|(s, e, _)| e - s >= 5 * 60) {
            let who = behind(conn, alias, s, e, false);
            add(&mut out, Level::Warn, format!("CPU was busy for {} starting {}", fmt::duration(e - s + 60), fmt::day_time(s)),
                format!("It peaked at {}.{}", fmt::pct(peak), if who.is_empty() { String::new() } else { format!(" Most CPU went to {who}.") }));
            if let Some(i) = out.last_mut() {
                i.at = Some(s);
            }
        }
        if let Some((s, e, peak)) = longest_run(&series, |p| p.mem_pct() >= 90.0, |p| p.mem_pct()).filter(|(s, e, _)| e - s >= 5 * 60) {
            let who = behind(conn, alias, s, e, true);
            add(&mut out, Level::Warn, format!("Memory was over 90% for {} starting {}", fmt::duration(e - s + 60), fmt::day_time(s)),
                format!("It peaked at {}.{}", fmt::pct(peak), if who.is_empty() { String::new() } else { format!(" The biggest users were {who}.") }));
            if let Some(i) = out.last_mut() {
                i.at = Some(s);
            }
        }
        let recent: Vec<&db::Point> = series.iter().filter(|p| p.ts >= to - 15 * 60).collect();
        if !recent.is_empty() {
            let load = recent.iter().map(|p| p.load1).sum::<f64>() / recent.len() as f64;
            if load > cores * 1.5 {
                add(&mut out, Level::Warn, format!("Load {:.1} on {} cores", load, cores), "More work is waiting than the CPU cores can run. This can also be programs waiting on a slow disk.");
                cmds(&mut out, &["vmstat 1 5", "ps -eo state,pid,cmd | awk '$1 ~ /D/' | head"]);
            }
        }
        if iowait >= 15.0 {
            add(&mut out, Level::Warn, format!("CPU waits on disk {} of the time", fmt::pct(iowait)), "The disks are slow to answer for what is being asked of them. The disk I/O chart shows when.");
            cmds(&mut out, &["iostat -x 1 5", "cat /proc/pressure/io 2>/dev/null"]);
        }
        if steal >= 8.0 {
            add(&mut out, Level::Warn, format!("{} of CPU time taken by the host", fmt::pct(steal)), "This is a VM and the machine it runs on is giving its CPU to other VMs. Upgrading the plan or moving hosts helps; nothing inside this server will.");
        }
    }

    if server.reboot {
        add(&mut out, Level::Info, "Restart needed", "Updates were installed that only take effect after a reboot (/var/run/reboot-required).");
        cmds(&mut out, &["cat /var/run/reboot-required.pkgs 2>/dev/null"]);
    }
    if !server.docker_note.is_empty() {
        add(&mut out, Level::Info, "Container names are hidden", server.docker_note.clone());
    }

    // What used the most over the range.
    let sums = db::group_summaries(conn, alias, from, to)?;
    let minutes = db::minutes_recorded(conn, alias, from, to)?.max(1) as f64;
    if let Some(g) = sums.iter().filter(|g| g.kind != Kind::Kernel).max_by(|a, b| a.cpu_load.total_cmp(&b.cpu_load)) {
        if g.cpu_load / minutes >= 1.0 {
            add(
                &mut out,
                Level::Info,
                format!("Most CPU: {}", label_of(g)),
                format!("Averaged {} of the whole server, peaking at {}. {}", fmt::pct(g.cpu_load / minutes), fmt::pct(g.cpu_peak), g.explain),
            );
        }
    }
    if let Some(g) = sums.iter().filter(|g| g.kind != Kind::Kernel).max_by(|a, b| a.mem_avg.total_cmp(&b.mem_avg)) {
        add(&mut out, Level::Info, format!("Most memory: {}", label_of(g)), format!("Averaged {}, at most {}. {}", fmt::bytes(g.mem_avg), fmt::bytes(g.mem_peak), g.explain));
    }

    out.sort_by(|a, b| b.level.cmp(&a.level));
    Ok(out)
}
