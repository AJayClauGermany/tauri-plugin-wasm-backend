//! Runtime-agnostic glue between the module registry and the WASM engines.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{Result, capabilities::Capabilities, config::EngineKind};

#[cfg(feature = "component")]
pub(crate) mod component;
#[cfg(feature = "extism")]
pub(crate) mod extism;
pub(crate) mod naming;

/// Type-erased access to the Tauri app, implemented by the registry.
pub(crate) trait Host: Send + Sync {
    fn emit(&self, event: &str, payload: Value) -> std::result::Result<(), String>;
    fn invoke(
        &self,
        module: &str,
        command: &str,
        args: Value,
    ) -> std::result::Result<Value, String>;
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

    pub fn invoke(&self, command: &str, args: &str) -> std::result::Result<String, String> {
        if !self.capabilities.can_invoke(command) {
            return Err(format!(
                "module `{}` is not allowed to invoke host command `{command}` (add it to `capabilities.hostCommands`)",
                self.module
            ));
        }
        let args = parse_json_arg(args)?;
        let out = self.host.invoke(&self.module, command, args)?;
        serde_json::to_string(&out).map_err(|e| e.to_string())
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
    /// Commands exported by the module.
    fn commands(&self) -> Vec<CommandInfo>;
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
