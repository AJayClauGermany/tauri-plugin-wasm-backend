//! Run backend logic as WebAssembly inside a Tauri app.
//!
//! Backend modules are hot-reloadable, sandboxed WASM binaries that are
//! called from the frontend the same way Rust commands are, can emit and
//! listen to events, and can call back into the app through host commands,
//! all restricted by capabilities. Two engines are supported:
//!
//! - **WIT components** (`component` feature), run by wasmtime. Exported
//!   functions are commands; arguments are mapped by parameter name.
//! - **Extism plugins** (`extism` feature), for toolchains that don't target
//!   the component model yet. Exported functions are commands taking and
//!   returning JSON.
//!
//! ```no_run
//! use tauri_plugin_wasm_backend::{Builder, Capabilities, ModuleConfig};
//!
//! let plugin: tauri::plugin::TauriPlugin<tauri::Wry, _> = Builder::new()
//!     .module(
//!         "greeter",
//!         ModuleConfig::from_path("../wasm/dist/greeter.wasm").capabilities(
//!             Capabilities::default()
//!                 .allow_emit("greeter://*")
//!                 .allow_host_command("now"),
//!         ),
//!     )
//!     .host_command("now", |_ctx, _args: ()| {
//!         Ok::<_, String>(
//!             std::time::SystemTime::now()
//!                 .duration_since(std::time::UNIX_EPOCH)
//!                 .unwrap()
//!                 .as_millis() as u64,
//!         )
//!     })
//!     .build();
//! // tauri::Builder::default().plugin(plugin)
//! ```

// Without any engine, modules can't be instantiated and the host plumbing is unused.
#![cfg_attr(not(any(feature = "component", feature = "extism")), allow(dead_code))]

use std::{collections::HashMap, fmt::Display, path::PathBuf, sync::Arc};

use serde::{Serialize, de::DeserializeOwned};
use tauri::{Manager, Runtime, Wry, path::BaseDirectory, plugin::TauriPlugin};

mod backend;
mod capabilities;
mod commands;
mod config;
mod engine;
mod error;
#[cfg(feature = "hot-reload")]
mod watcher;

pub use backend::{
    EVENT_MODULE_ERROR, EVENT_MODULE_LOADED, EVENT_MODULE_RELOADED, EVENT_MODULE_UNLOADED,
    HostCallContext, ModuleError, ModuleInfo, WasmBackend,
};
pub use capabilities::{Capabilities, EventCapabilities, Preopen, WasiCapabilities};
pub use commands::InvokeScope;
pub use config::{Config, EngineKind, ExternalConfig, ModuleConfig};
pub use engine::{CommandInfo, ParamInfo};
pub use error::{Error, Result};

/// Extensions to [`tauri::App`], [`tauri::AppHandle`] and [`tauri::Window`]
/// to access the WASM backend.
pub trait WasmBackendExt<R: Runtime> {
    fn wasm_backend(&self) -> &WasmBackend<R>;
}

impl<R: Runtime, T: Manager<R>> WasmBackendExt<R> for T {
    fn wasm_backend(&self) -> &WasmBackend<R> {
        self.state::<WasmBackend<R>>().inner()
    }
}

/// Builds the plugin. Configuration from `plugins.wasm-backend` in
/// `tauri.conf.json` is merged with the configuration given here (modules
/// registered here win on name conflicts).
pub struct Builder<R: Runtime = Wry> {
    config: Config,
    host_commands: HashMap<String, backend::HostCommandFn<R>>,
    typescript_out_dir: Option<PathBuf>,
}

impl<R: Runtime> Default for Builder<R> {
    fn default() -> Self {
        Self::new()
    }
}

impl<R: Runtime> Builder<R> {
    pub fn new() -> Self {
        Self {
            config: Config::default(),
            host_commands: HashMap::new(),
            typescript_out_dir: None,
        }
    }

    /// Register a module.
    pub fn module(mut self, name: impl Into<String>, config: ModuleConfig) -> Self {
        self.config.modules.insert(name.into(), config);
        self
    }

    /// Load third-party plugins from a directory.
    pub fn external(mut self, config: ExternalConfig) -> Self {
        self.config.external = Some(config);
        self
    }

    /// Enable or disable hot reload (default: enabled in debug builds).
    pub fn hot_reload(mut self, enabled: bool) -> Self {
        self.config.hot_reload = Some(enabled);
        self
    }

    /// In debug builds, write TypeScript bindings for every module to
    /// `<dir>/<module>.ts` whenever it is loaded or reloaded. Relative paths
    /// are resolved against the current directory (`src-tauri` under
    /// `tauri dev`).
    pub fn typescript_out_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.typescript_out_dir = Some(dir.into());
        self
    }

    /// Register a host command that modules can call through the host
    /// `invoke` function, if their capabilities allow it. Arguments are
    /// deserialized from JSON, like Tauri command arguments, and the result
    /// is serialized to JSON.
    pub fn host_command<A, T, E, F>(mut self, name: impl Into<String>, f: F) -> Self
    where
        A: DeserializeOwned,
        T: Serialize,
        E: Display,
        F: Fn(HostCallContext<R>, A) -> std::result::Result<T, E> + Send + Sync + 'static,
    {
        let name = name.into();
        let command = name.clone();
        self.host_commands.insert(
            name,
            Arc::new(move |ctx, args| {
                let args = serde_json::from_value(args)
                    .map_err(|e| format!("invalid arguments for host command `{command}`: {e}"))?;
                let out = f(ctx, args).map_err(|e| e.to_string())?;
                serde_json::to_value(out).map_err(|e| e.to_string())
            }),
        );
        self
    }

    pub fn build(self) -> TauriPlugin<R, Option<Config>> {
        let Builder {
            config: builder_config,
            host_commands,
            typescript_out_dir,
        } = self;
        tauri::plugin::Builder::<R, Option<Config>>::new("wasm-backend")
            .invoke_handler(tauri::generate_handler![
                commands::invoke,
                commands::list_modules,
                commands::reload_module,
                commands::typescript,
            ])
            .setup(move |app, api| {
                let mut config = api.config().clone().unwrap_or_default();
                config.modules.extend(builder_config.modules);
                if builder_config.external.is_some() {
                    config.external = builder_config.external;
                }
                if builder_config.hot_reload.is_some() {
                    config.hot_reload = builder_config.hot_reload;
                }
                let hot_reload = config.hot_reload.unwrap_or(cfg!(debug_assertions));
                if hot_reload && !cfg!(feature = "hot-reload") {
                    log::warn!(
                        "hot reload requested but the `hot-reload` feature of tauri-plugin-wasm-backend is disabled"
                    );
                }

                let external = match &config.external {
                    Some(ext) => {
                        let dir = backend::resolve_dir(app, &ext.dir, BaseDirectory::AppData)?;
                        // Create it before the watcher starts so it can be watched.
                        std::fs::create_dir_all(&dir)?;
                        Some(backend::External {
                            dir,
                            config: ext.clone(),
                        })
                    }
                    None => None,
                };
                let typescript_dir = typescript_out_dir
                    .or(config.typescript_out_dir.clone())
                    .filter(|_| cfg!(debug_assertions))
                    .map(|d| std::env::current_dir().map(|c| c.join(d)))
                    .transpose()?;
                let backend = WasmBackend::new(
                    app.clone(),
                    host_commands,
                    hot_reload,
                    external,
                    typescript_dir,
                );
                app.manage(backend.clone());

                for (name, module) in config.modules {
                    // A broken module shouldn't take the app down; it can be
                    // fixed and hot reloaded.
                    if let Err(e) = backend.load(&name, module) {
                        backend.report_error(&name, &e);
                    }
                }
                backend.sync_external();
                #[cfg(feature = "hot-reload")]
                watcher::sync(&backend);
                Ok(())
            })
            .build()
    }
}

/// Initializes the plugin with the configuration from `tauri.conf.json`.
pub fn init<R: Runtime>() -> TauriPlugin<R, Option<Config>> {
    Builder::new().build()
}
