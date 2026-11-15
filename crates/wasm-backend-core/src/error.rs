use serde::{Serialize, ser::Serializer};

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// An error reported by the [`Platform`](crate::Platform) (e.g. a
    /// `tauri::Error` when resolving a directory).
    #[error(transparent)]
    Platform(Box<dyn std::error::Error + Send + Sync>),
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
    #[error("{0}")]
    NotAllowed(String),
    #[error(
        "reentrant call into WASM module `{0}` (a host command called back into the module that invoked it)"
    )]
    Reentrant(String),
    #[error("the `{0}` engine is not enabled; enable the `{0}` cargo feature of wasm-backend-core")]
    EngineDisabled(&'static str),
}

impl Error {
    /// Wrap an error coming from the platform integration.
    pub fn platform(error: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> Self {
        Self::Platform(error.into())
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
