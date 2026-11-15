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
//! The engines, capabilities, registry and hot reload live in the
//! framework-agnostic [`wasm_backend_core`] crate (re-exported here), so the
//! same modules can also be hosted by other Rust apps (e.g. GPUI). This
//! crate adds the Tauri glue: the plugin, IPC commands, permissions and the
//! [`TauriPlatform`].
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

use std::{collections::HashMap, fmt::Display, path::PathBuf};

use serde::{Serialize, de::DeserializeOwned};
use tauri::{Manager, Runtime, Wry, plugin::TauriPlugin};
use wasm_backend_core::{Backend, HostCall, HostCommandFn, TypeScriptOptions};

mod backend;
mod commands;

pub use backend::{HostCallContext, TauriPlatform, WasmBackend};
pub use commands::InvokeScope;
pub use wasm_backend_core::{
    self, Capabilities, CommandInfo, Config, EVENT_MODULE_ERROR, EVENT_MODULE_LOADED,
    EVENT_MODULE_RELOADED, EVENT_MODULE_UNLOADED, EngineKind, Error, EventCapabilities,
    ExternalConfig, ModuleConfig, ModuleError, ModuleInfo, ParamInfo, Preopen, Result,
    WasiCapabilities,
};

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
    host_commands: HashMap<String, HostCommandFn<TauriPlatform<R>>>,
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
        let f = wasm_backend_core::host_command_fn(
            name.clone(),
            move |call: HostCall<'_, TauriPlatform<R>>, args: A| {
                f(
                    HostCallContext {
                        app: call.platform.app().clone(),
                        module: call.module.to_string(),
                    },
                    args,
                )
            },
        );
        self.host_commands.insert(name, f);
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

                let typescript_dir = typescript_out_dir
                    .or(config.typescript_out_dir.clone())
                    .filter(|_| cfg!(debug_assertions))
                    .map(|d| std::env::current_dir().map(|c| c.join(d)))
                    .transpose()?;
                let mut core = Backend::builder(TauriPlatform::new(app.clone()))
                    .hot_reload(hot_reload)
                    .external(config.external.clone())
                    .typescript_out_dir(typescript_dir)
                    .typescript_options(TypeScriptOptions {
                        generator: "tauri-plugin-wasm-backend".into(),
                        api_module: "tauri-plugin-wasm-backend-api".into(),
                    });
                for (name, f) in host_commands {
                    core = core.host_command_fn(name, f);
                }
                let backend = WasmBackend(core.build()?);
                // Managed before modules load so host commands and event
                // handlers they trigger can reach it.
                app.manage(backend.clone());
                backend.core().load_all(config.modules);
                Ok(())
            })
            .build()
    }
}

/// Initializes the plugin with the configuration from `tauri.conf.json`.
pub fn init<R: Runtime>() -> TauriPlugin<R, Option<Config>> {
    Builder::new().build()
}

#[cfg(test)]
mod tests {
    /// The WIT package shipped to guests (repository root, npm package) must
    /// match the one the engine is compiled against.
    #[test]
    fn wit_in_sync() {
        assert_eq!(
            include_str!("../wit/backend.wit"),
            wasm_backend_core::WIT,
            "wit/backend.wit and crates/wasm-backend-core/wit/backend.wit differ"
        );
    }
}
