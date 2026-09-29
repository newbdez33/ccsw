//! `mappings.json`: directory → account. Keys are normalized absolute paths; the
//! value is an identity, never a slot number (slots get reused).

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::errors::{CswitchError, Result};
use crate::fsutil::{read_json, write_json_private};
use crate::model::{Identity, now_iso};
use crate::paths::Paths;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MappingEntry {
    pub email: String,
    #[serde(default)]
    pub account_id: String,
    #[serde(default)]
    pub added: String,
}

impl MappingEntry {
    pub fn identity(&self) -> Identity {
        Identity::new(self.email.clone(), self.account_id.clone())
    }
}

/// In-memory view of `mappings.json`. Mutations are local until `save`.
#[derive(Debug, Clone)]
pub struct MappingStore {
    path: PathBuf,
    mappings: BTreeMap<String, MappingEntry>,
}

impl MappingStore {
    /// Missing, corrupt, or wrongly shaped files read as empty.
    pub fn load(paths: &Paths) -> Self {
        let path = paths.mappings_file();
        let mappings = read_json(&path)
            .ok()
            .flatten()
            .and_then(|root| root.get("mappings").cloned())
            .and_then(|raw| serde_json::from_value(raw).ok())
            .unwrap_or_default();
        Self { path, mappings }
    }

    pub fn save(&self) -> Result<()> {
        let value = serde_json::json!({
            "schemaVersion": 1,
            "mappings": serde_json::to_value(&self.mappings).unwrap_or(Value::Null),
        });
        write_json_private(&self.path, &value)
            .map_err(|err| CswitchError::config(format!("{}: {err}", self.path.display())))
    }

    /// Expand `~`, absolutize, resolve symlinks as far as the path exists, normalize the
    /// rest lexically (so `p`, `p/` and `p/.` share one key). Case-folded on Windows.
    pub fn normalize_path(path: &Path) -> PathBuf {
        let expanded = expand_home(path);
        let absolute = if expanded.is_absolute() {
            expanded
        } else {
            std::env::current_dir()
                .map(|cwd| cwd.join(&expanded))
                .unwrap_or(expanded)
        };
        let resolved = resolve_existing_prefix(&absolute);
        if cfg!(windows) {
            PathBuf::from(resolved.to_string_lossy().to_lowercase())
        } else {
            resolved
        }
    }

    fn key(path: &Path) -> String {
        Self::normalize_path(path).to_string_lossy().into_owned()
    }

    /// Map `path` to `identity`; returns the identity it replaced, if any.
    pub fn set(&mut self, path: &Path, identity: &Identity) -> Option<Identity> {
        let entry = MappingEntry {
            email: identity.email.clone(),
            account_id: identity.account_id.clone(),
            added: now_iso(),
        };
        self.mappings
            .insert(Self::key(path), entry)
            .map(|previous| previous.identity())
    }

    /// Exact-key removal; returns whether a mapping existed.
    pub fn remove(&mut self, path: &Path) -> bool {
        self.mappings.remove(&Self::key(path)).is_some()
    }

    /// Exact-key lookup.
    pub fn get(&self, path: &Path) -> Option<Identity> {
        self.mappings
            .get(&Self::key(path))
            .map(MappingEntry::identity)
    }

    /// The mapping for `cwd` itself or its nearest mapped ancestor (component-wise:
    /// `/a/b` does not cover `/a/bc`).
    pub fn resolve(&self, cwd: &Path) -> Option<(PathBuf, Identity)> {
        let target = Self::normalize_path(cwd);
        self.mappings
            .iter()
            .map(|(key, entry)| (PathBuf::from(key), entry))
            .filter(|(dir, _)| target.starts_with(dir))
            .max_by_key(|(dir, _)| dir.components().count())
            .map(|(dir, entry)| (dir, entry.identity()))
    }

    /// Drop every mapping to `identity`; returns how many were removed.
    pub fn prune(&mut self, identity: &Identity) -> usize {
        let before = self.mappings.len();
        self.mappings
            .retain(|_, entry| entry.identity() != *identity);
        before - self.mappings.len()
    }

    /// All mappings in key order.
    pub fn entries(&self) -> Vec<(PathBuf, Identity)> {
        self.mappings
            .iter()
            .map(|(key, entry)| (PathBuf::from(key), entry.identity()))
            .collect()
    }

    pub fn is_empty(&self) -> bool {
        self.mappings.is_empty()
    }
}

fn expand_home(path: &Path) -> PathBuf {
    let Some(home) = dirs::home_dir() else {
        return path.to_path_buf();
    };
    let mut components = path.components();
    match components.next() {
        Some(Component::Normal(first)) if first == "~" => home.join(components.as_path()),
        _ => path.to_path_buf(),
    }
}

/// Canonicalize the longest existing prefix and append the remainder lexically
/// (`realpath` semantics for a path that does not fully exist).
fn resolve_existing_prefix(path: &Path) -> PathBuf {
    let components: Vec<Component<'_>> = path.components().collect();
    for split in (1..=components.len()).rev() {
        let prefix: PathBuf = components[..split].iter().map(|c| c.as_os_str()).collect();
        if let Ok(canonical) = std::fs::canonicalize(&prefix) {
            return push_lexically(strip_verbatim_prefix(canonical), &components[split..]);
        }
    }
    push_lexically(PathBuf::new(), &components)
}

fn push_lexically(mut out: PathBuf, components: &[Component<'_>]) -> PathBuf {
    for component in components {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

// `canonicalize` on Windows yields `\\?\C:\...`; keys should read like user paths.
fn strip_verbatim_prefix(path: PathBuf) -> PathBuf {
    let text = path.to_string_lossy();
    match text.strip_prefix(r"\\?\") {
        Some(rest) if cfg!(windows) => PathBuf::from(rest),
        _ => path,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::temp_store;

    fn identity(email: &str) -> Identity {
        Identity::new(email, "acct")
    }

    #[test]
    fn normalization_collapses_trailing_and_dot_components() {
        let dir = tempfile::tempdir().unwrap();
        let base = MappingStore::normalize_path(dir.path());
        assert_eq!(MappingStore::normalize_path(&dir.path().join("")), base);
        assert_eq!(MappingStore::normalize_path(&dir.path().join(".")), base);
        assert_eq!(
            MappingStore::normalize_path(&dir.path().join("missing/./deeper/../x")),
            base.join("missing/x"),
            "a non-existent tail is normalized lexically"
        );
        #[cfg(unix)]
        {
            std::fs::create_dir(dir.path().join("real")).unwrap();
            std::os::unix::fs::symlink(dir.path().join("real"), dir.path().join("link")).unwrap();
            assert_eq!(
                MappingStore::normalize_path(&dir.path().join("link/sub")),
                base.join("real/sub")
            );
        }
        // Windows lowercases the whole key, so compare against the normalized home.
        let home = MappingStore::normalize_path(&dirs::home_dir().unwrap());
        assert!(MappingStore::normalize_path(Path::new("~/x")).starts_with(&home));
        let relative = MappingStore::normalize_path(Path::new("rel"));
        assert!(relative.is_absolute());
    }

    #[test]
    fn set_get_remove_and_previous() {
        let (dir, store) = temp_store();
        let mut mappings = MappingStore::load(&store.paths);
        assert!(mappings.is_empty());
        let work = dir.path().join("work");
        assert_eq!(mappings.set(&work, &identity("a@x.com")), None);
        assert_eq!(
            mappings.set(&work.join("."), &identity("b@x.com")),
            Some(identity("a@x.com"))
        );
        assert_eq!(mappings.get(&work), Some(identity("b@x.com")));
        assert_eq!(mappings.get(&work.join("other")), None);
        assert!(mappings.remove(&work));
        assert!(!mappings.remove(&work));
    }

    #[test]
    fn save_and_load_use_the_documented_json() {
        let (dir, store) = temp_store();
        let mut mappings = MappingStore::load(&store.paths);
        let work = dir.path().join("work");
        mappings.set(&work, &identity("a@x.com"));
        mappings.save().unwrap();
        let raw = read_json(&store.paths.mappings_file()).unwrap().unwrap();
        assert_eq!(raw["schemaVersion"], 1);
        let key = MappingStore::normalize_path(&work)
            .to_string_lossy()
            .into_owned();
        let entry = &raw["mappings"][&key];
        assert_eq!(entry["email"], "a@x.com");
        assert_eq!(entry["accountId"], "acct");
        assert!(entry["added"].as_str().unwrap().ends_with('Z'));
        let reloaded = MappingStore::load(&store.paths);
        assert_eq!(reloaded.entries(), mappings.entries());

        std::fs::write(store.paths.mappings_file(), "{\"mappings\": [1]}").unwrap();
        assert!(MappingStore::load(&store.paths).is_empty());
        std::fs::write(store.paths.mappings_file(), "nope").unwrap();
        assert!(MappingStore::load(&store.paths).is_empty());
    }

    #[test]
    fn resolve_picks_the_nearest_component_wise_ancestor() {
        let (dir, store) = temp_store();
        let mut mappings = MappingStore::load(&store.paths);
        let root = dir.path().join("proj");
        mappings.set(&root, &identity("root@x.com"));
        mappings.set(&root.join("sub"), &identity("sub@x.com"));
        let norm = |p: &Path| MappingStore::normalize_path(p);
        assert_eq!(
            mappings.resolve(&root.join("sub/deeper")),
            Some((norm(&root.join("sub")), identity("sub@x.com")))
        );
        assert_eq!(
            mappings.resolve(&root.join("other")),
            Some((norm(&root), identity("root@x.com")))
        );
        assert_eq!(
            mappings.resolve(&root),
            Some((norm(&root), identity("root@x.com")))
        );
        assert_eq!(
            mappings.resolve(&dir.path().join("projx")),
            None,
            "string prefix is not enough"
        );
        assert_eq!(mappings.resolve(dir.path()), None);
    }

    #[test]
    fn prune_and_sorted_entries() {
        let (dir, store) = temp_store();
        let mut mappings = MappingStore::load(&store.paths);
        mappings.set(&dir.path().join("b"), &identity("a@x.com"));
        mappings.set(&dir.path().join("a"), &identity("a@x.com"));
        mappings.set(&dir.path().join("c"), &identity("c@x.com"));
        let keys: Vec<PathBuf> = mappings.entries().into_iter().map(|(p, _)| p).collect();
        assert_eq!(
            keys,
            ["a", "b", "c"]
                .iter()
                .map(|n| MappingStore::normalize_path(&dir.path().join(n)))
                .collect::<Vec<_>>()
        );
        assert_eq!(mappings.prune(&identity("a@x.com")), 2);
        assert_eq!(mappings.prune(&identity("a@x.com")), 0);
        assert_eq!(mappings.entries().len(), 1);
    }
}
