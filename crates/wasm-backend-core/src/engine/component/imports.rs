//! Typed host commands: a component can import the app's host commands as
//! ordinary WIT functions instead of calling the stringly typed
//! `tauri:backend/host.invoke`:
//!
//! ```wit
//! interface app {
//!   record version { major: u32, minor: u32 }
//!   app-version: func() -> version;
//! }
//! world greeter {
//!   include tauri:backend/module@0.1.0;
//!   import app;
//! }
//! ```
//!
//! Every function imported from an interface outside `wasi:*` and
//! `tauri:backend/*` is backed by the host command of the same name
//! (`interface.function` first, then `function`, across naming conventions).
//! The imports are checked when the module loads: each one needs a
//! registered host command its capabilities allow, so a renamed or missing
//! host command fails the load (and a hot reload keeps the previous
//! version) instead of failing the first call.
//!
//! Arguments reach the host command as an object keyed by camelCase
//! parameter name, like `invoke` arguments reach a Tauri command, and its
//! JSON result is converted to the declared WIT result type. A result that
//! doesn't fit is an error naming the field that didn't.

use serde_json::{Map, Value};
use wasmtime::{
    Engine, StoreContextMut,
    component::{
        Component, Linker, Type, Val,
        types::{ComponentFunc, ComponentItem},
    },
};

use super::{State, convert};
use crate::engine::{CommandInfo, ModuleHost, ParamInfo};
use crate::naming;

/// Interfaces the runtime provides itself.
fn is_builtin(import: &str) -> bool {
    import.starts_with("wasi:") || import.starts_with("tauri:backend/")
}

/// `example:greeter/app@1.0.0` -> `app`.
fn short_name(interface: &str) -> &str {
    let base = interface.split('@').next().unwrap_or(interface);
    base.rsplit('/').next().unwrap_or(base)
}

pub(super) struct HostImport {
    /// Instance the function is imported from (`None` for a world-level
    /// function import).
    instance: Option<String>,
    func: String,
    /// `instance#func`, for messages.
    path: String,
    /// The registered host command backing it.
    command: String,
    pub(super) info: CommandInfo,
}

/// Find the imports backed by host commands and check each one against the
/// registered host commands and the module's capabilities. Returns every
/// problem at once, so they can be fixed in one go.
pub(super) fn resolve(
    engine: &Engine,
    component: &Component,
    host: &ModuleHost,
) -> Result<Vec<HostImport>, Vec<String>> {
    let mut imports = Vec::new();
    let mut problems = Vec::new();
    let mut funcs: Vec<(Option<&str>, String, ComponentFunc)> = Vec::new();

    let ty = component.component_type();
    for (name, import) in ty.imports(engine) {
        if is_builtin(name) {
            continue;
        }
        match import.ty {
            ComponentItem::ComponentFunc(func) => funcs.push((None, name.to_string(), func)),
            ComponentItem::ComponentInstance(instance) => {
                for (item, export) in instance.exports(engine) {
                    match export.ty {
                        // Resource methods (`[method]r.f`) are reported with the resource.
                        ComponentItem::ComponentFunc(_) if item.starts_with('[') => {}
                        ComponentItem::ComponentFunc(func) => {
                            funcs.push((Some(name), item.to_string(), func))
                        }
                        ComponentItem::Resource(_) => problems.push(format!(
                            "`{name}` declares resource `{item}`: host command interfaces can only use plain data types (records, variants, lists, ...)"
                        )),
                        _ => {}
                    }
                }
            }
            // Anything else is left to the linker, which reports it.
            _ => {}
        }
    }

    for (instance, func, ty) in funcs {
        let func = func.as_str();
        let path = match instance {
            Some(i) => format!("{i}#{func}"),
            None => func.to_string(),
        };
        if ty.async_() {
            problems.push(format!(
                "`{path}`: async functions can't be backed by host commands"
            ));
            continue;
        }
        // Like exported commands: `interface.function`, then `function`.
        let candidates: Vec<String> = instance
            .map(|i| format!("{}.{func}", short_name(i)))
            .into_iter()
            .chain([func.to_string()])
            .collect();
        let Some(registered) = candidates.iter().find_map(|c| host.host.resolve_command(c)) else {
            let names = host.host.command_names();
            problems.push(format!(
                "`{path}` needs a host command named `{}`, but none is registered (register it with `host_command`; registered: {})",
                candidates.join("` or `"),
                if names.is_empty() {
                    "none".into()
                } else {
                    names.join(", ")
                }
            ));
            continue;
        };
        let command = match host.resolve_command(&registered) {
            Ok(command) => command,
            Err(e) => {
                problems.push(format!("`{path}`: {e}"));
                continue;
            }
        };
        let info = CommandInfo {
            name: command.clone(),
            params: ty
                .params()
                .map(|(name, t)| ParamInfo {
                    name: naming::to_camel(name),
                    ty: convert::wit_type(&t),
                })
                .collect(),
            result: ty.results().next().map(|t| convert::wit_type(&t)),
        };
        imports.push(HostImport {
            instance: instance.map(str::to_string),
            func: func.to_string(),
            path,
            command,
            info,
        });
    }
    if problems.is_empty() {
        Ok(imports)
    } else {
        Err(problems)
    }
}

/// Define the imports in `linker`, dispatching to the host commands.
pub(super) fn link(linker: &mut Linker<State>, imports: &[HostImport]) -> wasmtime::Result<()> {
    for import in imports {
        let (command, path) = (import.command.clone(), import.path.clone());
        let func = move |store: StoreContextMut<'_, State>,
                         ty: ComponentFunc,
                         params: &[Val],
                         results: &mut [Val]| {
            call(
                store.data().host.as_ref(),
                &command,
                &path,
                &ty,
                params,
                results,
            )
        };
        match &import.instance {
            Some(instance) => linker.instance(instance)?.func_new(&import.func, func)?,
            None => linker.root().func_new(&import.func, func)?,
        }
    }
    Ok(())
}

fn call(
    host: &ModuleHost,
    command: &str,
    path: &str,
    ty: &ComponentFunc,
    params: &[Val],
    results: &mut [Val],
) -> wasmtime::Result<()> {
    let args: Map<String, Value> = ty
        .params()
        .zip(params)
        .map(|((name, _), val)| (naming::to_camel(name), convert::val_to_json(val.clone())))
        .collect();
    let out = host.invoke_resolved(command, Value::Object(args));

    // Without a `result` to report it in, a host error traps the module,
    // like a `throw` in an export without one.
    let failed = |e: String| {
        wasmtime::Error::msg(format!(
            "host command `{command}` (imported as `{path}`) failed: {e} (declare the import as `-> result<T, string>` to handle errors in the module)"
        ))
    };
    let mismatch = |e: String| {
        wasmtime::Error::msg(format!(
            "host command `{command}` returned a value that doesn't match its import `{path}`: {e}"
        ))
    };
    let Some(result_ty) = ty.results().next() else {
        return out.map(|_| ()).map_err(failed);
    };
    results[0] = match (&result_ty, out) {
        (Type::Result(r), Ok(v)) => Val::Result(Ok(match r.ok() {
            Some(t) => Some(Box::new(convert::json_to_val(&v, &t).map_err(mismatch)?)),
            None => None,
        })),
        (Type::Result(r), Err(e)) => Val::Result(Err(match r.err() {
            Some(t) => Some(Box::new(error_val(e, &t).map_err(mismatch)?)),
            None => None,
        })),
        (t, Ok(v)) => convert::json_to_val(&v, t).map_err(mismatch)?,
        (_, Err(e)) => return Err(failed(e)),
    };
    Ok(())
}

/// Host command errors are strings: pass them as is to `string` errors, and
/// parse them as JSON for structured ones.
fn error_val(message: String, ty: &Type) -> Result<Val, String> {
    if matches!(ty, Type::String) {
        return Ok(Val::String(message));
    }
    let value = serde_json::from_str(&message).unwrap_or(Value::String(message.clone()));
    convert::json_to_val(&value, ty)
        .map_err(|e| format!("its error `{message}` doesn't fit the error type: {e}"))
}
