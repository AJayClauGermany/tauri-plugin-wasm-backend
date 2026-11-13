use serde::Deserialize;
use serde_json::Value;
use tauri::{
    AppHandle, Runtime, command,
    ipc::{CommandScope, GlobalScope},
};

use crate::{
    Error, ModuleInfo, Result, WasmBackendExt, capabilities::wildcard_match, engine::naming,
};

/// Scope entry of the `invoke` command, used in capability files to restrict
/// which modules and commands the frontend may call:
///
/// ```json
/// {
///   "identifier": "wasm-backend:allow-invoke",
///   "allow": [{ "module": "greeter", "command": "greet" }],
///   "deny": [{ "module": "greeter", "command": "admin*" }]
/// }
/// ```
///
/// `module` and `command` support `*` wildcards; omitting `command` matches
/// every command of the module.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InvokeScope {
    pub module: String,
    #[serde(default)]
    pub command: Option<String>,
}

impl InvokeScope {
    fn matches(&self, module: &str, command: &str) -> bool {
        wildcard_match(&self.module, module)
            && self.command.as_deref().is_none_or(|c| {
                wildcard_match(c, command) || naming::to_kebab(c) == naming::to_kebab(command)
            })
    }
}

/// Deny entries always win. If there are allow entries, one must match;
/// otherwise the module must be exposed (app modules are by default,
/// external plugins aren't).
fn check_scope(
    allows: &[&InvokeScope],
    denies: &[&InvokeScope],
    exposed: bool,
    module: &str,
    command: &str,
) -> bool {
    if denies.iter().any(|s| s.matches(module, command)) {
        return false;
    }
    if allows.is_empty() {
        exposed
    } else {
        allows.iter().any(|s| s.matches(module, command))
    }
}

#[command]
pub(crate) async fn invoke<R: Runtime>(
    app: AppHandle<R>,
    command_scope: CommandScope<InvokeScope>,
    global_scope: GlobalScope<InvokeScope>,
    module: String,
    command: String,
    args: Option<Value>,
) -> Result<Value> {
    let backend = app.wasm_backend();
    let allows: Vec<&InvokeScope> = command_scope
        .allows()
        .iter()
        .chain(global_scope.allows())
        .map(|s| s.as_ref())
        .collect();
    let denies: Vec<&InvokeScope> = command_scope
        .denies()
        .iter()
        .chain(global_scope.denies())
        .map(|s| s.as_ref())
        .collect();
    if !check_scope(
        &allows,
        &denies,
        backend.is_exposed(&module),
        &module,
        &command,
    ) {
        return Err(Error::NotAllowed(format!(
            "invoking `{module}`/`{command}` is not allowed by the `wasm-backend:allow-invoke` scope"
        )));
    }
    backend
        .invoke_async(&module, &command, args.unwrap_or(Value::Null))
        .await
}

#[command]
pub(crate) fn list_modules<R: Runtime>(app: AppHandle<R>) -> Vec<ModuleInfo> {
    app.wasm_backend().modules()
}

#[command]
pub(crate) async fn reload_module<R: Runtime>(
    app: AppHandle<R>,
    module: String,
) -> Result<ModuleInfo> {
    let backend = app.wasm_backend().clone();
    tauri::async_runtime::spawn_blocking(move || backend.reload(&module))
        .await
        .map_err(|e| Error::Runtime(e.to_string()))?
}

#[command]
pub(crate) fn typescript<R: Runtime>(app: AppHandle<R>, module: String) -> Result<String> {
    app.wasm_backend().typescript(&module)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope(module: &str, command: Option<&str>) -> InvokeScope {
        InvokeScope {
            module: module.into(),
            command: command.map(Into::into),
        }
    }

    #[test]
    fn scopes() {
        let greet = scope("greeter", Some("greet"));
        let any_greeter = scope("greeter", None);
        let admin = scope("*", Some("admin*"));

        // No scope: exposure decides.
        assert!(check_scope(&[], &[], true, "greeter", "greet"));
        assert!(!check_scope(&[], &[], false, "plugin", "run"));
        // Allow lists are exhaustive, even for exposed modules.
        assert!(check_scope(&[&greet], &[], true, "greeter", "greet"));
        assert!(!check_scope(&[&greet], &[], true, "greeter", "add"));
        assert!(check_scope(&[&any_greeter], &[], false, "greeter", "add"));
        // Command names match across naming conventions.
        assert!(check_scope(
            &[&scope("m", Some("get_user"))],
            &[],
            false,
            "m",
            "getUser"
        ));
        // Denies win.
        assert!(!check_scope(
            &[&any_greeter],
            &[&admin],
            true,
            "greeter",
            "adminReset"
        ));
    }
}
