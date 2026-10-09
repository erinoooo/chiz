//! Paired tablets: device_id -> 32-byte secret + display name, persisted
//! to `paired.json` (0600 on Unix; the per-user %APPDATA% ACL on Windows).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::util::{b64, b64_decode};

#[derive(Clone, Debug, PartialEq)]
pub struct PairedTablet {
    pub secret: [u8; 32],
    pub name: String,
}

#[derive(Default)]
pub struct PairedStore {
    tablets: BTreeMap<String, PairedTablet>,
    /// Where `save` writes. `None` = in-memory only (tests, test hooks).
    path: Option<PathBuf>,
}

impl PairedStore {
    #[cfg(test)]
    pub fn in_memory() -> Self {
        Self::default()
    }

    /// Load from `path` (missing = empty, corrupt = empty + log) and keep
    /// saving there.
    pub fn open(path: PathBuf) -> Self {
        let mut me = Self {
            tablets: Self::read(&path),
            path: Some(path),
        };
        me.tablets.retain(|id, _| !id.is_empty());
        if !me.tablets.is_empty() {
            eprintln!("paired tablets: {} loaded", me.tablets.len());
        }
        me
    }

    fn read(path: &Path) -> BTreeMap<String, PairedTablet> {
        let mut out = BTreeMap::new();
        let Ok(text) = std::fs::read_to_string(path) else {
            return out;
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
            eprintln!(
                "{} is unreadable; starting with no paired tablets",
                path.display()
            );
            return out;
        };
        for (id, e) in v.as_object().into_iter().flatten() {
            let secret = e
                .get("secret")
                .and_then(|s| s.as_str())
                .and_then(|s| b64_decode(s).ok())
                .and_then(|b| <[u8; 32]>::try_from(b.as_slice()).ok());
            if let Some(secret) = secret {
                let name = e.get("name").and_then(|n| n.as_str()).unwrap_or("tablet");
                out.insert(
                    id.clone(),
                    PairedTablet {
                        secret,
                        name: name.to_string(),
                    },
                );
            }
        }
        out
    }

    pub fn get(&self, device_id: &str) -> Option<&PairedTablet> {
        self.tablets.get(device_id)
    }

    /// (device_id, name), sorted by id.
    pub fn list(&self) -> Vec<(String, String)> {
        self.tablets
            .iter()
            .map(|(id, t)| (id.clone(), t.name.clone()))
            .collect()
    }

    /// Insert or replace (a re-pair gets a fresh secret) and persist.
    pub fn insert(&mut self, device_id: &str, tablet: PairedTablet) {
        self.tablets.insert(device_id.to_string(), tablet);
        self.persist();
    }

    /// Insert without persisting (the CHIZ_TEST_PAIR hook).
    pub fn insert_ephemeral(&mut self, device_id: &str, tablet: PairedTablet) {
        self.tablets.insert(device_id.to_string(), tablet);
    }

    pub fn remove(&mut self, device_id: &str) -> bool {
        let had = self.tablets.remove(device_id).is_some();
        if had {
            self.persist();
        }
        had
    }

    fn persist(&self) {
        if let Some(p) = &self.path {
            if let Err(e) = self.write(p) {
                eprintln!(
                    "CHIZ: could not save paired tablets to {}: {e}",
                    p.display()
                );
            }
        }
    }

    /// Write-then-rename so a crash never leaves a half-written file.
    fn write(&self, path: &Path) -> std::io::Result<()> {
        let obj: serde_json::Map<String, serde_json::Value> = self
            .tablets
            .iter()
            .map(|(id, t)| {
                (
                    id.clone(),
                    serde_json::json!({"secret": b64(&t.secret), "name": t.name}),
                )
            })
            .collect();
        let body = serde_json::to_vec_pretty(&serde_json::Value::Object(obj))?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        {
            use std::io::Write;
            let mut opts = std::fs::OpenOptions::new();
            opts.write(true).create(true).truncate(true);
            #[cfg(unix)]
            std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
            let mut f = opts.open(&tmp)?;
            f.write_all(&body)?;
            f.sync_all()?;
        }
        std::fs::rename(&tmp, path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tab(b: u8, name: &str) -> PairedTablet {
        PairedTablet {
            secret: [b; 32],
            name: name.into(),
        }
    }

    #[test]
    fn survives_restart_forget_and_corruption() {
        let dir = std::env::temp_dir().join(format!("chiz-store-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("paired.json");
        let mut a = PairedStore::open(path.clone());
        a.insert("dev-1", tab(7, "Tab S9"));
        a.insert("dev-2", tab(9, "Other"));
        let mut b = PairedStore::open(path.clone());
        assert_eq!(b.get("dev-1"), Some(&tab(7, "Tab S9")));
        assert_eq!(b.list().len(), 2);
        assert!(b.remove("dev-1"));
        let c = PairedStore::open(path.clone());
        assert!(c.get("dev-1").is_none() && c.get("dev-2").is_some());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
        std::fs::write(&path, b"{not json").unwrap();
        assert!(PairedStore::open(path).list().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn in_memory_never_writes() {
        let mut s = PairedStore::in_memory();
        s.insert("x", tab(1, "t"));
        assert_eq!(s.list(), vec![("x".to_string(), "t".to_string())]);
    }
}
