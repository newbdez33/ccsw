//! `mappings.json`: directory → account. Keys are normalized absolute paths; the
//! value is an identity, never a slot number (slots get reused).

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::errors::{CcswError, Result};
use crate::fsutil::{read_json, write_json_private};
use crate::model::{Identity, now_iso};
use crate::paths::Paths;
use crate::provider::Provider;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MappingEntry {
    #[serde(default)]
    pub provider: Provider,
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
    mappings: BTreeMap<String, Vec<MappingEntry>>,
}

impl MappingStore {
    /// Missing, corrupt, or wrongly shaped files read as empty.
    pub fn load(paths: &Paths) -> Self {
        let path = paths.mappings_file();
        let mappings = read_json(&path)
            .ok()
            .flatten()
            .and_then(|root| root.get("mappings").and_then(Value::as_object).cloned())
            .map(|raw| {
                raw.into_iter()
                    .filter_map(|(key, value)| {
                        let entries: Vec<MappingEntry> = if value.is_array() {
                            serde_json::from_value(value).ok()?
                        } else {
                            vec![serde_json::from_value(value).ok()?]
                        };
                        Some((key, entries))
                    })
                    .collect()
            })
            .unwrap_or_default();
        Self { path, mappings }
    }

    pub fn save(&self) -> Result<()> {
        let value = serde_json::json!({
            "schemaVersion": 2,
            "mappings": serde_json::to_value(&self.mappings).unwrap_or(Value::Null),
        });
        write_json_private(&self.path, &value)
            .map_err(|err| CcswError::config(format!("{}: {err}", self.path.display())))
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

    /// Map one provider at `path`; retain the other provider's mapping.
    pub fn set(
        &mut self,
        provider: Provider,
        path: &Path,
        identity: &Identity,
    ) -> Option<Identity> {
        let entries = self.mappings.entry(Self::key(path)).or_default();
        let previous = entries
            .iter()
            .find(|e| e.provider == provider)
            .map(MappingEntry::identity);
        entries.retain(|e| e.provider != provider);
        entries.push(MappingEntry {
            provider,
            email: identity.email.clone(),
            account_id: identity.account_id.clone(),
            added: now_iso(),
        });
        entries.sort_by_key(|entry| entry.provider);
        previous
    }

    /// Remove one provider, or every mapping at the exact path.
    pub fn remove(&mut self, path: &Path, provider: Option<Provider>) -> bool {
        let key = Self::key(path);
        let Some(entries) = self.mappings.get_mut(&key) else {
            return false;
        };
        let before = entries.len();
        entries.retain(|e| provider.is_some_and(|p| e.provider != p));
        let removed = entries.len() != before;
        if entries.is_empty() {
            self.mappings.remove(&key);
        }
        removed
    }

    pub fn get(&self, provider: Provider, path: &Path) -> Option<Identity> {
        self.mappings
            .get(&Self::key(path))?
            .iter()
            .find(|e| e.provider == provider)
            .map(MappingEntry::identity)
    }

    /// Resolve the nearest ancestor independently for each provider.
    pub fn resolve(&self, provider: Provider, cwd: &Path) -> Option<(PathBuf, Identity)> {
        let target = Self::normalize_path(cwd);
        self.mappings
            .iter()
            .filter_map(|(key, entries)| {
                Some((
                    PathBuf::from(key),
                    entries.iter().find(|e| e.provider == provider)?,
                ))
            })
            .filter(|(dir, _)| target.starts_with(dir))
            .max_by_key(|(dir, _)| dir.components().count())
            .map(|(dir, entry)| (dir, entry.identity()))
    }

    pub fn prune(&mut self, provider: Provider, identity: &Identity) -> usize {
        let mut removed = 0;
        self.mappings.retain(|_, entries| {
            let before = entries.len();
            entries.retain(|e| e.provider != provider || e.identity() != *identity);
            removed += before - entries.len();
            !entries.is_empty()
        });
        removed
    }

    pub fn entries(&self) -> Vec<(PathBuf, Provider, Identity)> {
        self.mappings
            .iter()
            .flat_map(|(key, entries)| {
                entries
                    .iter()
                    .map(move |e| (PathBuf::from(key), e.provider, e.identity()))
            })
            .collect()
    }

    pub fn is_empty(&self) -> bool {
        self.mappings.values().all(Vec::is_empty)
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
    fn reads_multiple_provider_entries_at_one_directory() {
        let (dir, store) = temp_store();
        let work = MappingStore::normalize_path(&dir.path().join("work"));
        write_json_private(
            &store.paths.mappings_file(),
            &serde_json::json!({
                "schemaVersion": 2,
                "mappings": { work.to_string_lossy(): [
                    {"provider": "codex", "email": "one@x.com", "accountId": "acct"},
                    {"provider": "claude", "email": "two@x.com", "accountId": "org"}
                ] }
            }),
        )
        .unwrap();
        let mappings = MappingStore::load(&store.paths);
        assert_eq!(mappings.entries().len(), 2);
    }

    #[test]
    fn provider_mappings_resolve_and_prune_independently() {
        let (dir, store) = temp_store();
        let root = dir.path().join("work");
        let child = root.join("child");
        let mut mappings = MappingStore::load(&store.paths);
        let same = identity("shared@x.com");
        mappings.set(Provider::Codex, &root, &same);
        mappings.set(Provider::Claude, &root, &same);
        mappings.set(Provider::Codex, &child, &identity("child@x.com"));
        assert_eq!(
            mappings
                .resolve(Provider::Claude, &child.join("src"))
                .unwrap()
                .1,
            same
        );
        assert_eq!(
            mappings
                .resolve(Provider::Codex, &child.join("src"))
                .unwrap()
                .1,
            identity("child@x.com")
        );
        assert_eq!(mappings.prune(Provider::Codex, &same), 1);
        assert_eq!(mappings.get(Provider::Claude, &root), Some(same));
        assert!(!mappings.remove(&root, Some(Provider::Codex)));
        assert!(mappings.remove(&root, Some(Provider::Claude)));
        mappings.set(Provider::Claude, &child, &identity("other@x.com"));
        assert!(mappings.remove(&child, None));
        assert!(mappings.is_empty());
    }

    #[test]
    fn legacy_mapping_survives_a_second_provider_and_save() {
        let (dir, store) = temp_store();
        let work = MappingStore::normalize_path(&dir.path().join("work"));
        write_json_private(&store.paths.mappings_file(), &serde_json::json!({
            "schemaVersion": 1,
            "mappings": {work.to_string_lossy(): {"email": "legacy@x.com", "accountId": "acct", "added": "2026-01-01T00:00:00Z"}}
        })).unwrap();
        let mut mappings = MappingStore::load(&store.paths);
        mappings.set(Provider::Claude, &work, &identity("new@x.com"));
        mappings.save().unwrap();
        let reloaded = MappingStore::load(&store.paths);
        assert_eq!(
            reloaded.get(Provider::Codex, &work),
            Some(identity("legacy@x.com"))
        );
        assert_eq!(
            reloaded.get(Provider::Claude, &work),
            Some(identity("new@x.com"))
        );
        let raw = read_json(&store.paths.mappings_file()).unwrap().unwrap();
        assert_eq!(
            raw["mappings"][work.to_string_lossy().as_ref()][0]["added"],
            "2026-01-01T00:00:00Z"
        );
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
        assert_eq!(
            mappings.set(Provider::Codex, &work, &identity("a@x.com")),
            None
        );
        assert_eq!(
            mappings.set(Provider::Codex, &work.join("."), &identity("b@x.com")),
            Some(identity("a@x.com"))
        );
        assert_eq!(
            mappings.get(Provider::Codex, &work),
            Some(identity("b@x.com"))
        );
        assert_eq!(mappings.get(Provider::Codex, &work.join("other")), None);
        assert!(mappings.remove(&work, None));
        assert!(!mappings.remove(&work, None));
    }

    #[test]
    fn save_and_load_use_the_documented_json() {
        let (dir, store) = temp_store();
        let mut mappings = MappingStore::load(&store.paths);
        let work = dir.path().join("work");
        mappings.set(Provider::Codex, &work, &identity("a@x.com"));
        mappings.save().unwrap();
        let raw = read_json(&store.paths.mappings_file()).unwrap().unwrap();
        assert_eq!(raw["schemaVersion"], 2);
        let key = MappingStore::normalize_path(&work)
            .to_string_lossy()
            .into_owned();
        let entry = &raw["mappings"][&key][0];
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
        mappings.set(Provider::Codex, &root, &identity("root@x.com"));
        mappings.set(Provider::Codex, &root.join("sub"), &identity("sub@x.com"));
        let norm = |p: &Path| MappingStore::normalize_path(p);
        assert_eq!(
            mappings.resolve(Provider::Codex, &root.join("sub/deeper")),
            Some((norm(&root.join("sub")), identity("sub@x.com")))
        );
        assert_eq!(
            mappings.resolve(Provider::Codex, &root.join("other")),
            Some((norm(&root), identity("root@x.com")))
        );
        assert_eq!(
            mappings.resolve(Provider::Codex, &root),
            Some((norm(&root), identity("root@x.com")))
        );
        assert_eq!(
            mappings.resolve(Provider::Codex, &dir.path().join("projx")),
            None,
            "string prefix is not enough"
        );
        assert_eq!(mappings.resolve(Provider::Codex, dir.path()), None);
    }

    #[test]
    fn prune_and_sorted_entries() {
        let (dir, store) = temp_store();
        let mut mappings = MappingStore::load(&store.paths);
        mappings.set(Provider::Codex, &dir.path().join("b"), &identity("a@x.com"));
        mappings.set(Provider::Codex, &dir.path().join("a"), &identity("a@x.com"));
        mappings.set(Provider::Codex, &dir.path().join("c"), &identity("c@x.com"));
        let keys: Vec<PathBuf> = mappings.entries().into_iter().map(|(p, _, _)| p).collect();
        assert_eq!(
            keys,
            ["a", "b", "c"]
                .iter()
                .map(|n| MappingStore::normalize_path(&dir.path().join(n)))
                .collect::<Vec<_>>()
        );
        assert_eq!(mappings.prune(Provider::Codex, &identity("a@x.com")), 2);
        assert_eq!(mappings.prune(Provider::Codex, &identity("a@x.com")), 0);
        assert_eq!(mappings.entries().len(), 1);
    }
}
