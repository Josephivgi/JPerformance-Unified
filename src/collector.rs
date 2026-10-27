//! The background recorder process, `JPerformanceUnified.exe --collector`.
//! It records this PC all the time and, when servers over SSH are turned on,
//! also watches the servers. It keeps running after the window is closed.

use crate::{local, paths, servers};
use std::fs::File;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

fn open_lock() -> std::io::Result<File> {
    File::options().read(true).write(true).create(true).truncate(false).open(paths::lock_path())
}

/// True if a recorder process currently holds the lock file.
pub fn is_running() -> bool {
    let Ok(f) = open_lock() else {
        return false;
    };
    match f.try_lock() {
        Ok(()) => {
            let _ = f.unlock();
            false
        }
        Err(std::fs::TryLockError::WouldBlock) => true,
        Err(std::fs::TryLockError::Error(_)) => false,
    }
}

/// Starts a hidden, detached recorder process if one is not already running.
pub fn spawn_detached() -> std::io::Result<()> {
    use std::os::windows::process::CommandExt;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    if is_running() {
        return Ok(());
    }
    std::process::Command::new(std::env::current_exe()?)
        .arg("--collector")
        .creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW)
        .spawn()?;
    Ok(())
}

pub fn run() -> Result<(), String> {
    let lock = open_lock().map_err(|e| e.to_string())?;
    match lock.try_lock() {
        Ok(()) => {}
        // Another recorder is already running.
        Err(std::fs::TryLockError::WouldBlock) => return Ok(()),
        Err(std::fs::TryLockError::Error(e)) => return Err(format!("lock failed: {e}")),
    }
    paths::import_legacy();
    {
        // History copied from JPerformance may have recording switched off; this app always records.
        let conn = local::db::open(&paths::local_db()).map_err(|e| e.to_string())?;
        let _ = local::db::set_setting(&conn, local::db::KEY_TRACKING, "1");
    }
    crate::log(&format!("recorder started, pid {}", std::process::id()));

    let quit = Arc::new(AtomicBool::new(false));
    let servers_thread = {
        let quit = quit.clone();
        std::thread::spawn(move || watch_servers(&quit))
    };
    if let Err(e) = local::collector::run(&quit) {
        crate::log(&format!("this PC recorder stopped: {e}"));
    }
    quit.store(true, Ordering::SeqCst);
    let _ = servers_thread.join();
    crate::log("recorder stopped");
    drop(lock);
    Ok(())
}

/// Runs the SSH monitor while servers over SSH are turned on, and starts it
/// again when they are turned back on.
fn watch_servers(quit: &Arc<AtomicBool>) {
    let conn = match servers::db::open(&paths::servers_db()) {
        Ok(c) => c,
        Err(e) => {
            crate::log(&format!("could not open servers database: {e}"));
            return;
        }
    };
    while !quit.load(Ordering::SeqCst) {
        if servers::db::monitoring_enabled(&conn) {
            if let Err(e) = servers::collector::run(quit) {
                crate::log(&format!("server monitor stopped: {e}"));
            }
        }
        std::thread::sleep(Duration::from_secs(2));
    }
}
