//! Where JPerformance Unified keeps its data: `%APPDATA%\JPerformance Unified`.
//! This PC and the servers have separate SQLite files there, so history from
//! JPerformance and JServer can be copied in as it is.

use std::path::{Path, PathBuf};

fn appdata() -> PathBuf {
    std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir)
}

/// `%APPDATA%\JPerformance Unified`, created on first use.
pub fn data_dir() -> PathBuf {
    let dir = appdata().join("JPerformance Unified");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// History of this PC.
pub fn local_db() -> PathBuf {
    data_dir().join("this-pc.db")
}

/// History of the servers watched over SSH.
pub fn servers_db() -> PathBuf {
    data_dir().join("servers.db")
}

pub fn lock_path() -> PathBuf {
    data_dir().join("recorder.lock")
}

/// `~/.ssh`, where ssh itself looks for config and keys.
pub fn ssh_dir() -> PathBuf {
    std::env::var_os("USERPROFILE").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(".")).join(".ssh")
}

/// History databases of the two apps this one replaces.
pub fn legacy_local_db() -> PathBuf {
    appdata().join("JPerformance").join("jperformance.db")
}

pub fn legacy_servers_db() -> PathBuf {
    appdata().join("JServer").join("jserver.db")
}

/// What happened to the history of one old app.
#[derive(Clone, Copy, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Import {
    Copied,
    NotFound,
    Failed,
}

/// Copies JPerformance and JServer history the first time this app runs.
/// The old apps and their files are only read, never changed.
pub fn import_legacy() {
    // The window and the recorder can both get here at startup; one copies, the other waits.
    let lock = std::fs::File::options().read(true).write(true).create(true).truncate(false).open(data_dir().join("import.lock"));
    let guard = lock.as_ref().ok().filter(|f| f.lock().is_ok());
    for (old, new, app) in [(legacy_local_db(), local_db(), "JPerformance"), (legacy_servers_db(), servers_db(), "JServer")] {
        match copy_once(&old, &new) {
            Some(Ok(())) => {
                crate::log(&format!("copied {app} history from {}", old.display()));
                note(app, Import::Copied);
            }
            Some(Err(e)) => {
                crate::log(&format!("could not copy {app} history: {e}"));
                note(app, Import::Failed);
            }
            None if !new.exists() => note(app, Import::NotFound),
            None => {}
        }
    }
    if let Some(f) = guard {
        let _ = f.unlock();
    }
}

/// Copies `old` to `new` unless `new` already exists or `old` is missing (then `None`).
fn copy_once(old: &Path, new: &Path) -> Option<Result<(), String>> {
    if new.exists() || !old.exists() {
        return None;
    }
    let ro = rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI;
    let result = rusqlite::Connection::open_with_flags(old, ro)
        .and_then(|c| c.execute("VACUUM INTO ?1", [new.to_string_lossy().as_ref()]).map(|_| ()))
        .map_err(|e| e.to_string());
    if result.is_err() {
        let _ = std::fs::remove_file(new); // drop a partial copy so the next start retries
    }
    Some(result)
}

fn note_path(app: &str) -> PathBuf {
    data_dir().join(format!("imported-{}.txt", app.to_ascii_lowercase()))
}

fn note(app: &str, what: Import) {
    let text = match what {
        Import::Copied => "copied",
        Import::NotFound => "not-found",
        Import::Failed => "failed",
    };
    let _ = std::fs::write(note_path(app), text);
}

/// What happened when history from `app` ("JPerformance" or "JServer") was imported.
pub fn import_result(app: &str) -> Option<Import> {
    match std::fs::read_to_string(note_path(app)).ok()?.trim() {
        "copied" => Some(Import::Copied),
        "not-found" => Some(Import::NotFound),
        "failed" => Some(Import::Failed),
        _ => None,
    }
}
