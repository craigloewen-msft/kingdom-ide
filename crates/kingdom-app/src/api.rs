//! Server functions: the typed bridge between browser and server.
//!
//! Leptos `#[server]` functions compile to a real HTTP call on the client and
//! a direct invocation on the server, sharing one signature. That is the main
//! reason this project is Rust on both ends — there is no hand-written client,
//! no schema to keep in sync, and a domain type change breaks the build rather
//! than failing at runtime.

use kingdom_core::Kingdom;
use leptos::prelude::*;

/// In-memory kingdom state.
///
/// A process-global `Mutex` is the right amount of machinery for a
/// single-user local tool at this stage. It sits behind these server
/// functions, so swapping in SQLite later touches only this module.
#[cfg(feature = "ssr")]
mod store {
    use kingdom_core::Kingdom;
    use std::sync::{Mutex, OnceLock};

    static KINGDOM: OnceLock<Mutex<Kingdom>> = OnceLock::new();

    pub fn get() -> &'static Mutex<Kingdom> {
        KINGDOM.get_or_init(|| Mutex::new(Kingdom::unopened()))
    }
}

/// Returns the currently open kingdom, or an empty one if none is open.
#[server(GetKingdom, "/api")]
pub async fn get_kingdom() -> Result<Kingdom, ServerFnError> {
    let kingdom = store::get()
        .lock()
        .map_err(|e| ServerFnError::new(format!("kingdom state poisoned: {e}")))?
        .clone();
    Ok(kingdom)
}

/// Opens a dev folder as the kingdom: scans it for cities and seats a court.
#[server(OpenKingdom, "/api")]
pub async fn open_kingdom(path: String) -> Result<Kingdom, ServerFnError> {
    use crate::scan::scan_kingdom;
    use std::path::PathBuf;

    let expanded = expand_home(&path);
    let root = PathBuf::from(&expanded);

    if !root.is_dir() {
        return Err(ServerFnError::new(format!(
            "No such folder: {expanded}. Give an absolute path to your dev folder."
        )));
    }

    let cities = scan_kingdom(&root)
        .map_err(|e| ServerFnError::new(format!("Could not read {expanded}: {e}")))?;

    // Cities are real; the court is still fabricated. See `kingdom_core::sample`.
    let (architects, plans, resources) = kingdom_core::sample::populate_court(&cities);

    let kingdom = Kingdom {
        name: root
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("Kingdom")
            .to_string(),
        root: root.to_string_lossy().to_string(),
        cities,
        architects,
        plans,
        resources,
    };

    *store::get()
        .lock()
        .map_err(|e| ServerFnError::new(format!("kingdom state poisoned: {e}")))? = kingdom.clone();

    Ok(kingdom)
}

/// Issues a decree: the King starts a new task, optionally aimed at a city.
///
/// Placeholder — no agent is spawned yet. It returns the acknowledgement the
/// UI echoes back, so the chat dock's full round trip is real even though the
/// work behind it is not.
#[server(StartTask, "/api")]
pub async fn start_task(prompt: String, city: Option<String>) -> Result<String, ServerFnError> {
    let target = city.unwrap_or_else(|| "the kingdom at large".to_string());
    Ok(format!(
        "Decree received for {target}: \"{prompt}\". No architect has been dispatched yet \
         — agent spawning is not implemented."
    ))
}

/// A suggested starting folder, so the King is not typing a path from scratch.
#[server(SuggestRoot, "/api")]
pub async fn suggest_root() -> Result<String, ServerFnError> {
    let home = std::env::var("HOME").unwrap_or_default();
    for candidate in ["dev", "Development", "projects", "code", "src", "repos"] {
        let p = std::path::Path::new(&home).join(candidate);
        if p.is_dir() {
            return Ok(p.to_string_lossy().to_string());
        }
    }
    Ok(home)
}

/// Expands a leading `~` to the user's home directory.
#[cfg(feature = "ssr")]
fn expand_home(path: &str) -> String {
    let trimmed = path.trim();
    if let Some(rest) = trimmed.strip_prefix("~") {
        if let Ok(home) = std::env::var("HOME") {
            return format!("{home}{rest}");
        }
    }
    trimmed.to_string()
}
