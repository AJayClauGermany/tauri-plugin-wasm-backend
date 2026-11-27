//! Remembers which modules were switched off, for apps that opt in with
//! [`Builder::remember_disabled`](crate::Builder::remember_disabled).

use std::{collections::BTreeSet, path::PathBuf, sync::Mutex};

/// The names of the modules switched off with `disable`, kept in a JSON
/// file so they are still off the next time the app starts.
pub(crate) struct Disabled {
    path: PathBuf,
    names: Mutex<BTreeSet<String>>,
}

impl Disabled {
    /// Read the file. One that is missing or unreadable is an empty list:
    /// a damaged file must not keep the app from starting.
    pub fn open(path: PathBuf) -> Self {
        let names = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
                log::warn!(
                    "ignoring {}: it is not a list of module names ({e})",
                    path.display()
                );
                BTreeSet::new()
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeSet::new(),
            Err(e) => {
                log::warn!("can't read {}: {e}", path.display());
                BTreeSet::new()
            }
        };
        Self {
            path,
            names: Mutex::new(names),
        }
    }

    pub fn names(&self) -> Vec<String> {
        self.lock().iter().cloned().collect()
    }

    /// Remember that `name` is switched off, or forget it. The file is
    /// replaced atomically, so a crash never leaves half a list behind.
    pub fn set(&self, name: &str, disabled: bool) -> std::io::Result<()> {
        let mut names = self.lock();
        let changed = if disabled {
            names.insert(name.to_string())
        } else {
            names.remove(name)
        };
        if !changed {
            return Ok(());
        }
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(
            &tmp,
            serde_json::to_vec(&*names).map_err(std::io::Error::other)?,
        )?;
        std::fs::rename(&tmp, &self.path)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BTreeSet<String>> {
        self.names.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remembers_across_instances() {
        let dir =
            std::env::temp_dir().join(format!("wasm-backend-disabled-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("nested/disabled.json");

        let list = Disabled::open(path.clone());
        assert!(list.names().is_empty());
        // Forgetting what was never remembered writes nothing.
        list.set("notes", false).unwrap();
        assert!(!path.exists());
        list.set("notes", true).unwrap();
        list.set("weather", true).unwrap();
        list.set("notes", true).unwrap();
        assert_eq!(Disabled::open(path.clone()).names(), ["notes", "weather"]);
        list.set("notes", false).unwrap();
        assert_eq!(Disabled::open(path.clone()).names(), ["weather"]);

        // A damaged file is an empty list, and is repaired by the next change.
        std::fs::write(&path, "{").unwrap();
        let list = Disabled::open(path.clone());
        assert!(list.names().is_empty());
        list.set("notes", true).unwrap();
        assert_eq!(Disabled::open(path).names(), ["notes"]);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
