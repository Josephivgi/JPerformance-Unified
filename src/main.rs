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

/// `--processes` prints every app with what it started, and the clean-up suggestions. Read-only.
fn processes() -> Result<(), String> {
    attach_console();
    let conn = local::db::open(&paths::local_db()).map_err(|e| e.to_string())?;
    let activity = local::db::activity(&conn).map_err(|e| e.to_string())?;
    let mut sys = local::tree::snapshot();
    std::thread::sleep(std::time::Duration::from_millis(500));
    sys.refresh_processes_specifics(sysinfo::ProcessesToUpdate::All, true, local::tree::refresh_kind());
    let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1) as f64;
    let t = local::tree::Tree::build(&sys, cores, &activity, chrono::Utc::now().timestamp());
    let me = local::tree::my_session();
    let mb = |b: u64| b as f64 / 1_048_576.0;
    println!("
Apps and what they started (heaviest first)");
    for g in t.groups(me).iter().take(15) {
        println!("  {:<40} {:>4} procs {:>8.0} MB {:>5.1}% CPU  idle {} min", g.label.chars().take(40).collect::<String>(), g.sum.procs, mb(g.sum.mem), g.sum.cpu, g.sum.idle / 60);
        for b in g.branches.iter().take(8) {
            println!("      {:<36} {:>4} procs {:>8.0} MB {:>5.1}% CPU  idle {} min", b.label.chars().take(36).collect::<String>(), b.sum.procs, mb(b.sum.mem), b.sum.cpu, b.sum.idle / 60);
        }
    }
    println!("
Clean-up suggestions");
    for s in t.suggestions(me) {
        println!("  [{}] {:<8} {:<40} {:>4} procs {:>8.0} MB idle {} min", if s.checked { "x" } else { " " }, s.kind, s.label.chars().take(40).collect::<String>(), s.sum.procs, mb(s.sum.mem), s.sum.idle / 60);
    }
    let (cu, ct) = local::collector::commit_charge();
    println!("
Commit charge: {:.1} of {:.1} GB", cu as f64 / 1e9, ct as f64 / 1e9);
    Ok(())
}

/// `--storage [folder]` scans a folder and prints what fills it, duplicates and caches. Read-only.
fn storage(root: Option<&String>) -> Result<(), String> {
    attach_console();
    let root = root.cloned().or_else(|| std::env::var("USERPROFILE").ok()).ok_or("No folder given")?;
    let t = std::time::Instant::now();
    local::storage::scan_blocking(root.clone().into())?;
    let st = local::storage::status();
    let r = &st["result"];
    let gb = |v: &serde_json::Value| v.as_f64().unwrap_or(0.0) / 1e9;
    println!("\n{root}: {:.1} GB in {} files, {} folders ({:.1} s). Cloud-only: {:.1} GB. Unreadable folders: {}",
        gb(&r["total"]), r["files"], r["dirs"], t.elapsed().as_secs_f64(), gb(&r["cloud"]), r["denied"]);
    println!("\nDuplicates: {} groups, {:.2} GB could be freed", r["dup_groups"], gb(&r["dup_wasted"]));
    for g in r["dups"].as_array().into_iter().flatten().take(10) {
        println!("  {:.1} MB x{}", g["size"].as_f64().unwrap_or(0.0) / 1e6, g["files"].as_array().map_or(0, |f| f.len()));
        for f in g["files"].as_array().into_iter().flatten() {
            println!("      {}", f["path"].as_str().unwrap_or(""));
        }
    }
    println!("\nLargest files");
    for f in r["largest"].as_array().into_iter().flatten().take(10) {
        println!("  {:>8.2} GB  {}", gb(&f["size"]), f["path"].as_str().unwrap_or(""));
    }
    println!("\nProject build folders");
    for b in r["builds"].as_array().into_iter().flatten().take(10) {
        println!("  {:>8.2} GB  {:<13} {}", gb(&b["size"]), b["kind"].as_str().unwrap_or(""), b["path"].as_str().unwrap_or(""));
    }
    println!("\nCaches");
    for j in r["junk"].as_array().into_iter().flatten() {
        println!("  {:>8.2} GB  {}", gb(&j["size"]), j["label"].as_str().unwrap_or(""));
    }
    println!("\nRecycle Bin: {:.2} GB, {} items", gb(&st["bin"]["size"]), st["bin"]["items"]);
    Ok(())
}

/// `--cleanup` lists every cache with its size. `--cleanup clear <id>...` clears the named ones.
fn cleanup_cli(args: &[String]) -> Result<(), String> {
    attach_console();
    let mb = |v: &serde_json::Value| v.as_f64().unwrap_or(0.0) / 1e6;
    if args.first().map(String::as_str) == Some("clear") {
        if args.len() < 2 {
            return Err("usage: --cleanup clear <id>...".into());
        }
        for r in local::junk::clean_blocking(&args[1..])? {
            println!("{:<34} freed {:>10.1} MB in {} files, {:.1} MB in {} files left in use", r["label"].as_str().unwrap_or(""), mb(&r["freed"]), r["files"], mb(&r["left"]), r["in_use"]);
        }
        return Ok(());
    }
    let t = std::time::Instant::now();
    local::junk::refresh(true);
    while local::junk::busy() {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let list = local::junk::list_json();
    println!("{} caches measured in {:.1} s", list.len(), t.elapsed().as_secs_f64());
    for group in local::junk::GROUPS {
        println!("
{group}");
        let mut rows: Vec<&serde_json::Value> = list.iter().filter(|j| j["group"] == group).collect();
        rows.sort_by(|a, b| b["size"].as_u64().cmp(&a["size"].as_u64()));
        for j in rows {
            println!("  {:>10.1} MB {:>8} files  {}{}  {}  [{}]", mb(&j["size"]), j["files"], if j["rec"] == true { "rec " } else { "    " },
                if j["running"].is_string() { "open" } else { "    " }, j["label"].as_str().unwrap_or(""), j["id"].as_str().unwrap_or(""));
        }
    }
    let dl = local::junk::downloads(0, 10);
    println!("
Downloads: {} items, {:.1} MB  ({})", dl["count"], mb(&dl["size"]), dl["path"].as_str().unwrap_or(""));
    for i in dl["items"].as_array().into_iter().flatten() {
        println!("  {:>10.1} MB  {}", mb(&i["size"]), i["name"].as_str().unwrap_or(""));
    }
    let old = local::junk::downloads(90, 1);
    println!("  not changed for 90 days: {} items, {:.1} MB", old["old_count"], mb(&old["old_size"]));
    println!("
Kept by Windows");
    for s in local::junk::status()["system"].as_array().into_iter().flatten() {
        println!("  {:>10.1} MB  {}  ({})", mb(&s["size"]), s["label"].as_str().unwrap_or(""), s["name"].as_str().unwrap_or(""));
    }
    Ok(())
}

/// `--apps` lists installed apps by size. `--apps uninstall <id>` uninstalls one and waits.
fn apps_cli(args: &[String]) -> Result<(), String> {
    attach_console();
    let wait = |done: &dyn Fn(&serde_json::Value) -> bool| loop {
        let v = local::apps::status();
        if done(&v) {
            return v;
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    };
    if args.first().map(String::as_str) == Some("uninstall") {
        let id = args.get(1).ok_or("usage: --apps uninstall <id>")?;
        local::apps::uninstall(id)?;
        let v = wait(&|v| v["jobs"].as_array().is_some_and(|j| j.iter().any(|j| j["id"] == id.as_str() && j["state"] != "running")));
        println!("{}", serde_json::to_string_pretty(&v["jobs"]).unwrap_or_default());
        return Ok(());
    }
    let t = std::time::Instant::now();
    let v = wait(&|v| v["measuring"] == false && v["measured"].as_u64() > Some(0));
    println!("{} apps, measured in {:.1} s", v["count"], t.elapsed().as_secs_f64());
    for a in v["apps"].as_array().into_iter().flatten().take(args.first().and_then(|n| n.parse().ok()).unwrap_or(25)) {
        println!("{:>9.2} GB  prog {:>8.2}  data {:>8.2}  quiet={:<5} {}  [{}]", a["total"].as_f64().unwrap_or(0.0) / 1e9, a["program"].as_f64().unwrap_or(0.0) / 1e9,
            a["data"].as_f64().unwrap_or(0.0) / 1e9, a["quiet"], a["name"].as_str().unwrap_or(""), a["id"].as_str().unwrap_or(""));
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
        Some("--storage") => {
            if let Err(e) = storage(args.get(1)) {
                eprintln!("{e}");
                std::process::exit(1);
            }
        }
        Some("--cleanup") => {
            if let Err(e) = cleanup_cli(&args[1..]) {
                eprintln!("{e}");
                std::process::exit(1);
            }
        }
        Some("--apps") => {
            if let Err(e) = apps_cli(&args[1..]) {
                eprintln!("{e}");
                std::process::exit(1);
            }
        }
        Some("--processes") => {
            if let Err(e) = processes() {
                eprintln!("{e}");
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
            if smoke {
                paths::set_smoke();
            }
            if let Err(e) = collector::spawn_detached() {
                log(&format!("could not start the recorder: {e}"));
            }
            if let Err(e) = api::run(smoke) {
                log(&format!("window error: {e}"));
            }
        }
    }
}
