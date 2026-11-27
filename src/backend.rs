//! The Tauri side of the module registry: a [`Platform`] backed by an
//! [`AppHandle`] and the [`WasmBackend`] handle stored in the app state.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::{AppHandle, Emitter, EventId, Listener, Manager, Runtime, path::BaseDirectory};
use wasm_backend_core::{
    Backend, BaseDir, EventHandler, InactiveModule, ModuleConfig, ModuleInfo, Platform,
};

use crate::{Error, Result};

/// Runs the registry on top of a Tauri app: module events are Tauri events,
/// `$APPDATA`-style paths use Tauri's path resolver and blocking work runs
/// on Tauri's async runtime.
pub struct TauriPlatform<R: Runtime> {
    app: AppHandle<R>,
}

impl<R: Runtime> TauriPlatform<R> {
    pub fn new(app: AppHandle<R>) -> Self {
        Self { app }
    }

    pub fn app(&self) -> &AppHandle<R> {
        &self.app
    }
}

impl<R: Runtime> Platform for TauriPlatform<R> {
    type ListenerId = EventId;

    fn emit(&self, event: &str, payload: Value) -> std::result::Result<(), String> {
        self.app.emit(event, payload).map_err(|e| e.to_string())
    }

    fn listen(&self, event: &str, handler: EventHandler) -> EventId {
        self.app.listen_any(event, move |e| {
            handler(serde_json::from_str(e.payload()).unwrap_or(Value::Null))
        })
    }

    fn unlisten(&self, id: EventId) {
        self.app.unlisten(id);
    }

    fn base_dir(&self, base: BaseDir) -> wasm_backend_core::Result<PathBuf> {
        self.resolve(Path::new(""), base)
    }

    fn resolve(&self, path: &Path, base: BaseDir) -> wasm_backend_core::Result<PathBuf> {
        let base = match base {
            BaseDir::AppData => BaseDirectory::AppData,
            BaseDir::AppLocalData => BaseDirectory::AppLocalData,
            BaseDir::AppCache => BaseDirectory::AppCache,
            BaseDir::AppConfig => BaseDirectory::AppConfig,
            BaseDir::Resource => BaseDirectory::Resource,
            BaseDir::Temp => BaseDirectory::Temp,
        };
        self.app
            .path()
            .resolve(path, base)
            .map_err(wasm_backend_core::Error::platform)
    }

    fn spawn_blocking(&self, task: Box<dyn FnOnce() + Send + 'static>) {
        tauri::async_runtime::spawn_blocking(task);
    }
}

/// Context passed to host commands.
pub struct HostCallContext<R: Runtime> {
    /// The app handle.
    pub app: AppHandle<R>,
    /// Name of the module that invoked the command.
    pub module: String,
}

/// Access to the WASM backend. Get it with
/// [`WasmBackendExt::wasm_backend`](crate::WasmBackendExt::wasm_backend).
pub struct WasmBackend<R: Runtime>(pub(crate) Backend<TauriPlatform<R>>);

impl<R: Runtime> Clone for WasmBackend<R> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<R: Runtime> WasmBackend<R> {
    /// The framework-agnostic registry this wraps.
    pub fn core(&self) -> &Backend<TauriPlatform<R>> {
        &self.0
    }

    /// Invoke a module command, like the frontend's `invoke`. `args` is an
    /// object keyed by parameter name for WIT modules; Extism modules receive
    /// it as their JSON input.
    ///
    /// This runs WASM on the calling thread; from async code, prefer
    /// [`WasmBackend::invoke_async`].
    pub fn invoke(&self, module: &str, command: &str, args: Value) -> Result<Value> {
        Ok(self.0.invoke(module, command, args)?)
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
        Ok(self.0.call(module, command, args)?)
    }

    /// Deliver an event directly to a module's event handler, regardless of
    /// its `events.listen` subscriptions.
    pub fn send_event(&self, module: &str, event: &str, payload: &Value) -> Result<()> {
        Ok(self.0.send_event(module, event, payload)?)
    }

    /// Information about every loaded module. [`WasmBackend::inactive`]
    /// lists the ones that are not.
    pub fn modules(&self) -> Vec<ModuleInfo> {
        self.0.modules()
    }

    /// Every module that is known but not running, and why: switched off,
    /// refused (by the load gate, the external plugins' `allow` and `deny`
    /// lists or its `sha256`) or failed to load. Each entry has the
    /// module's hash and the capabilities it would get, which is what a
    /// plugin manager shows before asking the user.
    pub fn inactive(&self) -> Vec<InactiveModule> {
        self.0.inactive()
    }

    /// Switch a module off: it is unloaded and stays that way, an external
    /// plugin too, until [`WasmBackend::enable`] is called. Its state is
    /// kept.
    ///
    /// This lasts until the app exits. To keep a module off between runs,
    /// remember the user's choice and call this at startup, or set
    /// `"enabled": false` in its configuration.
    pub fn disable(&self, name: &str) -> Result<()> {
        Ok(self.0.disable(name)?)
    }

    /// Load a module [`WasmBackend::inactive`] lists: one that was switched
    /// off, or one that was refused or failed and should be tried again
    /// (e.g. after the user approved it). The load gate is asked as for any
    /// load.
    pub fn enable(&self, name: &str) -> Result<ModuleInfo> {
        Ok(self.0.enable(name)?)
    }

    /// Information about one module.
    pub fn module(&self, name: &str) -> Result<ModuleInfo> {
        Ok(self.0.module(name)?)
    }

    /// TypeScript bindings for a module's commands, built from the types
    /// the module exports. See the README for how to use them.
    pub fn typescript(&self, name: &str) -> Result<String> {
        Ok(self.0.typescript(name)?)
    }

    /// Load (or replace) a module at runtime. Use this to manage plugins
    /// yourself, e.g. after the user picked a file and granted permissions.
    pub fn load(&self, name: &str, config: ModuleConfig) -> Result<ModuleInfo> {
        Ok(self.0.load(name, config)?)
    }

    /// Unload a module and forget it. An external plugin whose file is
    /// still there is loaded again by the next scan of the plugins
    /// directory; use [`WasmBackend::disable`] to keep one off.
    pub fn unload(&self, name: &str) -> Result<()> {
        Ok(self.0.unload(name)?)
    }

    /// Reload a module from its file, even if the file is unchanged (which
    /// resets the module's state).
    pub fn reload(&self, name: &str) -> Result<ModuleInfo> {
        Ok(self.0.reload(name)?)
    }

    /// A module's state store: what it wrote through `tauri:backend/state`
    /// (`tauri_state_*` for Extism), whether or not it is loaded right now.
    pub fn state(&self, module: &str) -> Result<BTreeMap<String, String>> {
        Ok(self.0.state(module)?)
    }

    /// Remove everything a module stored, e.g. when the user uninstalls a
    /// plugin. Unloading a module keeps its state.
    pub fn clear_state(&self, module: &str) -> Result<()> {
        Ok(self.0.clear_state(module)?)
    }

    /// Load new plugins from the external plugins directory and unload the
    /// ones whose file was removed.
    pub fn sync_external(&self) {
        self.0.sync_external()
    }
}
