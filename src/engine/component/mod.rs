//! WebAssembly component model (WIT) engine, backed by wasmtime.
//!
//! Commands are the functions a component exports, either at the top level
//! of its world (`export greet: func(...)`) or inside an exported interface
//! (invoked as `interface.function`). Arguments are matched to parameters by
//! name, so `invoke('greet', { name })` calls `greet: func(name: string)`,
//! exactly like Tauri maps invoke arguments to Rust command parameters.

mod convert;

use std::{
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use serde_json::Value;
use wasmtime::{
    Config, Engine, Store, StoreLimits, StoreLimitsBuilder,
    component::{
        Component, ComponentExportIndex, HasSelf, InstancePre, Linker, ResourceTable, Type,
        types::{ComponentFunc, ComponentItem},
    },
};
use wasmtime_wasi::{FsPerms, WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

use super::{CommandInfo, ModuleHost, ModuleInstance, ParamInfo, naming};
use crate::{Error, Result};

mod bindings {
    wasmtime::component::bindgen!({
        path: "wit/backend.wit",
        world: "module",
    });
}

use bindings::tauri::backend::host::{self as host_iface, LogLevel};

const EVENTS_INTERFACE: &str = "tauri:backend/events";
/// Granularity of call timeouts.
const EPOCH_TICK: Duration = Duration::from_millis(5);

pub(crate) struct State {
    wasi: WasiCtx,
    table: ResourceTable,
    limits: StoreLimits,
    host: Arc<ModuleHost>,
}

impl WasiView for State {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

impl host_iface::Host for State {
    fn log(&mut self, level: LogLevel, message: String) {
        let level = match level {
            LogLevel::Trace => log::Level::Trace,
            LogLevel::Debug => log::Level::Debug,
            LogLevel::Info => log::Level::Info,
            LogLevel::Warn => log::Level::Warn,
            LogLevel::Error => log::Level::Error,
        };
        self.host.log(level, &message);
    }

    fn emit(&mut self, event: String, payload: String) -> std::result::Result<(), String> {
        self.host.emit(&event, &payload)
    }

    fn invoke(&mut self, command: String, args: String) -> std::result::Result<String, String> {
        self.host.invoke(&command, &args)
    }
}

/// In-memory store for wasmtime's incremental compilation cache.
#[derive(Debug, Default)]
struct MemoryCacheStore(std::sync::Mutex<std::collections::HashMap<Vec<u8>, Vec<u8>>>);

impl wasmtime::CacheStore for MemoryCacheStore {
    fn get(&self, key: &[u8]) -> Option<std::borrow::Cow<'_, [u8]>> {
        let map = self.0.lock().ok()?;
        map.get(key).map(|v| std::borrow::Cow::Owned(v.clone()))
    }

    fn insert(&self, key: &[u8], value: Vec<u8>) -> bool {
        match self.0.lock() {
            Ok(mut map) => {
                map.insert(key.to_vec(), value);
                true
            }
            Err(_) => false,
        }
    }
}

struct Runtime {
    engine: Engine,
    linker: Linker<State>,
    ticker: AtomicBool,
}

fn runtime() -> Result<&'static Runtime> {
    static RUNTIME: OnceLock<std::result::Result<Runtime, String>> = OnceLock::new();
    RUNTIME
        .get_or_init(|| {
            let mut config = Config::new();
            config.wasm_component_model(true).epoch_interruption(true);
            // Cache compiled code on disk: large components (e.g. JS engines)
            // take a while to compile, but rarely change between runs.
            match wasmtime::Cache::from_file(None) {
                Ok(cache) => {
                    config.cache(Some(cache));
                }
                Err(e) => log::debug!("wasmtime compilation cache disabled: {e}"),
            }
            // In development, also cache per function: after an edit, only the
            // functions that changed are recompiled. JS engines compiled to
            // WASM are large, but editing the script rarely changes their code.
            if cfg!(debug_assertions) {
                config
                    .enable_incremental_compilation(Arc::new(MemoryCacheStore::default()))
                    .map_err(|e| e.to_string())?;
            }
            let engine = Engine::new(&config).map_err(|e| e.to_string())?;
            let mut linker = Linker::new(&engine);
            wasmtime_wasi::p2::add_to_linker_sync(&mut linker).map_err(|e| e.to_string())?;
            host_iface::add_to_linker::<State, HasSelf<State>>(&mut linker, |s| s)
                .map_err(|e| e.to_string())?;
            Ok(Runtime {
                engine,
                linker,
                ticker: AtomicBool::new(false),
            })
        })
        .as_ref()
        .map_err(|e| Error::Runtime(e.clone()))
}

impl Runtime {
    fn ensure_ticker(&'static self) {
        if !self.ticker.swap(true, Ordering::SeqCst) {
            std::thread::Builder::new()
                .name("wasm-backend-epoch".into())
                .spawn(move || {
                    loop {
                        std::thread::sleep(EPOCH_TICK);
                        self.engine.increment_epoch();
                    }
                })
                .expect("failed to spawn epoch thread");
        }
    }
}

struct Command {
    info: CommandInfo,
    index: ComponentExportIndex,
    ty: ComponentFunc,
}

pub(crate) struct ComponentInstance {
    runtime: &'static Runtime,
    pre: InstancePre<State>,
    host: Arc<ModuleHost>,
    store: Store<State>,
    instance: Option<wasmtime::component::Instance>,
    commands: Vec<Command>,
    on_event: Option<ComponentExportIndex>,
}

impl ComponentInstance {
    pub fn new(bytes: &[u8], host: Arc<ModuleHost>) -> Result<Self> {
        let runtime = runtime()?;
        let load_err = |e: wasmtime::Error| Error::Load {
            module: host.module.clone(),
            message: format!("{e:#}"),
        };
        let component = Component::new(&runtime.engine, bytes).map_err(load_err)?;
        let pre = runtime
            .linker
            .instantiate_pre(&component)
            .map_err(load_err)?;
        let (commands, on_event) = exports(&runtime.engine, &component);
        if host.capabilities.timeout_ms.is_some() {
            runtime.ensure_ticker();
        }
        let store = new_store(runtime, &host)?;
        let mut this = Self {
            runtime,
            pre,
            host,
            store,
            instance: None,
            commands,
            on_event,
        };
        this.instantiate().map_err(|e| Error::Load {
            module: this.host.module.clone(),
            message: e.to_string(),
        })?;
        Ok(this)
    }

    /// (Re)create the store and instance. Called on load and after a trap,
    /// since a trapped instance can't be entered again.
    fn instantiate(&mut self) -> Result<()> {
        if self.instance.is_some() {
            self.store = new_store(self.runtime, &self.host)?;
            self.instance = None;
        }
        self.set_deadline();
        let instance = self
            .pre
            .instantiate(&mut self.store)
            .map_err(|e| Error::Runtime(format!("{e:#}")))?;
        self.instance = Some(instance);
        Ok(())
    }

    fn set_deadline(&mut self) {
        let ticks = match self.host.capabilities.timeout_ms {
            Some(ms) => (ms / EPOCH_TICK.as_millis() as u64).max(1) + 1,
            None => u64::MAX / 2,
        };
        self.store.set_epoch_deadline(ticks);
    }

    fn call_index(
        &mut self,
        index: &ComponentExportIndex,
        params: &[wasmtime::component::Val],
        results: &mut [wasmtime::component::Val],
    ) -> Result<()> {
        if self.instance.is_none() {
            self.instantiate()?;
        }
        let instance = self.instance.expect("instantiated above");
        let func = instance
            .get_func(&mut self.store, index)
            .ok_or_else(|| Error::Runtime("export is not a function".into()))?;
        self.set_deadline();
        if let Err(e) = func.call(&mut self.store, params, results) {
            // The instance may be left in an inconsistent state; start over
            // on the next call.
            self.instance = None;
            self.store = new_store(self.runtime, &self.host)?;
            return Err(Error::Runtime(trap_message(e)));
        }
        Ok(())
    }
}

fn trap_message(e: wasmtime::Error) -> String {
    if let Some(trap) = e.downcast_ref::<wasmtime::Trap>()
        && *trap == wasmtime::Trap::Interrupt
    {
        return "call timed out".into();
    }
    format!("{e:#}")
}

fn new_store(runtime: &Runtime, host: &Arc<ModuleHost>) -> Result<Store<State>> {
    let caps = &host.capabilities;
    let mut wasi = WasiCtxBuilder::new();
    if caps.wasi.stdio {
        wasi.inherit_stdout().inherit_stderr();
    }
    for (k, v) in &caps.wasi.env {
        wasi.env(k, v);
    }
    for p in &caps.wasi.preopens {
        let perms = if p.read_only {
            FsPerms::ReadOnly
        } else {
            FsPerms::ReadWrite
        };
        wasi.preopened_dir(&p.host, &p.guest, perms)
            .map_err(|e| Error::Load {
                module: host.module.clone(),
                message: format!("failed to preopen {}: {e}", p.host.display()),
            })?;
    }
    if caps.wasi.network {
        wasi.inherit_network()
            .allow_tcp(true)
            .allow_udp(true)
            .allow_ip_name_lookup(true);
    }
    let mut limits = StoreLimitsBuilder::new();
    if let Some(mb) = caps.memory_limit_mb {
        limits = limits.memory_size(mb as usize * 1024 * 1024);
    }
    let mut store = Store::new(
        &runtime.engine,
        State {
            wasi: wasi.build(),
            table: ResourceTable::new(),
            limits: limits.build(),
            host: host.clone(),
        },
    );
    store.limiter(|s| &mut s.limits);
    Ok(store)
}

/// Collect the commands (exported functions) and event handler of a component.
fn exports(engine: &Engine, component: &Component) -> (Vec<Command>, Option<ComponentExportIndex>) {
    let mut commands = Vec::new();
    let mut on_event = None;
    let ty = component.component_type();
    for (name, export) in ty.exports(engine) {
        match export.ty {
            ComponentItem::ComponentFunc(func) => {
                if let Some(index) = component.get_export_index(None, name) {
                    commands.push(command(name.to_string(), index, func));
                }
            }
            ComponentItem::ComponentInstance(inst) => {
                let Some(iface_index) = component.get_export_index(None, name) else {
                    continue;
                };
                let base = name.split('@').next().unwrap_or(name);
                if base == EVENTS_INTERFACE {
                    on_event = component.get_export_index(Some(&iface_index), "on-event");
                    continue;
                }
                let short = base.rsplit('/').next().unwrap_or(base);
                for (fname, item) in inst.exports(engine) {
                    if let ComponentItem::ComponentFunc(func) = item.ty
                        && let Some(index) = component.get_export_index(Some(&iface_index), fname)
                    {
                        commands.push(command(format!("{short}.{fname}"), index, func));
                    }
                }
            }
            _ => {}
        }
    }
    (commands, on_event)
}

fn command(name: String, index: ComponentExportIndex, ty: ComponentFunc) -> Command {
    let params = ty
        .params()
        .map(|(name, t)| ParamInfo {
            name: naming::to_camel(name),
            ty: convert::wit_type(&t),
        })
        .collect();
    let result = ty.results().next().map(|t| convert::wit_type(&t));
    Command {
        info: CommandInfo {
            name,
            params,
            result,
        },
        index,
        ty,
    }
}

impl ModuleInstance for ComponentInstance {
    fn call(&mut self, command: &str, args: Value) -> Result<Value> {
        let not_found = || Error::CommandNotFound {
            module: self.host.module.clone(),
            command: command.to_string(),
        };
        let wanted = naming::to_kebab(command);
        let cmd = self
            .commands
            .iter()
            .find(|c| c.info.name == command)
            .or_else(|| {
                self.commands
                    .iter()
                    .find(|c| naming::to_kebab(&c.info.name) == wanted)
            })
            .ok_or_else(not_found)?;

        let invalid = |message: String| Error::InvalidArgs {
            command: command.to_string(),
            message,
        };
        let empty = serde_json::Map::new();
        let obj = match &args {
            Value::Object(obj) => obj,
            Value::Null => &empty,
            other => {
                return Err(invalid(format!(
                    "arguments must be an object keyed by parameter name, got {other}"
                )));
            }
        };
        let mut params = Vec::new();
        for (name, ty) in cmd.ty.params() {
            let value = convert::lookup(obj, name).unwrap_or(&Value::Null);
            if value.is_null() && !matches!(ty, Type::Option(_)) {
                return Err(invalid(format!(
                    "missing required key `{}`",
                    naming::to_camel(name)
                )));
            }
            params.push(
                convert::json_to_val(value, &ty)
                    .map_err(|e| invalid(format!("`{}`: {e}", naming::to_camel(name))))?,
            );
        }
        let result_ty = cmd.ty.results().next();
        let mut results = vec![wasmtime::component::Val::Bool(false); cmd.ty.results().len()];
        let index = cmd.index;
        self.call_index(&index, &params, &mut results)?;

        let Some(result) = results.pop() else {
            return Ok(Value::Null);
        };
        // A top-level `result<T, E>` maps to Tauri's `Result<T, E>`
        // semantics: `ok` resolves the promise, `err` rejects it.
        match (result_ty, result) {
            (Some(Type::Result(_)), wasmtime::component::Val::Result(r)) => match r {
                Ok(v) => Ok(v.map(|v| convert::val_to_json(*v)).unwrap_or(Value::Null)),
                Err(e) => Err(Error::Command(
                    e.map(|v| convert::val_to_json(*v)).unwrap_or(Value::Null),
                )),
            },
            (_, v) => Ok(convert::val_to_json(v)),
        }
    }

    fn on_event(&mut self, event: &str, payload: &Value) -> Result<()> {
        let Some(index) = self.on_event else {
            return Ok(());
        };
        let params = [
            wasmtime::component::Val::String(event.to_string()),
            wasmtime::component::Val::String(payload.to_string()),
        ];
        self.call_index(&index, &params, &mut [])
    }

    fn commands(&self) -> Vec<CommandInfo> {
        self.commands.iter().map(|c| c.info.clone()).collect()
    }

    fn typescript(&self, module: &str) -> Option<String> {
        let mut out = String::new();
        for cmd in &self.commands {
            let params: Vec<(String, String)> = cmd
                .ty
                .params()
                .map(|(n, t)| (naming::to_camel(n), convert::ts_type(&t)))
                .collect();
            let ret = match cmd.ty.results().next() {
                None => "void".to_string(),
                Some(Type::Result(r)) => r
                    .ok()
                    .map(|t| convert::ts_type(&t))
                    .unwrap_or_else(|| "void".into()),
                Some(t) => convert::ts_type(&t),
            };
            let sig = params
                .iter()
                .map(|(n, t)| format!("{n}: {t}"))
                .collect::<Vec<_>>()
                .join(", ");
            let args = params
                .iter()
                .map(|(n, _)| n.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            if let Some(r) = &cmd.info.result {
                out.push_str(&format!("  /** WIT: `{}` -> `{r}` */\n", cmd.info.name));
            }
            out.push_str(&format!(
                "  {fname}: ({sig}): Promise<{ret}> =>\n    invokeModule<{ret}>({module:?}, {name:?}, {{ {args} }}),\n",
                fname = naming::to_camel(&cmd.info.name),
                name = cmd.info.name,
            ));
        }
        Some(out)
    }
}
