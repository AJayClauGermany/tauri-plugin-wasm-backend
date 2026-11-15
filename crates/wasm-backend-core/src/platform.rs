//! The seam between the module registry and the app framework it runs in.
//!
//! Everything framework specific (event delivery, standard directories,
//! threads) goes through [`Platform`]. The Tauri plugin implements it on top
//! of an `AppHandle`; other frameworks (GPUI, a CLI, a server, tests)
//! implement it on top of their own event system, or use [`EventBus`] if
//! they don't have one.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use serde_json::Value;

use crate::Result;

/// Callback receiving the JSON payload of an app event.
pub type EventHandler = Box<dyn Fn(Value) + Send + Sync + 'static>;

/// Standard directories module paths and WASI preopens can be relative to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BaseDir {
    /// `$APPDATA`; the default for preopens and the external plugins dir.
    AppData,
    /// `$APPLOCALDATA`.
    AppLocalData,
    /// `$APPCACHE`.
    AppCache,
    /// `$APPCONFIG`.
    AppConfig,
    /// `$RESOURCE`; relative module paths in release builds.
    Resource,
    /// `$TEMP`.
    Temp,
}

impl BaseDir {
    /// The `$VAR` prefix of the directory in configuration paths.
    pub const fn variable(self) -> &'static str {
        match self {
            BaseDir::AppData => "$APPDATA",
            BaseDir::AppLocalData => "$APPLOCALDATA",
            BaseDir::AppCache => "$APPCACHE",
            BaseDir::AppConfig => "$APPCONFIG",
            BaseDir::Resource => "$RESOURCE",
            BaseDir::Temp => "$TEMP",
        }
    }

    pub const ALL: [BaseDir; 6] = [
        BaseDir::AppData,
        BaseDir::AppLocalData,
        BaseDir::AppCache,
        BaseDir::AppConfig,
        BaseDir::Resource,
        BaseDir::Temp,
    ];
}

/// What the registry needs from the host app.
///
/// Implementations must be cheap to call from any thread: modules emit
/// events and listeners fire from WASM calls running on arbitrary threads.
pub trait Platform: Send + Sync + 'static {
    /// Handle returned by [`Platform::listen`], used to unsubscribe.
    type ListenerId: Send + 'static;

    /// Deliver an event to the app (frontend and native listeners). Called
    /// for events emitted by modules (after their capabilities were checked)
    /// and for lifecycle events such as
    /// [`EVENT_MODULE_LOADED`](crate::EVENT_MODULE_LOADED).
    fn emit(&self, event: &str, payload: Value) -> std::result::Result<(), String>;

    /// Subscribe to app events named `event`, from any source. Used to
    /// deliver the events a module listens to (`capabilities.events.listen`).
    fn listen(&self, event: &str, handler: EventHandler) -> Self::ListenerId;

    /// Undo a [`Platform::listen`].
    fn unlisten(&self, id: Self::ListenerId);

    /// Absolute path of a standard directory.
    fn base_dir(&self, base: BaseDir) -> Result<PathBuf>;

    /// Resolve a relative `path` against a standard directory. Override it
    /// when the platform lays files out specially (Tauri maps `..` in
    /// resource paths to `_up_`, like its bundler does).
    fn resolve(&self, path: &Path, base: BaseDir) -> Result<PathBuf> {
        Ok(self.base_dir(base)?.join(path))
    }

    /// Run blocking work (a WASM call) off the current thread: event
    /// handlers (WASM never runs inline, as the emitter may be the module
    /// itself) and [`Backend::invoke_async`](crate::Backend::invoke_async).
    /// Defaults to a new OS thread; point it at your runtime's blocking pool
    /// (e.g. `tokio::task::spawn_blocking`) if you have one. If a task is
    /// dropped without running, the call waiting for it fails.
    fn spawn_blocking(&self, task: Box<dyn FnOnce() + Send + 'static>) {
        std::thread::spawn(task);
    }
}

/// Resolve a configuration path that may start with a `$APPDATA`-style
/// variable. Other relative paths are resolved against `default`.
pub fn resolve_dir<P: Platform + ?Sized>(
    platform: &P,
    path: &Path,
    default: BaseDir,
) -> Result<PathBuf> {
    let s = path.to_string_lossy();
    for base in BaseDir::ALL {
        if let Some(rest) = s.strip_prefix(base.variable()) {
            let rest = rest.trim_start_matches(['/', '\\']);
            return platform.resolve(Path::new(rest), base);
        }
    }
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        platform.resolve(path, default)
    }
}

/// A minimal in-process event bus, for platforms without an event system of
/// their own (e.g. a GPUI app that forwards events to its views). `emit`
/// calls the listeners registered for that exact event name.
#[derive(Clone, Default)]
pub struct EventBus {
    inner: Arc<Mutex<EventBusInner>>,
}

type SharedHandler = Arc<dyn Fn(Value) + Send + Sync>;

#[derive(Default)]
struct EventBusInner {
    next_id: u64,
    listeners: BTreeMap<u64, (String, SharedHandler)>,
}

impl EventBus {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a listener and return its id.
    pub fn listen(&self, event: &str, handler: EventHandler) -> u64 {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.next_id += 1;
        let id = inner.next_id;
        inner
            .listeners
            .insert(id, (event.to_string(), Arc::from(handler)));
        id
    }

    pub fn unlisten(&self, id: u64) {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .listeners
            .remove(&id);
    }

    /// Call every listener of `event`. Listeners run on the calling thread,
    /// outside the bus lock, so they may (un)subscribe.
    pub fn emit(&self, event: &str, payload: &Value) {
        let handlers: Vec<_> = self
            .inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .listeners
            .values()
            .filter(|(name, _)| name == event)
            .map(|(_, h)| h.clone())
            .collect();
        for handler in handlers {
            handler(payload.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn event_bus() {
        let bus = EventBus::new();
        let hits = Arc::new(AtomicUsize::new(0));
        let h = hits.clone();
        let id = bus.listen(
            "a",
            Box::new(move |v| {
                assert_eq!(v, json!(1));
                h.fetch_add(1, Ordering::SeqCst);
            }),
        );
        bus.emit("a", &json!(1));
        bus.emit("b", &json!(1));
        bus.unlisten(id);
        bus.emit("a", &json!(1));
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }

    struct Dirs;
    impl Platform for Dirs {
        type ListenerId = ();
        fn emit(&self, _: &str, _: Value) -> std::result::Result<(), String> {
            Ok(())
        }
        fn listen(&self, _: &str, _: EventHandler) {}
        fn unlisten(&self, _: ()) {}
        fn base_dir(&self, base: BaseDir) -> Result<PathBuf> {
            Ok(PathBuf::from("/base").join(base.variable().trim_start_matches('$')))
        }
    }

    #[test]
    fn resolves_dirs() {
        let r = |p: &str| resolve_dir(&Dirs, Path::new(p), BaseDir::AppData).unwrap();
        assert_eq!(r("$TEMP/x"), PathBuf::from("/base/TEMP/x"));
        assert_eq!(r("$APPCACHE"), PathBuf::from("/base/APPCACHE/"));
        assert_eq!(r("rel"), PathBuf::from("/base/APPDATA/rel"));
        #[cfg(unix)]
        assert_eq!(r("/abs"), PathBuf::from("/abs"));
    }
}
