//! A backend module using the Extism engine.
//!
//! Every `#[plugin_fn]` is a command; its input is the JSON `args` passed to
//! `invokeModule` and its output is returned to the frontend.

use extism_pdk::*;
use serde::{Deserialize, Serialize};
use serde_json::json;

#[host_fn]
extern "ExtismHost" {
    fn tauri_emit(input: String) -> String;
}

#[derive(Deserialize)]
struct SummarizeArgs {
    numbers: Vec<f64>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Summary {
    count: usize,
    mean: f64,
    min: f64,
    max: f64,
}

#[plugin_fn]
pub fn summarize(Json(args): Json<SummarizeArgs>) -> FnResult<Json<Summary>> {
    if args.numbers.is_empty() {
        return Err(Error::msg("no numbers given").into());
    }
    let count = args.numbers.len();
    let summary = Summary {
        count,
        mean: args.numbers.iter().sum::<f64>() / count as f64,
        min: args.numbers.iter().cloned().fold(f64::INFINITY, f64::min),
        max: args.numbers.iter().cloned().fold(f64::NEG_INFINITY, f64::max),
    };
    // Host functions take and return JSON; allowed by `events.emit`.
    unsafe { tauri_emit(json!({ "event": "stats://computed", "payload": count }).to_string())? };
    Ok(Json(summary))
}
