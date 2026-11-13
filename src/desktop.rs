use serde::de::DeserializeOwned;
use tauri::{AppHandle, Runtime, plugin::PluginApi};

use crate::models::*;

pub fn init<R: Runtime, C: DeserializeOwned>(
    app: &AppHandle<R>,
    _api: PluginApi<R, C>,
) -> crate::Result<WasmBackend<R>> {
    Ok(WasmBackend(app.clone()))
}

/// Access to the wasm-backend APIs.
pub struct WasmBackend<R: Runtime>(AppHandle<R>);

impl<R: Runtime> WasmBackend<R> {
    pub fn ping(&self, payload: PingRequest) -> crate::Result<PingResponse> {
        Ok(PingResponse {
            value: payload.value,
        })
    }
}
