use chrono::{Local, TimeZone};

pub fn bytes(b: f64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = b.max(0.0);
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{v:.0} {}", UNITS[i])
    } else if v >= 100.0 {
        format!("{v:.0} {}", UNITS[i])
    } else {
        format!("{v:.1} {}", UNITS[i])
    }
}

pub fn pct(v: f64) -> String {
    if v < 10.0 { format!("{v:.1}%") } else { format!("{v:.0}%") }
}

pub fn local(ts: i64) -> chrono::DateTime<Local> {
    Local.timestamp_opt(ts, 0).single().unwrap_or_else(Local::now)
}

pub fn day_time(ts: i64) -> String {
    local(ts).format("%a %b %-d, %H:%M").to_string()
}

pub fn duration(secs: i64) -> String {
    let secs = secs.max(0);
    if secs < 90 {
        format!("{secs} s")
    } else if secs < 90 * 60 {
        format!("{} min", (secs + 30) / 60)
    } else if secs < 48 * 3600 {
        format!("{:.1} h", secs as f64 / 3600.0)
    } else {
        format!("{:.1} days", secs as f64 / 86_400.0)
    }
}
