//! Glue between the module registry and the WASM engines.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{Result, capabilities::Capabilities, config::EngineKind};

#[cfg(feature = "component")]
pub(crate) mod component;
#[cfg(feature = "extism")]
pub(crate) mod extism;

/// Type-erased access to the host app, implemented by the registry.
pub(crate) trait Host: Send + Sync {
    fn emit(&self, event: &str, payload: Value) -> std::result::Result<(), String>;
    /// Run a registered host command (`command` as returned by
    /// [`Host::resolve_command`]).
    fn invoke(
        &self,
        module: &str,
        command: &str,
        args: Value,
    ) -> std::result::Result<Value, String>;
    /// The registered host command `name` refers to: an exact match, else
    /// one whose name matches across naming conventions.
    fn resolve_command(&self, name: &str) -> Option<String>;
    /// Names of every registered host command, sorted.
    fn command_names(&self) -> Vec<String>;
}

/// Host functions as seen by one module: every call is checked against the
/// module's capabilities.
pub(crate) struct ModuleHost {
    pub module: String,
    pub capabilities: Capabilities,
    pub host: Arc<dyn Host>,
}

impl ModuleHost {
    pub fn log(&self, level: log::Level, message: &str) {
        log::log!(target: "wasm_backend", level, "[{}] {}", self.module, message);
    }

    pub fn emit(&self, event: &str, payload: &str) -> std::result::Result<(), String> {
        if !self.capabilities.can_emit(event) {
            return Err(format!(
                "module `{}` is not allowed to emit event `{event}` (add it to `capabilities.events.emit`)",
                self.module
            ));
        }
        let payload = parse_json_arg(payload)?;
        self.host.emit(event, payload)
    }

    /// The `invoke` host function: JSON strings in and out.
    pub fn invoke(&self, command: &str, args: &str) -> std::result::Result<String, String> {
        let command = self.resolve_command(command)?;
        let args = parse_json_arg(args)?;
        let out = self.host.invoke(&self.module, &command, args)?;
        serde_json::to_string(&out).map_err(|e| e.to_string())
    }

    /// Run a host command resolved with [`ModuleHost::resolve_command`].
    #[cfg_attr(not(feature = "component"), allow(dead_code))]
    pub fn invoke_resolved(
        &self,
        command: &str,
        args: Value,
    ) -> std::result::Result<Value, String> {
        self.host.invoke(&self.module, command, args)
    }

    /// The registered host command this module may call as `command`, or
    /// why it can't. Capabilities are checked first, so a module learns
    /// nothing about commands it isn't allowed to call.
    pub fn resolve_command(&self, command: &str) -> std::result::Result<String, String> {
        let registered = self.host.resolve_command(command);
        let allowed = self.capabilities.can_invoke(command)
            || registered
                .as_deref()
                .is_some_and(|c| self.capabilities.can_invoke(c));
        if !allowed {
            return Err(format!(
                "module `{}` is not allowed to invoke host command `{command}` (add {} to `capabilities.hostCommands`)",
                self.module,
                js_string(command),
            ));
        }
        registered.ok_or_else(|| {
            let callable: Vec<String> = self
                .host
                .command_names()
                .into_iter()
                .filter(|c| self.capabilities.can_invoke(c))
                .collect();
            if callable.is_empty() {
                format!("unknown host command `{command}` (no host command registered with `host_command` matches it)")
            } else {
                format!(
                    "unknown host command `{command}` (this module may call: {})",
                    callable.join(", ")
                )
            }
        })
    }
}

fn parse_json_arg(s: &str) -> std::result::Result<Value, String> {
    if s.trim().is_empty() {
        Ok(Value::Null)
    } else {
        serde_json::from_str(s).map_err(|e| format!("invalid JSON: {e}"))
    }
}

/// A loaded, instantiated module.
pub(crate) trait ModuleInstance: Send {
    /// Call a command with JSON arguments (an object keyed by parameter name
    /// for components, any JSON for Extism).
    fn call(&mut self, command: &str, args: Value) -> Result<Value>;
    /// Deliver an event. A module without an event handler ignores it.
    fn on_event(&mut self, event: &str, payload: &Value) -> Result<()>;
    /// Whether the module exports an event handler.
    fn handles_events(&self) -> bool;
    /// Commands exported by the module.
    fn commands(&self) -> Vec<CommandInfo>;
    /// Host commands the module imports as typed functions (checked when it
    /// loads), named as registered.
    fn imports(&self) -> Vec<CommandInfo> {
        Vec::new()
    }
    /// TypeScript declarations for the module's commands, if the engine has
    /// type information.
    fn typescript(&self, _module: &str) -> Option<String> {
        None
    }
}

/// Description of a command exported by a module.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct CommandInfo {
    /// Name to pass to `invoke`.
    pub name: String,
    /// Parameters (WIT modules only).
    pub params: Vec<ParamInfo>,
    /// Result type in WIT syntax (WIT modules only).
    pub result: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct ParamInfo {
    /// Argument key, as sent from JavaScript (camelCase).
    pub name: String,
    /// Type in WIT syntax.
    #[serde(rename = "type")]
    pub ty: String,
}

/// A JavaScript string literal (JSON strings are valid JS).
pub(crate) fn js_string(s: &str) -> String {
    serde_json::Value::String(s.to_string()).to_string()
}

/// Detect the engine to use for a binary.
pub(crate) fn detect(bytes: &[u8], configured: EngineKind) -> EngineKind {
    match configured {
        EngineKind::Auto if is_component(bytes) => EngineKind::Component,
        EngineKind::Auto => EngineKind::Extism,
        other => other,
    }
}

/// Components use layer 1 in the preamble: `\0asm` followed by
/// `0d 00 01 00`; core modules use version 1, layer 0.
pub(crate) fn is_component(bytes: &[u8]) -> bool {
    bytes.len() >= 8 && &bytes[0..4] == b"\0asm" && bytes[6..8] == [1, 0]
}

/// Instantiate a module with the given engine.
pub(crate) fn instantiate(
    kind: EngineKind,
    bytes: &[u8],
    host: Arc<ModuleHost>,
) -> Result<Box<dyn ModuleInstance>> {
    let _ = (&bytes, &host);
    match kind {
        #[cfg(feature = "component")]
        EngineKind::Component => Ok(Box::new(component::ComponentInstance::new(bytes, host)?)),
        #[cfg(not(feature = "component"))]
        EngineKind::Component => Err(crate::Error::EngineDisabled("component")),
        #[cfg(feature = "extism")]
        EngineKind::Extism => Ok(Box::new(extism::ExtismInstance::new(bytes, host)?)),
        #[cfg(not(feature = "extism"))]
        EngineKind::Extism => Err(crate::Error::EngineDisabled("extism")),
        EngineKind::Auto => unreachable!("engine kind is detected before instantiation"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_components() {
        assert!(is_component(b"\0asm\x0d\x00\x01\x00"));
        assert!(!is_component(b"\0asm\x01\x00\x00\x00"));
        assert_eq!(
            detect(b"\0asm\x01\x00\x00\x00", EngineKind::Auto),
            EngineKind::Extism
        );
        assert_eq!(
            detect(b"\0asm\x01\x00\x00\x00", EngineKind::Component),
            EngineKind::Component
        );
    }
}
