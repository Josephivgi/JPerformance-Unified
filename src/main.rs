#![windows_subsystem = "windows"]

mod api;
mod autostart;
mod cmd;
mod collector;
mod fmt;
mod local;
mod paths;
mod servers;
mod update;

use std::io::Write;

/// Appends a line to `collector.log` in the data folder.
pub fn log(msg: &str) {
    let path = paths::data_dir().join("collector.log");
    if std::fs::metadata(&path).map(|m| m.len() > 512 * 1024).unwrap_or(false) {
        let _ = std::fs::rename(&path, paths::data_dir().join("collector.old.log"));
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "{} {msg}", chrono::Local::now().format("%Y-%m-%d %H:%M:%S"));
    }
}

fn attach_console() {
    use windows_sys::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
    unsafe {
        AttachConsole(ATTACH_PARENT_PROCESS);
    }
}

/// `--report [hours]` prints the findings for this PC and every server.
fn report(hours: i64) -> Result<(), String> {
    attach_console();
    let to = chrono::Utc::now().timestamp();
    let from = to - hours * 3600;
    println!("\nJPerformance Unified report, last {hours} h  ({})", paths::data_dir().display());
    println!("Recorder running: {}", collector::is_running());
    let conn = local::db::open(&paths::local_db()).map_err(|e| e.to_string())?;
    println!("\n== This PC");
    for i in local::insights::compute(&conn, from, to).map_err(|e| e.to_string())? {
        println!("  [{:?}] {}\n      {}", i.severity, i.title, i.detail);
    }
    let conn = servers::db::open(&paths::servers_db()).map_err(|e| e.to_string())?;
    if !servers::db::monitoring_enabled(&conn) {
        println!("\nServers over SSH are off.");
        return Ok(());
    }
    for s in servers::db::servers(&conn).map_err(|e| e.to_string())? {
        println!("\n== {}  [{}] {}", s.alias, if s.label.is_empty() { "Not checked" } else { &s.label }, s.detail);
        for i in servers::insights::compute(&conn, &s, from, to).map_err(|e| e.to_string())? {
            println!("  [{:?}] {}\n      {}", i.level, i.title, i.detail);
        }
    }
    Ok(())
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("--collector") => {
            if let Err(e) = collector::run() {
                log(&format!("recorder error: {e}"));
            }
        }
        Some("--report") => {
            let hours = args.get(1).and_then(|h| h.parse().ok()).unwrap_or(24);
            if let Err(e) = report(hours) {
                eprintln!("report failed: {e}");
                std::process::exit(1);
            }
        }
        Some("--probe") => {
            attach_console();
            let Some(alias) = args.get(1) else {
                eprintln!("usage: JPerformanceUnified.exe --probe <host alias>");
                std::process::exit(2);
            };
            if let Err(e) = servers::collector::probe_cli(alias) {
                eprintln!("{alias}: {e}");
                std::process::exit(1);
            }
        }
        Some(flag @ ("--check-update" | "--update")) => {
            attach_console();
            if let Err(e) = update::cli(flag == "--update") {
                eprintln!("{e}");
                std::process::exit(1);
            }
        }
        _ => {
            if let (Some("--after-update"), Some(pid)) = (args.first().map(String::as_str), args.get(1).and_then(|p| p.parse().ok())) {
                update::wait_for_old(pid);
            }
            update::cleanup_after_update();
            autostart::refresh_path();
            paths::import_legacy();
            let smoke = args.first().is_some_and(|a| a == "--smoke");
            if let Err(e) = collector::spawn_detached() {
                log(&format!("could not start the recorder: {e}"));
            }
            if let Err(e) = api::run(smoke) {
                log(&format!("window error: {e}"));
            }
        }
    }
}
