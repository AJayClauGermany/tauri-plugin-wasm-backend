//! Framework-agnostic core of
//! [`tauri-plugin-wasm-backend`](https://crates.io/crates/tauri-plugin-wasm-backend):
//! run hot-reloadable, capability-restricted WebAssembly backend modules in
//! any Rust app.
//!
//! This crate holds the WASM engines, the capability model, the module
//! registry and hot reload. Everything that depends on the host framework
//! (delivering events, standard directories, threads) goes through the
//! [`Platform`] trait, so the same modules can run inside Tauri, GPUI, a CLI
//! or a server. Two engines are supported:
//!
//! - **WIT components** (`component` feature), run by wasmtime. Exported
//!   functions are commands; arguments are mapped by parameter name.
//! - **Extism plugins** (`extism` feature), for toolchains that don't target
//!   the component model yet. Exported functions are commands taking and
//!   returning JSON.
//!
//! The guest-facing API (the `tauri:backend` WIT package and the
//! `tauri_*` Extism host functions) is the same on every platform, so a
//! module built for the Tauri plugin runs unchanged elsewhere.
//!
//! ```no_run
//! use std::path::PathBuf;
//!
//! use serde_json::{Value, json};
//! use wasm_backend_core::{
//!     Backend, BaseDir, Capabilities, EventBus, EventHandler, ModuleConfig, Platform, Result,
//! };
//!
//! /// A platform for an app without an event system of its own.
//! struct MyPlatform {
//!     events: EventBus,
//! }
//!
//! impl Platform for MyPlatform {
//!     type ListenerId = u64;
//!
//!     fn emit(&self, event: &str, payload: Value) -> std::result::Result<(), String> {
//!         // Forward to your UI here (e.g. a channel read by a GPUI entity).
//!         self.events.emit(event, &payload);
//!         Ok(())
//!     }
//!
//!     fn listen(&self, event: &str, handler: EventHandler) -> u64 {
//!         self.events.listen(event, handler)
//!     }
//!
//!     fn unlisten(&self, id: u64) {
//!         self.events.unlisten(id)
//!     }
//!
//!     fn base_dir(&self, base: BaseDir) -> Result<PathBuf> {
//!         Ok(match base {
//!             BaseDir::Temp => std::env::temp_dir(),
//!             _ => PathBuf::from("./data"),
//!         })
//!     }
//! }
//!
//! let backend = Backend::builder(MyPlatform { events: EventBus::new() })
//!     .host_command("now", |_call, _args: ()| {
//!         Ok::<_, String>(
//!             std::time::SystemTime::now()
//!                 .duration_since(std::time::UNIX_EPOCH)
//!                 .unwrap()
//!                 .as_millis() as u64,
//!         )
//!     })
//!     .build()?;
//! backend.load_all([(
//!     "greeter".to_string(),
//!     ModuleConfig::from_path("wasm/greeter.wasm")
//!         .capabilities(Capabilities::default().allow_host_command("now")),
//! )]);
//! let greeting = backend.invoke("greeter", "greet", json!({ "name": "GPUI" }))?;
//! # Ok::<(), wasm_backend_core::Error>(())
//! ```

// Without any engine, modules can't be instantiated and the host plumbing is unused.
#![cfg_attr(not(any(feature = "component", feature = "extism")), allow(dead_code))]

mod capabilities;
mod config;
mod engine;
mod error;
pub mod naming;
mod platform;
mod registry;
mod task;
#[cfg(feature = "hot-reload")]
mod watcher;

pub use capabilities::{
    Capabilities, EventCapabilities, Preopen, WasiCapabilities, wildcard_match,
};
pub use config::{Config, EngineKind, ExternalConfig, ModuleConfig};
pub use engine::{CommandInfo, ParamInfo};
pub use error::{Error, Result};
pub use platform::{BaseDir, EventBus, EventHandler, Platform, resolve_dir};
pub use registry::{
    Backend, BackendBuilder, EVENT_MODULE_ERROR, EVENT_MODULE_LOADED, EVENT_MODULE_RELOADED,
    EVENT_MODULE_UNLOADED, HostCall, HostCommandFn, ModuleError, ModuleInfo, TypeScriptOptions,
    host_command_fn,
};

/// The WIT package modules import (`tauri:backend`), for tools that need to
/// hand it to guest builds.
pub const WIT: &str = include_str!("../wit/backend.wit");
