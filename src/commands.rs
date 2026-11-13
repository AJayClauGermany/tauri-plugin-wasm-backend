use tauri::{AppHandle, Runtime, command};

use crate::Result;
use crate::WasmBackendExt;
use crate::models::*;

#[command]
pub(crate) async fn ping<R: Runtime>(
    app: AppHandle<R>,
    payload: PingRequest,
) -> Result<PingResponse> {
    app.wasm_backend().ping(payload)
}
