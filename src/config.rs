//! Plugin configuration, read from `plugins.wasm-backend` in `tauri.conf.json`
//! and/or set programmatically through [`crate::Builder`].

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use serde::{Deserialize, Serialize};

use crate::capabilities::Capabilities;

/// Which runtime executes a module.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub enum EngineKind {
    /// Detect from the binary: WebAssembly components use the WIT engine,
    /// core modules use Extism.
    #[default]
    Auto,
    /// WebAssembly component model (WIT) via wasmtime.
    Component,
    /// Extism plugin.
    Extism,
}

/// Configuration of the whole plugin.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Config {
    /// Modules shipped with (or developed alongside) the app, keyed by name.
    #[serde(default)]
    pub modules: BTreeMap<String, ModuleConfig>,
    /// Third-party plugins loaded from a directory at runtime.
    #[serde(default)]
    pub external: Option<ExternalConfig>,
    /// Watch module files and reload them when they change.
    /// Defaults to `true` in debug builds and `false` in release builds.
    #[serde(default)]
    pub hot_reload: Option<bool>,
    /// In debug builds, write TypeScript bindings for every module to
    /// `<dir>/<module>.ts` on (re)load. Relative to the current directory
    /// (`src-tauri` under `tauri dev`).
    #[serde(default)]
    pub typescript_out_dir: Option<PathBuf>,
}

/// Configuration of a single module.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModuleConfig {
    /// Path of the `.wasm` file. Relative paths are resolved against the
    /// app's resource directory in release builds and against the current
    /// directory (`src-tauri` under `tauri dev`) in debug builds.
    #[serde(default)]
    pub path: Option<PathBuf>,
    /// Path used instead of `path` in debug builds.
    #[serde(default)]
    pub dev_path: Option<PathBuf>,
    #[serde(default)]
    pub engine: EngineKind,
    /// What the module is allowed to do.
    #[serde(default)]
    pub capabilities: Capabilities,
    /// Whether the frontend may invoke this module when the `invoke` command
    /// is allowed without an explicit scope. Defaults to `true` for app
    /// modules and `false` for external plugins.
    #[serde(default)]
    pub expose: Option<bool>,
    /// Override the global hot reload setting for this module.
    #[serde(default)]
    pub watch: Option<bool>,
    /// Module bytes, when the module is embedded in the binary instead of
    /// being loaded from disk (see [`ModuleConfig::from_bytes`]).
    #[serde(skip)]
    pub bytes: Option<Arc<[u8]>>,
}

impl ModuleConfig {
    /// A module loaded from a file.
    pub fn from_path(path: impl Into<PathBuf>) -> Self {
        Self {
            path: Some(path.into()),
            ..Default::default()
        }
    }

    /// A module embedded in the binary, e.g. with `include_bytes!`.
    /// Embedded modules can't be hot reloaded.
    pub fn from_bytes(bytes: impl Into<Arc<[u8]>>) -> Self {
        Self {
            bytes: Some(bytes.into()),
            ..Default::default()
        }
    }

    /// Path used in debug builds.
    pub fn dev_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.dev_path = Some(path.into());
        self
    }

    pub fn engine(mut self, engine: EngineKind) -> Self {
        self.engine = engine;
        self
    }

    pub fn capabilities(mut self, capabilities: Capabilities) -> Self {
        self.capabilities = capabilities;
        self
    }

    pub fn expose(mut self, expose: bool) -> Self {
        self.expose = Some(expose);
        self
    }

    pub fn watch(mut self, watch: bool) -> Self {
        self.watch = Some(watch);
        self
    }

    /// The configured path for the current build profile, unresolved.
    pub(crate) fn configured_path(&self) -> Option<&Path> {
        if cfg!(debug_assertions) {
            self.dev_path.as_deref().or(self.path.as_deref())
        } else {
            self.path.as_deref()
        }
    }
}

/// Third-party plugins: every `*.wasm` file in `dir` is loaded as a module
/// named after the file stem.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExternalConfig {
    /// Directory to load plugins from. Relative paths are resolved against
    /// the app data directory.
    pub dir: PathBuf,
    /// Capabilities granted to every external plugin. Defaults to none.
    #[serde(default)]
    pub capabilities: Capabilities,
    /// Whether the frontend may invoke external plugins without an explicit
    /// `invoke` scope entry naming them. Defaults to `false`.
    #[serde(default)]
    pub expose: bool,
    /// Engine used for external plugins.
    #[serde(default)]
    pub engine: EngineKind,
}
