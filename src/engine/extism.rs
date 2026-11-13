//! Extism engine, for languages whose toolchains don't target the component
//! model yet.
//!
//! Commands are the plugin's exported functions. The invoke arguments are
//! passed to the function as its JSON input, and its output is parsed as
//! JSON (falling back to a plain string). Errors set by the plugin reject
//! the invoke promise.
//!
//! Host functions live in the default `extism:host/user` namespace and take
//! and return JSON strings:
//!
//! | function       | input                          | output                               |
//! |----------------|--------------------------------|--------------------------------------|
//! | `tauri_log`    | `{ "level", "message" }`       | -                                    |
//! | `tauri_emit`   | `{ "event", "payload" }`       | `{ "ok": null }` or `{ "err": msg }` |
//! | `tauri_invoke` | `{ "command", "args" }`        | `{ "ok": value }` or `{ "err": msg }`|
//!
//! Events subscribed to via `capabilities.events.listen` are delivered to an
//! exported `on_event` (or `onEvent`) function with `{ "event", "payload" }`
//! as input.

use std::sync::Arc;

use extism::{CurrentPlugin, Function, Manifest, PTR, PluginBuilder, UserData, Val, Wasm};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{CommandInfo, ModuleHost, ModuleInstance, naming};
use crate::{Error, Result};

const EVENT_HANDLERS: &[&str] = &["on_event", "onEvent"];

pub(crate) struct ExtismInstance {
    plugin: extism::Plugin,
    host: Arc<ModuleHost>,
    exports: Vec<String>,
}

impl ExtismInstance {
    pub fn new(bytes: &[u8], host: Arc<ModuleHost>) -> Result<Self> {
        let caps = &host.capabilities;
        let mut manifest = Manifest::new([Wasm::data(bytes.to_vec())])
            .with_allowed_hosts(caps.allowed_hosts.iter().cloned())
            .with_allowed_paths(caps.wasi.preopens.iter().map(|p| {
                // Extism marks read-only mappings with an `ro:` prefix.
                let host_path = p.host.to_string_lossy().into_owned();
                let src = if p.read_only {
                    format!("ro:{host_path}")
                } else {
                    host_path
                };
                (src, p.guest.clone().into())
            }));
        for (k, v) in &caps.wasi.env {
            manifest = manifest.with_config_key(k, v);
        }
        if let Some(mb) = caps.memory_limit_mb {
            // 64 KiB pages.
            manifest = manifest.with_memory_max(mb.saturating_mul(16));
        }
        if let Some(ms) = caps.timeout_ms {
            manifest = manifest.with_timeout(std::time::Duration::from_millis(ms));
        }

        let plugin = PluginBuilder::new(manifest)
            .with_wasi(true)
            .with_functions(host_functions(&host))
            .build()
            .map_err(|e| Error::Load {
                module: host.module.clone(),
                message: format!("{e:#}"),
            })?;

        let exports = core_function_exports(bytes)
            .into_iter()
            .filter(|name| !is_internal_export(name))
            .collect();

        Ok(Self {
            plugin,
            host,
            exports,
        })
    }

    fn resolve(&self, command: &str) -> Option<String> {
        naming::candidates(command)
            .into_iter()
            .find(|c| self.plugin.function_exists(c))
    }

    fn call_raw(&mut self, function: &str, input: &str) -> Result<Vec<u8>> {
        self.plugin
            .call::<&str, Vec<u8>>(function, input)
            .map_err(|e| {
                let msg = e.to_string();
                let trapped = e
                    .chain()
                    .any(|c| c.downcast_ref::<wasmtime::Trap>().is_some());
                if trapped || msg == "timeout" || msg == "oom" {
                    let msg = if msg == "timeout" {
                        "call timed out".to_string()
                    } else {
                        format!("{e:#}")
                    };
                    return Error::Runtime(msg);
                }
                // Plugin-set errors are usually strings; pass JSON through as is.
                Error::Command(serde_json::from_str(&msg).unwrap_or(Value::String(msg)))
            })
    }
}

fn is_internal_export(name: &str) -> bool {
    name.starts_with('_')
        || EVENT_HANDLERS.contains(&name)
        || matches!(name, "memory" | "cabi_realloc")
}

impl ModuleInstance for ExtismInstance {
    fn call(&mut self, command: &str, args: Value) -> Result<Value> {
        let function = self
            .resolve(command)
            .filter(|f| !is_internal_export(f))
            .ok_or_else(|| Error::CommandNotFound {
                module: self.host.module.clone(),
                command: command.to_string(),
            })?;
        let input = if args.is_null() {
            String::new()
        } else {
            args.to_string()
        };
        let output = self.call_raw(&function, &input)?;
        if output.is_empty() {
            return Ok(Value::Null);
        }
        Ok(serde_json::from_slice(&output)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&output).into_owned())))
    }

    fn on_event(&mut self, event: &str, payload: &Value) -> Result<()> {
        let Some(handler) = EVENT_HANDLERS
            .iter()
            .find(|f| self.plugin.function_exists(f))
        else {
            return Ok(());
        };
        let input = json!({ "event": event, "payload": payload }).to_string();
        self.call_raw(handler, &input).map(|_| ())
    }

    fn commands(&self) -> Vec<CommandInfo> {
        self.exports
            .iter()
            .map(|name| CommandInfo {
                name: name.clone(),
                params: Vec::new(),
                result: None,
            })
            .collect()
    }

    fn typescript(&self, module: &str) -> Option<String> {
        let mut out = String::new();
        for name in &self.exports {
            out.push_str(&format!(
                "  {fname}: <T = unknown>(input?: unknown): Promise<T> =>\n    invokeModule<T>({module}, {name}, input),\n",
                fname = naming::to_camel(name),
                module = super::js_string(module),
                name = super::js_string(name),
            ));
        }
        Some(out)
    }
}

type Shared = UserData<Arc<ModuleHost>>;

fn host_functions(host: &Arc<ModuleHost>) -> Vec<Function> {
    let data: Shared = UserData::new(host.clone());
    vec![
        Function::new("tauri_log", [PTR], [], data.clone(), tauri_log),
        Function::new("tauri_emit", [PTR], [PTR], data.clone(), tauri_emit),
        Function::new("tauri_invoke", [PTR], [PTR], data, tauri_invoke),
    ]
}

fn input<T: for<'de> Deserialize<'de>>(
    plugin: &mut CurrentPlugin,
    inputs: &[Val],
) -> std::result::Result<T, extism::Error> {
    let raw: String = plugin.memory_get_val(&inputs[0])?;
    Ok(serde_json::from_str(&raw)?)
}

fn output(
    plugin: &mut CurrentPlugin,
    outputs: &mut [Val],
    result: std::result::Result<Value, String>,
) -> std::result::Result<(), extism::Error> {
    let body = match result {
        Ok(v) => json!({ "ok": v }),
        Err(e) => json!({ "err": e }),
    };
    let handle = plugin.memory_new(body.to_string())?;
    outputs[0] = plugin.memory_to_val(handle);
    Ok(())
}

fn with_host<T>(
    data: &Shared,
    f: impl FnOnce(&ModuleHost) -> T,
) -> std::result::Result<T, extism::Error> {
    let host = data.get()?;
    let host = host
        .lock()
        .map_err(|_| extism::Error::msg("host state poisoned"))?;
    Ok(f(&host))
}

fn tauri_log(
    plugin: &mut CurrentPlugin,
    inputs: &[Val],
    _outputs: &mut [Val],
    data: Shared,
) -> std::result::Result<(), extism::Error> {
    #[derive(Deserialize)]
    struct Args {
        #[serde(default)]
        level: Option<String>,
        message: String,
    }
    let args: Args = input(plugin, inputs)?;
    let level = match args.level.as_deref().unwrap_or("info") {
        "trace" => log::Level::Trace,
        "debug" => log::Level::Debug,
        "warn" => log::Level::Warn,
        "error" => log::Level::Error,
        _ => log::Level::Info,
    };
    with_host(&data, |h| h.log(level, &args.message))
}

fn tauri_emit(
    plugin: &mut CurrentPlugin,
    inputs: &[Val],
    outputs: &mut [Val],
    data: Shared,
) -> std::result::Result<(), extism::Error> {
    #[derive(Deserialize)]
    struct Args {
        event: String,
        #[serde(default)]
        payload: Value,
    }
    let args: Args = input(plugin, inputs)?;
    let result = with_host(&data, |h| h.emit(&args.event, &args.payload.to_string()))?;
    output(plugin, outputs, result.map(|_| Value::Null))
}

fn tauri_invoke(
    plugin: &mut CurrentPlugin,
    inputs: &[Val],
    outputs: &mut [Val],
    data: Shared,
) -> std::result::Result<(), extism::Error> {
    #[derive(Deserialize)]
    struct Args {
        command: String,
        #[serde(default)]
        args: Value,
    }
    let args: Args = input(plugin, inputs)?;
    let result = with_host(&data, |h| h.invoke(&args.command, &args.args.to_string()))?
        .and_then(|s| serde_json::from_str(&s).map_err(|e| e.to_string()));
    output(plugin, outputs, result)
}

/// Names of the functions exported by a core WebAssembly module.
fn core_function_exports(bytes: &[u8]) -> Vec<String> {
    fn leb(bytes: &[u8], pos: &mut usize) -> Option<u64> {
        let mut result = 0u64;
        let mut shift = 0;
        loop {
            let b = *bytes.get(*pos)?;
            *pos += 1;
            result |= ((b & 0x7f) as u64) << shift;
            if b & 0x80 == 0 {
                return Some(result);
            }
            shift += 7;
            if shift > 63 {
                return None;
            }
        }
    }
    fn parse(bytes: &[u8]) -> Option<Vec<String>> {
        let mut pos = 8;
        while pos < bytes.len() {
            let id = bytes[pos];
            pos += 1;
            let size = leb(bytes, &mut pos)? as usize;
            let end = pos.checked_add(size)?;
            if id == 7 {
                let mut names = Vec::new();
                let count = leb(bytes, &mut pos)?;
                for _ in 0..count {
                    let len = leb(bytes, &mut pos)? as usize;
                    let name = std::str::from_utf8(bytes.get(pos..pos + len)?).ok()?;
                    pos += len;
                    let kind = *bytes.get(pos)?;
                    pos += 1;
                    leb(bytes, &mut pos)?;
                    if kind == 0 {
                        names.push(name.to_string());
                    }
                }
                return Some(names);
            }
            pos = end;
        }
        Some(Vec::new())
    }
    parse(bytes).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_exports() {
        // (module (func (export "greet")) (memory (export "memory") 1))
        let wasm: &[u8] = &[
            0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, // header
            0x01, 0x04, 0x01, 0x60, 0x00, 0x00, // type section
            0x03, 0x02, 0x01, 0x00, // function section
            0x05, 0x03, 0x01, 0x00, 0x01, // memory section
            0x07, 0x12, 0x02, // export section, 2 exports
            0x05, b'g', b'r', b'e', b'e', b't', 0x00, 0x00, // func 0
            0x06, b'm', b'e', b'm', b'o', b'r', b'y', 0x02, 0x00, // memory 0
            0x0a, 0x04, 0x01, 0x02, 0x00, 0x0b, // code section
        ];
        assert_eq!(core_function_exports(wasm), vec!["greet"]);
    }
}
