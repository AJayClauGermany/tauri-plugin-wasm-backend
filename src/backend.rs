//! The module registry: loads modules, routes invokes and events, and
//! implements the host side of the module API.

use std::{
    cell::RefCell,
    collections::{BTreeMap, HashMap},
    hash::{DefaultHasher, Hash, Hasher},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, RwLock, Weak},
    time::SystemTime,
};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::{AppHandle, Emitter, EventId, Listener, Manager, Runtime, path::BaseDirectory};

use crate::{
    Error, Result,
    capabilities::{Capabilities, Preopen},
    config::{EngineKind, ExternalConfig, ModuleConfig},
    engine::{self, CommandInfo, ModuleHost, ModuleInstance},
};

/// Emitted with a [`ModuleInfo`] payload when a module is loaded.
pub const EVENT_MODULE_LOADED: &str = "wasm-backend://module-loaded";
/// Emitted with a [`ModuleInfo`] payload when a module is reloaded after its
/// file changed (or [`WasmBackend::reload`] was called).
pub const EVENT_MODULE_RELOADED: &str = "wasm-backend://module-reloaded";
/// Emitted with the module name when a module is unloaded.
pub const EVENT_MODULE_UNLOADED: &str = "wasm-backend://module-unloaded";
/// Emitted with a [`ModuleError`] payload when loading or reloading fails.
pub const EVENT_MODULE_ERROR: &str = "wasm-backend://module-error";

pub(crate) type HostCommandFn<R> =
    Arc<dyn Fn(HostCallContext<R>, Value) -> std::result::Result<Value, String> + Send + Sync>;

/// Context passed to host commands.
pub struct HostCallContext<R: Runtime> {
    /// The app handle.
    pub app: AppHandle<R>,
    /// Name of the module that invoked the command.
    pub module: String,
}

/// Public description of a loaded module.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct ModuleInfo {
    pub name: String,
    /// The engine running the module (never `auto`).
    pub engine: EngineKind,
    /// File the module was loaded from, if any.
    pub path: Option<PathBuf>,
    /// Whether the module was loaded from the external plugins directory.
    pub external: bool,
    /// Whether the frontend may invoke the module without an explicit scope.
    pub exposed: bool,
    /// Whether the module is reloaded when its file changes.
    pub watched: bool,
    /// Incremented on every reload.
    pub generation: u64,
    pub commands: Vec<CommandInfo>,
    pub capabilities: Capabilities,
}

/// Payload of [`EVENT_MODULE_ERROR`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct ModuleError {
    pub module: String,
    pub error: String,
}

/// Metadata, kept apart from the instance so that reading it never waits
/// for (or deadlocks with) a running call.
struct Meta {
    info: ModuleInfo,
    typescript: String,
    hash: u64,
}

pub(crate) struct Module {
    name: String,
    config: ModuleConfig,
    path: Option<PathBuf>,
    external: bool,
    watched: bool,
    instance: Mutex<Box<dyn ModuleInstance>>,
    meta: RwLock<Meta>,
    listeners: Mutex<Vec<EventId>>,
}

#[cfg_attr(not(feature = "hot-reload"), allow(dead_code))]
impl Module {
    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    pub(crate) fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub(crate) fn watched(&self) -> bool {
        self.watched
    }

    fn external(&self) -> bool {
        self.external
    }

    fn meta(&self) -> std::sync::RwLockReadGuard<'_, Meta> {
        self.meta.read().unwrap_or_else(|e| e.into_inner())
    }

    fn instance(&self) -> std::sync::MutexGuard<'_, Box<dyn ModuleInstance>> {
        self.instance.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Resolved external plugins configuration.
pub(crate) struct External {
    pub(crate) dir: PathBuf,
    pub(crate) config: ExternalConfig,
}

pub(crate) struct Inner<R: Runtime> {
    pub(crate) app: AppHandle<R>,
    modules: RwLock<BTreeMap<String, Arc<Module>>>,
    host_commands: HashMap<String, HostCommandFn<R>>,
    pub(crate) hot_reload: bool,
    pub(crate) external: Option<External>,
    typescript_dir: Option<PathBuf>,
    /// Modules that failed to load from a file; retried when it changes.
    pub(crate) pending: Mutex<BTreeMap<String, (ModuleConfig, PathBuf, bool)>>,
    #[cfg(feature = "hot-reload")]
    pub(crate) watcher: Mutex<Option<crate::watcher::Watcher>>,
}

/// Access to the WASM backend. Get it with
/// [`WasmBackendExt::wasm_backend`](crate::WasmBackendExt::wasm_backend).
pub struct WasmBackend<R: Runtime>(pub(crate) Arc<Inner<R>>);

impl<R: Runtime> Clone for WasmBackend<R> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

thread_local! {
    /// Modules currently executing on this thread, to detect reentrancy.
    static CALL_STACK: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

struct CallGuard;

impl CallGuard {
    fn enter(module: &str) -> Result<Self> {
        CALL_STACK.with(|s| {
            let mut s = s.borrow_mut();
            if s.iter().any(|m| m == module) {
                return Err(Error::Reentrant(module.to_string()));
            }
            s.push(module.to_string());
            Ok(CallGuard)
        })
    }
}

impl Drop for CallGuard {
    fn drop(&mut self) {
        CALL_STACK.with(|s| {
            s.borrow_mut().pop();
        });
    }
}

/// Bridges module host calls to the app. Holds a weak reference so modules
/// don't keep the registry alive.
struct Bridge<R: Runtime>(Weak<Inner<R>>);

impl<R: Runtime> engine::Host for Bridge<R> {
    fn emit(&self, event: &str, payload: Value) -> std::result::Result<(), String> {
        let inner = self.0.upgrade().ok_or("app is shutting down")?;
        if !valid_event_name(event) {
            return Err(format!(
                "invalid event name `{event}`: only alphanumeric characters, `-`, `/`, `:` and `_` are allowed"
            ));
        }
        inner.app.emit(event, payload).map_err(|e| e.to_string())
    }

    fn invoke(
        &self,
        module: &str,
        command: &str,
        args: Value,
    ) -> std::result::Result<Value, String> {
        let inner = self.0.upgrade().ok_or("app is shutting down")?;
        let f = inner
            .host_commands
            .get(command)
            .ok_or_else(|| format!("unknown host command `{command}`"))?
            .clone();
        f(
            HostCallContext {
                app: inner.app.clone(),
                module: module.to_string(),
            },
            args,
        )
    }
}

pub(crate) fn valid_event_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '-' | '/' | ':' | '_'))
}

fn hash(bytes: &[u8]) -> u64 {
    let mut h = DefaultHasher::new();
    bytes.hash(&mut h);
    h.finish()
}

impl<R: Runtime> WasmBackend<R> {
    pub(crate) fn new(
        app: AppHandle<R>,
        host_commands: HashMap<String, HostCommandFn<R>>,
        hot_reload: bool,
        external: Option<External>,
        typescript_dir: Option<PathBuf>,
    ) -> Self {
        Self(Arc::new(Inner {
            app,
            modules: RwLock::new(BTreeMap::new()),
            host_commands,
            hot_reload,
            external,
            typescript_dir,
            pending: Mutex::new(BTreeMap::new()),
            #[cfg(feature = "hot-reload")]
            watcher: Mutex::new(None),
        }))
    }

    /// Invoke a module command, like the frontend's `invoke`. `args` is an
    /// object keyed by parameter name for WIT modules; Extism modules receive
    /// it as their JSON input.
    ///
    /// This runs WASM on the calling thread; from async code, prefer
    /// [`WasmBackend::invoke_async`].
    pub fn invoke(&self, module: &str, command: &str, args: Value) -> Result<Value> {
        let m = self.get(module)?;
        let _guard = CallGuard::enter(module)?;
        m.instance().call(command, args)
    }

    /// [`WasmBackend::invoke`] on a blocking thread.
    pub async fn invoke_async(&self, module: &str, command: &str, args: Value) -> Result<Value> {
        let this = self.clone();
        let (module, command) = (module.to_string(), command.to_string());
        tauri::async_runtime::spawn_blocking(move || this.invoke(&module, &command, args))
            .await
            .map_err(|e| Error::Runtime(e.to_string()))?
    }

    /// Typed variant of [`WasmBackend::invoke`].
    pub fn call<A: Serialize, T: for<'de> Deserialize<'de>>(
        &self,
        module: &str,
        command: &str,
        args: A,
    ) -> Result<T> {
        let args = serde_json::to_value(args).map_err(|e| Error::InvalidArgs {
            command: command.to_string(),
            message: e.to_string(),
        })?;
        let out = self.invoke(module, command, args)?;
        serde_json::from_value(out).map_err(|e| Error::Runtime(format!("unexpected output: {e}")))
    }

    /// Deliver an event directly to a module's event handler, regardless of
    /// its `events.listen` subscriptions.
    pub fn send_event(&self, module: &str, event: &str, payload: &Value) -> Result<()> {
        let m = self.get(module)?;
        let _guard = CallGuard::enter(module)?;
        m.instance().on_event(event, payload)
    }

    /// Information about every loaded module.
    pub fn modules(&self) -> Vec<ModuleInfo> {
        let modules: Vec<_> = self.read_modules().values().cloned().collect();
        modules.iter().map(|m| m.meta().info.clone()).collect()
    }

    /// Information about one module.
    pub fn module(&self, name: &str) -> Result<ModuleInfo> {
        let m = self.get(name)?;
        let info = m.meta().info.clone();
        Ok(info)
    }

    /// TypeScript bindings for a module's commands, built from the types
    /// the module exports. See the README for how to use them.
    pub fn typescript(&self, name: &str) -> Result<String> {
        let m = self.get(name)?;
        let body = m.meta().typescript.clone();
        Ok(format!(
            "// Generated by tauri-plugin-wasm-backend from module `{name}`. Do not edit.\n\
             import {{ invokeModule }} from 'tauri-plugin-wasm-backend-api'\n\n\
             export const {ident} = {{\n{body}}}\n\nexport default {ident}\n",
            ident = engine::naming::to_camel(name),
        ))
    }

    /// Load (or replace) a module at runtime. Use this to manage plugins
    /// yourself, e.g. after the user picked a file and granted permissions.
    pub fn load(&self, name: &str, config: ModuleConfig) -> Result<ModuleInfo> {
        self.load_inner(name, config, false)
    }

    /// Unload a module.
    pub fn unload(&self, name: &str) -> Result<()> {
        self.0
            .pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(name);
        let m = self
            .0
            .modules
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(name)
            .ok_or_else(|| Error::ModuleNotFound(name.to_string()))?;
        for id in m
            .listeners
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .drain(..)
        {
            self.0.app.unlisten(id);
        }
        #[cfg(feature = "hot-reload")]
        crate::watcher::sync(self);
        let _ = self.0.app.emit(EVENT_MODULE_UNLOADED, name);
        Ok(())
    }

    /// Reload a module from its file, even if the file is unchanged (which
    /// resets the module's state).
    pub fn reload(&self, name: &str) -> Result<ModuleInfo> {
        let m = self.get(name)?;
        let result = self
            .reload_module(&m, false)
            .map(|info| info.expect("forced reloads always reload"));
        if let Err(e) = &result {
            self.report_error(name, e);
        }
        result
    }

    pub(crate) fn reload_module(
        &self,
        m: &Module,
        only_if_changed: bool,
    ) -> Result<Option<ModuleInfo>> {
        let Some(path) = &m.path else {
            return Err(Error::NotAllowed(format!(
                "module `{}` is embedded and can't be reloaded",
                m.name
            )));
        };
        let bytes = std::fs::read(path)?;
        let new_hash = hash(&bytes);
        if only_if_changed && m.meta().hash == new_hash {
            return Ok(None);
        }
        // Compile and instantiate outside the lock so calls keep being served
        // by the old instance meanwhile.
        let started = SystemTime::now();
        let (instance, kind) = self.instantiate(&m.name, &m.config, &bytes)?;
        let _guard = CallGuard::enter(&m.name)?;
        // The module may have been unloaded or replaced while compiling.
        let current = self.get(&m.name)?;
        if !std::ptr::eq(Arc::as_ptr(&current), m) {
            return Err(Error::ModuleNotFound(m.name.clone()));
        }
        let commands = instance.commands();
        let typescript = instance.typescript(&m.name).unwrap_or_default();
        *m.instance() = instance;
        let info = {
            let mut meta = m.meta.write().unwrap_or_else(|e| e.into_inner());
            meta.info.commands = commands;
            meta.info.engine = kind;
            meta.info.generation += 1;
            meta.typescript = typescript;
            meta.hash = new_hash;
            meta.info.clone()
        };
        log::info!(
            "reloaded WASM module `{}` in {:?}",
            m.name,
            started.elapsed().unwrap_or_default()
        );
        self.write_typescript(&m.name);
        let _ = self.0.app.emit(EVENT_MODULE_RELOADED, &info);
        Ok(Some(info))
    }

    pub(crate) fn report_error(&self, module: &str, error: &Error) {
        log::error!("WASM module `{module}`: {error}");
        let _ = self.0.app.emit(
            EVENT_MODULE_ERROR,
            ModuleError {
                module: module.to_string(),
                error: error.to_string(),
            },
        );
    }

    pub(crate) fn read_modules(
        &self,
    ) -> std::sync::RwLockReadGuard<'_, BTreeMap<String, Arc<Module>>> {
        self.0.modules.read().unwrap_or_else(|e| e.into_inner())
    }

    pub(crate) fn get(&self, name: &str) -> Result<Arc<Module>> {
        self.read_modules()
            .get(name)
            .cloned()
            .ok_or_else(|| Error::ModuleNotFound(name.to_string()))
    }

    /// Whether the frontend may invoke the module without an explicit scope.
    pub(crate) fn is_exposed(&self, name: &str) -> bool {
        self.get(name)
            .map(|m| m.meta().info.exposed)
            .unwrap_or(false)
    }

    fn instantiate(
        &self,
        name: &str,
        config: &ModuleConfig,
        bytes: &[u8],
    ) -> Result<(Box<dyn ModuleInstance>, EngineKind)> {
        let kind = engine::detect(bytes, config.engine);
        let host = Arc::new(ModuleHost {
            module: name.to_string(),
            capabilities: self.resolve_capabilities(&config.capabilities)?,
            host: Arc::new(Bridge(Arc::downgrade(&self.0))),
        });
        Ok((engine::instantiate(kind, bytes, host)?, kind))
    }

    pub(crate) fn load_inner(
        &self,
        name: &str,
        config: ModuleConfig,
        external: bool,
    ) -> Result<ModuleInfo> {
        self.0
            .pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(name);
        let watch = config.watch.unwrap_or(self.0.hot_reload);
        let retry = (watch && config.bytes.is_none())
            .then(|| {
                config
                    .configured_path()
                    .map(|p| self.resolve_module_path(p, external))
            })
            .flatten()
            .and_then(Result::ok)
            .map(|path| (config.clone(), path));
        let result = self.try_load(name, config, external);
        if result.is_err()
            && let Some((config, path)) = retry
        {
            // Retry when the file shows up or changes (e.g. after a build).
            self.0
                .pending
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(name.to_string(), (config, path, external));
            #[cfg(feature = "hot-reload")]
            crate::watcher::sync(self);
        }
        result
    }

    /// Retry loading a module that previously failed.
    #[cfg_attr(not(feature = "hot-reload"), allow(dead_code))]
    pub(crate) fn retry_pending(&self, name: &str) {
        let entry = self
            .0
            .pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(name)
            .cloned();
        if let Some((config, _, external)) = entry
            && let Err(e) = self.load_inner(name, config, external)
        {
            self.report_error(name, &e);
        }
    }

    fn write_typescript(&self, name: &str) {
        let Some(dir) = &self.0.typescript_dir else {
            return;
        };
        let result = self.typescript(name).and_then(|ts| {
            std::fs::create_dir_all(dir)?;
            let path = dir.join(format!("{name}.ts"));
            // Avoid touching the file (and triggering frontend HMR) needlessly.
            if std::fs::read_to_string(&path).ok().as_deref() != Some(ts.as_str()) {
                std::fs::write(&path, ts)?;
            }
            Ok(())
        });
        if let Err(e) = result {
            log::warn!("failed to write TypeScript bindings for `{name}`: {e}");
        }
    }

    fn try_load(&self, name: &str, config: ModuleConfig, external: bool) -> Result<ModuleInfo> {
        let load_err = |message: String| Error::Load {
            module: name.to_string(),
            message,
        };
        for event in &config.capabilities.events.listen {
            if !valid_event_name(event) {
                return Err(load_err(format!(
                    "invalid event name `{event}` in `events.listen`"
                )));
            }
        }
        let (bytes, path): (Arc<[u8]>, Option<PathBuf>) =
            match (&config.bytes, config.configured_path()) {
                (Some(bytes), _) => (bytes.clone(), None),
                (None, Some(path)) => {
                    let path = self.resolve_module_path(path, external)?;
                    let bytes = std::fs::read(&path)
                        .map_err(|e| load_err(format!("can't read {}: {e}", path.display())))?;
                    (bytes.into(), Some(path))
                }
                (None, None) => return Err(load_err("no `path` configured".into())),
            };

        let started = SystemTime::now();
        let (instance, kind) = self.instantiate(name, &config, &bytes)?;
        let watched = path.is_some() && config.watch.unwrap_or(self.0.hot_reload);
        let info = ModuleInfo {
            name: name.to_string(),
            engine: kind,
            path: path.clone(),
            external,
            exposed: config.expose.unwrap_or(!external),
            watched,
            generation: 0,
            commands: instance.commands(),
            capabilities: config.capabilities.clone(),
        };
        let module = Arc::new(Module {
            name: name.to_string(),
            external,
            watched,
            meta: RwLock::new(Meta {
                info: info.clone(),
                typescript: instance.typescript(name).unwrap_or_default(),
                hash: hash(&bytes),
            }),
            instance: Mutex::new(instance),
            listeners: Mutex::new(Vec::new()),
            config,
            path,
        });
        self.subscribe(&module);

        let previous = self
            .0
            .modules
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(name.to_string(), module);
        if let Some(previous) = previous {
            for id in previous
                .listeners
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .drain(..)
            {
                self.0.app.unlisten(id);
            }
        }
        #[cfg(feature = "hot-reload")]
        crate::watcher::sync(self);

        log::info!(
            "loaded WASM module `{name}` ({kind:?}, {} commands) in {:?}",
            info.commands.len(),
            started.elapsed().unwrap_or_default()
        );
        self.write_typescript(name);
        let _ = self.0.app.emit(EVENT_MODULE_LOADED, &info);
        Ok(info)
    }

    /// Load new plugins from the external plugins directory and unload the
    /// ones whose file was removed.
    pub fn sync_external(&self) {
        let Some(ext) = &self.0.external else { return };
        if let Err(e) = std::fs::create_dir_all(&ext.dir) {
            log::error!("can't create plugins directory {}: {e}", ext.dir.display());
            return;
        }
        let mut present = Vec::new();
        match std::fs::read_dir(&ext.dir) {
            Ok(entries) => {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.extension().and_then(|e| e.to_str()) != Some("wasm") || !path.is_file()
                    {
                        continue;
                    }
                    let Some(name) = path
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .map(str::to_string)
                    else {
                        continue;
                    };
                    present.push(name.clone());
                    match self.get(&name) {
                        Ok(m) if !m.external() => {
                            log::warn!(
                                "ignoring external plugin {}: an app module named `{name}` exists",
                                path.display()
                            );
                        }
                        Ok(_) => {}
                        Err(_) => {
                            let config = ModuleConfig {
                                path: Some(path.clone()),
                                dev_path: Some(path.clone()),
                                engine: ext.config.engine,
                                capabilities: ext.config.capabilities.clone(),
                                expose: Some(ext.config.expose),
                                ..Default::default()
                            };
                            if let Err(e) = self.load_inner(&name, config, true) {
                                self.report_error(&name, &e);
                            }
                        }
                    }
                }
            }
            Err(e) => log::error!("can't read plugins directory {}: {e}", ext.dir.display()),
        }
        let gone: Vec<String> = self
            .read_modules()
            .values()
            .filter(|m| m.external() && !present.contains(&m.name))
            .map(|m| m.name.clone())
            .collect();
        for name in gone {
            let _ = self.unload(&name);
        }
    }

    /// Forward subscribed app events to the module.
    fn subscribe(&self, module: &Arc<Module>) {
        let mut ids = module.listeners.lock().unwrap_or_else(|e| e.into_inner());
        for event in &module.config.capabilities.events.listen {
            let backend = Arc::downgrade(&self.0);
            let name = module.name.clone();
            let event_name = event.clone();
            let id = self.0.app.listen_any(event.clone(), move |e| {
                let Some(inner) = backend.upgrade() else {
                    return;
                };
                let backend = WasmBackend(inner);
                let payload: Value = serde_json::from_str(e.payload()).unwrap_or(Value::Null);
                let name = name.clone();
                let event = event_name.clone();
                // Never run WASM inline: the emitter may be the module itself.
                tauri::async_runtime::spawn_blocking(move || {
                    if let Err(err) = backend.send_event(&name, &event, &payload) {
                        log::error!("WASM module `{name}` failed to handle event `{event}`: {err}");
                    }
                });
            });
            ids.push(id);
        }
    }

    fn resolve_module_path(&self, path: &Path, external: bool) -> Result<PathBuf> {
        if path.is_absolute() {
            return Ok(path.to_path_buf());
        }
        if external || cfg!(debug_assertions) {
            // External plugin paths are already absolute; dev paths are
            // relative to the working directory (`src-tauri` under `tauri dev`).
            return Ok(std::env::current_dir()?.join(path));
        }
        Ok(self.0.app.path().resolve(path, BaseDirectory::Resource)?)
    }

    fn resolve_dir(&self, path: &Path, default: BaseDirectory) -> Result<PathBuf> {
        resolve_dir(&self.0.app, path, default)
    }
}

/// Resolve a path that may start with a `$APPDATA`-style variable.
pub(crate) fn resolve_dir<R: Runtime>(
    app: &AppHandle<R>,
    path: &Path,
    default: BaseDirectory,
) -> Result<PathBuf> {
    {
        let s = path.to_string_lossy();
        let vars = [
            ("$APPDATA", BaseDirectory::AppData),
            ("$APPLOCALDATA", BaseDirectory::AppLocalData),
            ("$APPCACHE", BaseDirectory::AppCache),
            ("$APPCONFIG", BaseDirectory::AppConfig),
            ("$RESOURCE", BaseDirectory::Resource),
            ("$TEMP", BaseDirectory::Temp),
        ];
        for (var, base) in vars {
            if let Some(rest) = s.strip_prefix(var) {
                let rest = rest.trim_start_matches(['/', '\\']);
                return Ok(app.path().resolve(rest, base)?);
            }
        }
        if path.is_absolute() {
            Ok(path.to_path_buf())
        } else {
            Ok(app.path().resolve(path, default)?)
        }
    }
}

impl<R: Runtime> WasmBackend<R> {
    fn resolve_capabilities(&self, caps: &Capabilities) -> Result<Capabilities> {
        let mut caps = caps.clone();
        let default = BaseDirectory::AppData;
        caps.wasi.preopens = caps
            .wasi
            .preopens
            .iter()
            .map(|p| {
                let host = self.resolve_dir(&p.host, default)?;
                if !p.read_only {
                    let _ = std::fs::create_dir_all(&host);
                }
                Ok(Preopen { host, ..p.clone() })
            })
            .collect::<Result<_>>()?;
        Ok(caps)
    }
}
