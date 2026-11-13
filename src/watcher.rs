//! Hot reload: watch the directories containing module files and reload
//! modules whose file content changed.
//!
//! Directories (not files) are watched because build tools often replace
//! files by writing a temporary file and renaming it over the original.

use std::{
    collections::{BTreeSet, HashSet},
    path::{Path, PathBuf},
    sync::mpsc,
    time::Duration,
};

use notify::{RecursiveMode, Watcher as _};
use tauri::Runtime;

use crate::backend::WasmBackend;

/// Changes within this window are handled together.
const DEBOUNCE: Duration = Duration::from_millis(150);

pub(crate) struct Watcher {
    _watcher: notify::RecommendedWatcher,
    dirs: BTreeSet<PathBuf>,
}

/// Normalize a path for comparisons (resolves `..` and symlinks of the
/// parent directory, which exists even when the file was just removed).
pub(crate) fn normalize(path: &Path) -> PathBuf {
    match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) => std::fs::canonicalize(parent)
            .map(|p| p.join(name))
            .unwrap_or_else(|_| path.to_path_buf()),
        _ => path.to_path_buf(),
    }
}

/// Make the watcher match the current set of watched modules.
pub(crate) fn sync<R: Runtime>(backend: &WasmBackend<R>) {
    let mut dirs = BTreeSet::new();
    for module in backend.read_modules().values() {
        if let (true, Some(path)) = (module.watched(), module.path())
            && let Some(parent) = normalize(path).parent()
        {
            dirs.insert(parent.to_path_buf());
        }
    }
    for (_, path, _) in backend
        .0
        .pending
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .values()
    {
        if let Some(parent) = normalize(path).parent() {
            dirs.insert(parent.to_path_buf());
        }
    }
    if backend.0.hot_reload
        && let Some(ext) = &backend.0.external
    {
        dirs.insert(
            normalize(&ext.dir.join("x"))
                .parent()
                .unwrap()
                .to_path_buf(),
        );
    }

    let mut guard = backend.0.watcher.lock().unwrap_or_else(|e| e.into_inner());
    if guard.as_ref().map(|w| &w.dirs) == Some(&dirs) {
        return;
    }
    if dirs.is_empty() {
        *guard = None;
        return;
    }

    let (tx, rx) = mpsc::channel::<PathBuf>();
    let mut watcher =
        match notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            if let Ok(event) = res
                && !event.kind.is_access()
            {
                for path in event.paths {
                    let _ = tx.send(path);
                }
            }
        }) {
            Ok(w) => w,
            Err(e) => {
                log::error!("failed to start the WASM module watcher: {e}");
                return;
            }
        };
    for dir in &dirs {
        match watcher.watch(dir, RecursiveMode::NonRecursive) {
            Ok(()) => log::debug!("watching {} for WASM module changes", dir.display()),
            Err(e) => log::warn!("can't watch {}: {e}", dir.display()),
        }
    }

    let weak = std::sync::Arc::downgrade(&backend.0);
    std::thread::Builder::new()
        .name("wasm-backend-watcher".into())
        .spawn(move || {
            // Ends when the watcher (and with it the sender) is dropped.
            while let Ok(first) = rx.recv() {
                let mut changed = HashSet::from([normalize(&first)]);
                let mut disconnected = false;
                loop {
                    match rx.recv_timeout(DEBOUNCE) {
                        Ok(p) => {
                            changed.insert(normalize(&p));
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => break,
                        Err(mpsc::RecvTimeoutError::Disconnected) => {
                            disconnected = true;
                            break;
                        }
                    }
                }
                let Some(inner) = weak.upgrade() else { return };
                handle_changes(&WasmBackend(inner), &changed);
                if disconnected {
                    return;
                }
            }
        })
        .expect("failed to spawn watcher thread");

    *guard = Some(Watcher {
        _watcher: watcher,
        dirs,
    });
}

fn handle_changes<R: Runtime>(backend: &WasmBackend<R>, changed: &HashSet<PathBuf>) {
    let modules: Vec<_> = backend.read_modules().values().cloned().collect();
    for module in modules {
        let Some(path) = module.path() else { continue };
        if !module.watched() || !changed.contains(&normalize(path)) || !path.exists() {
            continue;
        }
        if let Err(e) = backend.reload_module(&module, true) {
            backend.report_error(module.name(), &e);
        }
    }

    let pending: Vec<String> = backend
        .0
        .pending
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .filter(|(_, (_, path, _))| changed.contains(&normalize(path)) && path.exists())
        .map(|(name, _)| name.clone())
        .collect();
    for name in pending {
        backend.retry_pending(&name);
    }

    if let Some(ext) = &backend.0.external {
        let dir = normalize(&ext.dir.join("x"));
        let dir = dir.parent().unwrap();
        if changed.iter().any(|p| p.parent() == Some(dir)) {
            backend.sync_external();
        }
    }
}
