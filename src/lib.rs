use tauri::{
    Manager, Runtime,
    plugin::{Builder, TauriPlugin},
};

pub use models::*;

#[cfg(desktop)]
mod desktop;
#[cfg(mobile)]
mod mobile;

mod commands;
mod error;
mod models;

pub use error::{Error, Result};

#[cfg(desktop)]
use desktop::WasmBackend;
#[cfg(mobile)]
use mobile::WasmBackend;

/// Extensions to [`tauri::App`], [`tauri::AppHandle`] and [`tauri::Window`] to access the wasm-backend APIs.
pub trait WasmBackendExt<R: Runtime> {
    fn wasm_backend(&self) -> &WasmBackend<R>;
}

impl<R: Runtime, T: Manager<R>> crate::WasmBackendExt<R> for T {
    fn wasm_backend(&self) -> &WasmBackend<R> {
        self.state::<WasmBackend<R>>().inner()
    }
}

/// Initializes the plugin.
pub fn init<R: Runtime>() -> TauriPlugin<R> {
    Builder::new("wasm-backend")
        .invoke_handler(tauri::generate_handler![commands::ping])
        .setup(|app, api| {
            #[cfg(mobile)]
            let wasm_backend = mobile::init(app, api)?;
            #[cfg(desktop)]
            let wasm_backend = desktop::init(app, api)?;
            app.manage(wasm_backend);
            Ok(())
        })
        .build()
}
