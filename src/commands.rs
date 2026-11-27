use serde::Deserialize;
use serde_json::Value;
use tauri::{
    AppHandle, Runtime, command,
    ipc::{CommandScope, GlobalScope},
};

use wasm_backend_core::{name_matches, wildcard_match};

use crate::{Error, InactiveModule, ModuleInfo, Result, WasmBackendExt};

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
            && self
                .command
                .as_deref()
                .is_none_or(|c| name_matches(c, command))
    }
}

/// Deny entries always win. Otherwise a module is invocable if it is exposed
/// (app modules are by default, external plugins aren't) or an allow entry
/// matches. Like Tauri capabilities, allow entries only add access.
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
    exposed || allows.iter().any(|s| s.matches(module, command))
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
    backend.module(&module)?;
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
        backend.core().is_exposed(&module),
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

/// Scope entry of the commands that manage modules (`reload-module`,
/// `enable-module`, `disable-module`), used in capability files to restrict
/// which modules a window may manage:
///
/// ```json
/// {
///   "identifier": "wasm-backend:allow-disable-module",
///   "allow": [{ "module": "plugin-*" }],
///   "deny": [{ "module": "plugin-core" }]
/// }
/// ```
///
/// `module` supports `*` wildcards. Without `allow` entries the command
/// works on every module; `deny` entries always win.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModuleScope {
    pub module: String,
}

/// Unlike `invoke`, these commands are off unless a capability allows them,
/// so allowing one without a scope means all modules.
fn check_module_scope(allows: &[&ModuleScope], denies: &[&ModuleScope], module: &str) -> bool {
    let matches = |s: &&ModuleScope| wildcard_match(&s.module, module);
    !denies.iter().any(matches) && (allows.is_empty() || allows.iter().any(matches))
}

fn manage(scope: &CommandScope<ModuleScope>, action: &str, module: &str) -> Result<()> {
    let allows: Vec<&ModuleScope> = scope.allows().iter().map(|s| s.as_ref()).collect();
    let denies: Vec<&ModuleScope> = scope.denies().iter().map(|s| s.as_ref()).collect();
    if check_module_scope(&allows, &denies, module) {
        Ok(())
    } else {
        Err(Error::NotAllowed(format!(
            "{action} `{module}` is not allowed by the scope of this command"
        )))
    }
}

/// Run blocking work (compiling a module) off the IPC thread.
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T> {
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(|e| Error::Runtime(e.to_string()))?
}

#[command]
pub(crate) fn list_modules<R: Runtime>(app: AppHandle<R>) -> Vec<ModuleInfo> {
    app.wasm_backend().modules()
}

#[command]
pub(crate) fn list_inactive_modules<R: Runtime>(app: AppHandle<R>) -> Vec<InactiveModule> {
    app.wasm_backend().inactive()
}

#[command]
pub(crate) async fn reload_module<R: Runtime>(
    app: AppHandle<R>,
    command_scope: CommandScope<ModuleScope>,
    module: String,
) -> Result<ModuleInfo> {
    manage(&command_scope, "reloading", &module)?;
    let backend = app.wasm_backend().clone();
    blocking(move || backend.reload(&module)).await
}

#[command]
pub(crate) async fn enable_module<R: Runtime>(
    app: AppHandle<R>,
    command_scope: CommandScope<ModuleScope>,
    module: String,
) -> Result<ModuleInfo> {
    manage(&command_scope, "enabling", &module)?;
    let backend = app.wasm_backend().clone();
    blocking(move || backend.enable(&module)).await
}

#[command]
pub(crate) fn disable_module<R: Runtime>(
    app: AppHandle<R>,
    command_scope: CommandScope<ModuleScope>,
    module: String,
) -> Result<()> {
    manage(&command_scope, "disabling", &module)?;
    app.wasm_backend().disable(&module)
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
    fn module_scopes() {
        let plugins = ModuleScope {
            module: "plugin-*".into(),
        };
        let core = ModuleScope {
            module: "plugin-core".into(),
        };
        // Allowed without a scope: every module.
        assert!(check_module_scope(&[], &[], "greeter"));
        // Allow entries narrow it down, deny entries win.
        assert!(check_module_scope(&[&plugins], &[], "plugin-notes"));
        assert!(!check_module_scope(&[&plugins], &[], "greeter"));
        assert!(!check_module_scope(&[&plugins], &[&core], "plugin-core"));
        assert!(!check_module_scope(&[], &[&core], "plugin-core"));
        assert!(check_module_scope(&[], &[&core], "greeter"));
        // A typo in an entry is an error, not a scope that matches nothing.
        assert!(serde_json::from_str::<ModuleScope>(r#"{ "modul": "x" }"#).is_err());
    }

    #[test]
    fn scopes() {
        let greet = scope("greeter", Some("greet"));
        let any_greeter = scope("greeter", None);
        let admin = scope("*", Some("admin*"));

        // No scope: exposure decides.
        assert!(check_scope(&[], &[], true, "greeter", "greet"));
        assert!(!check_scope(&[], &[], false, "plugin", "run"));
        // Allow entries add access; they don't revoke it from exposed modules.
        assert!(check_scope(&[&greet], &[], true, "greeter", "add"));
        assert!(check_scope(&[&greet], &[], false, "greeter", "greet"));
        assert!(!check_scope(&[&greet], &[], false, "greeter", "add"));
        assert!(check_scope(&[&any_greeter], &[], false, "greeter", "add"));
        assert!(!check_scope(&[&any_greeter], &[], false, "plugin", "run"));
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
