//! Test fixture: an Extism plugin exercising the host API.

use std::sync::Mutex;

use extism_pdk::*;
use serde::Deserialize;
use serde_json::{Value, json};

#[host_fn]
extern "ExtismHost" {
    fn tauri_log(input: String);
    fn tauri_emit(input: String) -> String;
    fn tauri_invoke(input: String) -> String;
}

static COUNT: Mutex<u32> = Mutex::new(0);
static LAST_EVENT: Mutex<Option<Value>> = Mutex::new(None);

#[derive(Deserialize)]
struct GreetArgs {
    name: String,
}

/// Turn a host function's `{ ok } | { err }` reply into a result.
fn reply(raw: String) -> FnResult<Value> {
    let v: Value = serde_json::from_str(&raw)?;
    match v.get("err") {
        Some(e) => Err(Error::msg(e.as_str().unwrap_or_default().to_string()).into()),
        None => Ok(v.get("ok").cloned().unwrap_or(Value::Null)),
    }
}

#[plugin_fn]
pub fn greet(Json(args): Json<GreetArgs>) -> FnResult<String> {
    unsafe { tauri_log(json!({ "level": "info", "message": format!("greeting {}", args.name) }).to_string())? };
    Ok(format!("Hello, {}!", args.name))
}

#[plugin_fn]
pub fn count() -> FnResult<Json<u32>> {
    let mut c = COUNT.lock().unwrap();
    *c += 1;
    Ok(Json(*c))
}

#[plugin_fn]
pub fn fail() -> FnResult<String> {
    Err(Error::msg("this command always fails").into())
}

#[plugin_fn]
pub fn crash() -> FnResult<String> {
    panic!("crash requested")
}

#[plugin_fn]
pub fn emit_event(Json(args): Json<Value>) -> FnResult<Json<Value>> {
    let raw = unsafe { tauri_emit(args.to_string())? };
    Ok(Json(reply(raw)?))
}

#[plugin_fn]
pub fn call_host(Json(args): Json<Value>) -> FnResult<Json<Value>> {
    let raw = unsafe { tauri_invoke(args.to_string())? };
    Ok(Json(reply(raw)?))
}

#[plugin_fn]
pub fn on_event(Json(event): Json<Value>) -> FnResult<()> {
    *LAST_EVENT.lock().unwrap() = Some(event);
    Ok(())
}

#[plugin_fn]
pub fn last_event() -> FnResult<Json<Option<Value>>> {
    Ok(Json(LAST_EVENT.lock().unwrap().clone()))
}
