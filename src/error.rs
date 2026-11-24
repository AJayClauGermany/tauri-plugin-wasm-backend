//! The plugin's error type. It mirrors [`wasm_backend_core::Error`] and adds
//! the Tauri variant, so `?` on a `tauri::Error` keeps working in code that
//! returns this crate's [`Result`].

use serde::{Serialize, ser::Serializer};

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Tauri(#[from] tauri::Error),
    #[error("WASM module `{0}` is not loaded")]
    ModuleNotFound(String),
    #[error("WASM module `{module}` has no command `{command}`")]
    CommandNotFound { module: String, command: String },
    #[error("WASM module `{module}` failed to load: {message}")]
    Load { module: String, message: String },
    #[error("invalid arguments for `{command}`: {message}")]
    InvalidArgs { command: String, message: String },
    /// The command ran and returned an error value (e.g. the `err` case of a
    /// WIT `result`, or an Extism plugin error). Serialized as the value
    /// itself, so the frontend receives exactly what the module returned.
    #[error("{}", display_value(.0))]
    Command(serde_json::Value),
    /// The module trapped, ran out of fuel/time/memory, or returned malformed
    /// output.
    #[error("WASM runtime error: {0}")]
    Runtime(String),
    /// The module trapped more often than its `maxRestarts` capability
    /// allows and stays stopped until it is reloaded.
    #[error("WASM module `{module}` was stopped after trapping {traps} times and must be reloaded")]
    Stopped { module: String, traps: u32 },
    /// A module's state could not be read from or written to its store.
    #[error("WASM module state: {0}")]
    State(String),
    #[error("{0}")]
    NotAllowed(String),
    #[error(
        "reentrant call into WASM module `{0}` (a host command called back into the module that invoked it)"
    )]
    Reentrant(String),
    #[error(
        "the `{0}` engine is not enabled; enable the `{0}` cargo feature of tauri-plugin-wasm-backend"
    )]
    EngineDisabled(&'static str),
}

impl From<wasm_backend_core::Error> for Error {
    fn from(error: wasm_backend_core::Error) -> Self {
        use wasm_backend_core::Error as Core;
        match error {
            Core::Io(e) => Error::Io(e),
            // `TauriPlatform` only reports `tauri::Error`s.
            Core::Platform(e) => match e.downcast::<tauri::Error>() {
                Ok(e) => Error::Tauri(*e),
                Err(e) => Error::Io(std::io::Error::other(e)),
            },
            Core::ModuleNotFound(m) => Error::ModuleNotFound(m),
            Core::CommandNotFound { module, command } => Error::CommandNotFound { module, command },
            Core::Load { module, message } => Error::Load { module, message },
            Core::InvalidArgs { command, message } => Error::InvalidArgs { command, message },
            Core::Command(v) => Error::Command(v),
            Core::Runtime(m) => Error::Runtime(m),
            Core::Stopped { module, traps } => Error::Stopped { module, traps },
            Core::State(m) => Error::State(m),
            Core::NotAllowed(m) => Error::NotAllowed(m),
            Core::Reentrant(m) => Error::Reentrant(m),
            Core::EngineDisabled(e) => Error::EngineDisabled(e),
        }
    }
}

fn display_value(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

impl Serialize for Error {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Error::Command(value) => value.serialize(serializer),
            other => serializer.serialize_str(other.to_string().as_ref()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tauri_errors_convert() {
        // `?` on a `tauri::Error` in a function returning this crate's Result.
        fn f() -> Result<()> {
            Err(tauri::Error::UnknownPath)?
        }
        assert!(matches!(f(), Err(Error::Tauri(_))));
        // Platform errors from the core come back as the Tauri variant.
        let core = wasm_backend_core::Error::platform(tauri::Error::UnknownPath);
        assert!(matches!(Error::from(core), Error::Tauri(_)));
    }
}
