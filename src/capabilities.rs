//! What a WASM module is allowed to do.
//!
//! Capabilities are deny-by-default: a module with `Capabilities::default()`
//! can compute and log, but it can't emit or listen to events, call host
//! commands, touch the filesystem, the network or the environment.

use std::{collections::BTreeMap, path::PathBuf};

use serde::{Deserialize, Serialize};

/// Capabilities granted to a module.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Capabilities {
    /// Host commands (registered with `Builder::host_command`) the module may
    /// invoke. Supports `*` wildcards.
    #[serde(default)]
    pub host_commands: Vec<String>,
    /// Events the module may emit and receive.
    #[serde(default)]
    pub events: EventCapabilities,
    /// WASI access (filesystem, environment, stdio, sockets).
    #[serde(default)]
    pub wasi: WasiCapabilities,
    /// Hosts the module may reach with Extism's HTTP functions. Supports `*`
    /// wildcards (e.g. `*.example.com`). Extism only.
    #[serde(default)]
    pub allowed_hosts: Vec<String>,
    /// Maximum linear memory, in MiB.
    #[serde(default)]
    pub memory_limit_mb: Option<u32>,
    /// Maximum wall-clock time for a single call, in milliseconds.
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

/// Event related capabilities.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EventCapabilities {
    /// Event names the module may emit. Supports `*` wildcards.
    #[serde(default)]
    pub emit: Vec<String>,
    /// Exact event names the module subscribes to. They are delivered to the
    /// module's `on-event` export (`on_event` for Extism).
    #[serde(default)]
    pub listen: Vec<String>,
}

/// WASI related capabilities.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WasiCapabilities {
    /// Forward the module's stdout/stderr to the app's stdout/stderr.
    #[serde(default)]
    pub stdio: bool,
    /// Environment variables visible to the module.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Host directories made available to the module.
    #[serde(default)]
    pub preopens: Vec<Preopen>,
    /// Allow outgoing/incoming TCP and UDP sockets and DNS lookups (WIT only).
    #[serde(default)]
    pub network: bool,
}

/// A host directory mapped into the module's filesystem.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Preopen {
    /// Host directory. May start with `$APPDATA`, `$APPLOCALDATA`,
    /// `$APPCACHE`, `$APPCONFIG`, `$RESOURCE` or `$TEMP`; other relative
    /// paths are resolved against the app data directory. Writable
    /// directories are created if missing.
    pub host: PathBuf,
    /// Path the directory is visible under inside the module.
    pub guest: String,
    #[serde(default)]
    pub read_only: bool,
}

impl Capabilities {
    pub fn allow_host_command(mut self, pattern: impl Into<String>) -> Self {
        self.host_commands.push(pattern.into());
        self
    }

    pub fn allow_emit(mut self, pattern: impl Into<String>) -> Self {
        self.events.emit.push(pattern.into());
        self
    }

    pub fn listen(mut self, event: impl Into<String>) -> Self {
        self.events.listen.push(event.into());
        self
    }

    pub fn stdio(mut self, enabled: bool) -> Self {
        self.wasi.stdio = enabled;
        self
    }

    pub fn timeout_ms(mut self, ms: u64) -> Self {
        self.timeout_ms = Some(ms);
        self
    }

    pub fn memory_limit_mb(mut self, mb: u32) -> Self {
        self.memory_limit_mb = Some(mb);
        self
    }

    pub(crate) fn can_invoke(&self, command: &str) -> bool {
        any_match(&self.host_commands, command)
    }

    pub(crate) fn can_emit(&self, event: &str) -> bool {
        any_match(&self.events.emit, event)
    }
}

pub(crate) fn any_match(patterns: &[String], value: &str) -> bool {
    patterns.iter().any(|p| wildcard_match(p, value))
}

/// Glob-like matching where `*` matches any (possibly empty) sequence.
pub(crate) fn wildcard_match(pattern: &str, value: &str) -> bool {
    let p = pattern.as_bytes();
    let v = value.as_bytes();
    let (mut pi, mut vi) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while vi < v.len() {
        if pi < p.len() && p[pi] == b'*' {
            star = Some((pi, vi));
            pi += 1;
        } else if pi < p.len() && p[pi] == v[vi] {
            pi += 1;
            vi += 1;
        } else if let Some((sp, sv)) = star {
            pi = sp + 1;
            vi = sv + 1;
            star = Some((sp, sv + 1));
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == b'*' {
        pi += 1;
    }
    pi == p.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wildcards() {
        assert!(wildcard_match("*", ""));
        assert!(wildcard_match("*", "anything"));
        assert!(wildcard_match("greeter://*", "greeter://greeted"));
        assert!(!wildcard_match("greeter://*", "other://greeted"));
        assert!(wildcard_match("a*c*e", "abcde"));
        assert!(!wildcard_match("a*c*e", "abcdf"));
        assert!(wildcard_match("exact", "exact"));
        assert!(!wildcard_match("exact", "exactly"));
    }

    #[test]
    fn deny_by_default() {
        let caps = Capabilities::default();
        assert!(!caps.can_emit("x"));
        assert!(!caps.can_invoke("x"));
        let caps = caps.allow_emit("app://*").allow_host_command("now");
        assert!(caps.can_emit("app://tick"));
        assert!(caps.can_invoke("now"));
        assert!(!caps.can_invoke("now2"));
    }
}
