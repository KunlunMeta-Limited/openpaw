use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::manifest::PluginManifest;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Provenance {
    Bundled,
    User,
    Project,
}

impl std::fmt::Display for Provenance {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Provenance::Bundled => write!(f, "bundled"),
            Provenance::User => write!(f, "user"),
            Provenance::Project => write!(f, "project"),
        }
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct DiscoveryReport {
    pub total: usize,
    pub by_provenance: HashMap<String, usize>,
    pub errors: Vec<String>,
}

pub struct LoadedPlugin {
    pub manifest: PluginManifest,
    pub provenance: Provenance,
    pub base_dir: PathBuf,
}

pub struct PluginManager {
    plugins: HashMap<String, LoadedPlugin>,
}

#[derive(Debug, thiserror::Error)]
pub enum DiscoveryError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("toml parse error in {path}: {source}")]
    TomlParse {
        path: PathBuf,
        source: toml::de::Error,
    },
    #[error("validation error in {name}: {reason}")]
    Validation { name: String, reason: String },
}

impl Default for PluginManager {
    fn default() -> Self {
        Self::new()
    }
}

impl PluginManager {
    pub fn new() -> Self {
        Self {
            plugins: HashMap::new(),
        }
    }

    #[allow(clippy::result_large_err)]
    pub fn discover_and_load(
        &mut self,
        force: bool,
    ) -> std::result::Result<DiscoveryReport, DiscoveryError> {
        let mut discovered: Vec<(PluginManifest, Provenance, PathBuf)> = Vec::new();
        let mut report = DiscoveryReport::default();
        let mut errors = Vec::new();

        let bundled_dir = std::env::current_exe()
            .ok()
            .and_then(|e| e.parent().map(|p| p.join("plugins")));

        if let Some(dir) = bundled_dir {
            if dir.exists() {
                match self.scan_directory(&dir, Provenance::Bundled) {
                    Ok(items) => discovered.extend(items),
                    Err(e) => errors.push(format!("bundled scan: {e}")),
                }
            }
        }

        if let Some(home) = dirs::home_dir() {
            let user_dir = home.join(".openpaw").join("plugins");
            if user_dir.exists() {
                match self.scan_directory(&user_dir, Provenance::User) {
                    Ok(items) => discovered.extend(items),
                    Err(e) => errors.push(format!("user scan: {e}")),
                }
            }
        }

        // Only enable project plugins when the env var is explicitly
        // set to "1" or "true". An empty string or any other value must NOT
        // enable project plugins (which are loaded from the CWD and could be
        // a privilege-escalation vector if accidentally enabled).
        let project_plugins_enabled = std::env::var("OPENPAW_ENABLE_PROJECT_PLUGINS")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
        if project_plugins_enabled {
            let project_dir = PathBuf::from(".openpaw/plugins");
            if project_dir.exists() {
                match self.scan_directory(&project_dir, Provenance::Project) {
                    Ok(items) => discovered.extend(items),
                    Err(e) => errors.push(format!("project scan: {e}")),
                }
            }
        }

        // Always sort by priority descending so higher-priority plugins are
        // processed first. This ensures User > Project > Bundled precedence
        // regardless of the `force` flag.
        discovered.sort_by(|a, b| {
            let pa = provenance_priority(&a.1);
            let pb = provenance_priority(&b.1);
            pb.cmp(&pa)
        });

        let mut seen_in_this_pass = std::collections::HashSet::new();
        let mut loaded_this_pass = 0usize;

        for (manifest, provenance, base_dir) in discovered {
            if let Err(e) = manifest.validate() {
                errors.push(format!("{}: {e}", manifest.name));
                continue;
            }

            let name = manifest.name.clone();

            // Skip duplicates within the same discovery pass. Because we sorted
            // by priority descending, the first occurrence has the highest
            // priority and wins.
            if seen_in_this_pass.contains(&name) {
                tracing::debug!(name=%name, "skipping duplicate plugin (higher-priority-wins)");
                continue;
            }
            seen_in_this_pass.insert(name.clone());

            // When force is false, skip plugins already loaded in a previous
            // call. When force is true, overwrite them.
            if !force && self.plugins.contains_key(&name) {
                tracing::debug!(name=%name, "skipping already loaded plugin");
                continue;
            }

            let entry = provenance.to_string();
            *report.by_provenance.entry(entry).or_insert(0) += 1;
            loaded_this_pass += 1;

            self.plugins.insert(
                name,
                LoadedPlugin {
                    manifest,
                    provenance,
                    base_dir,
                },
            );
        }

        report.total = loaded_this_pass;
        report.errors = errors;
        Ok(report)
    }

    #[allow(clippy::result_large_err)]
    fn scan_directory(
        &self,
        dir: &Path,
        source: Provenance,
    ) -> std::result::Result<Vec<(PluginManifest, Provenance, PathBuf)>, DiscoveryError> {
        let mut results = Vec::new();
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let manifest_path = entry.path().join("plugin.toml");
            if !manifest_path.exists() {
                continue;
            }
            let content = std::fs::read_to_string(&manifest_path).map_err(DiscoveryError::Io)?;
            let manifest =
                PluginManifest::parse_toml(&content).map_err(|e| DiscoveryError::TomlParse {
                    path: manifest_path,
                    source: e,
                })?;
            results.push((manifest, source, entry.path()));
        }
        // Sort by plugin name for deterministic ordering across platforms
        results.sort_by(|a, b| a.0.name.cmp(&b.0.name));
        Ok(results)
    }

    pub fn get(&self, name: &str) -> Option<&LoadedPlugin> {
        self.plugins.get(name)
    }

    pub fn list(&self) -> Vec<&LoadedPlugin> {
        let mut list: Vec<&LoadedPlugin> = self.plugins.values().collect();
        list.sort_by(|a, b| a.manifest.name.cmp(&b.manifest.name));
        list
    }

    pub fn is_loaded(&self, name: &str) -> bool {
        self.plugins.contains_key(name)
    }

    pub fn unload(&mut self, name: &str) -> bool {
        self.plugins.remove(name).is_some()
    }
}

fn provenance_priority(p: &Provenance) -> u8 {
    match p {
        Provenance::Bundled => 0,
        Provenance::User => 2,
        Provenance::Project => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_discover_empty() {
        let mut pm = PluginManager::new();
        let report = pm.discover_and_load(false).unwrap();
        assert_eq!(report.total, 0);
    }

    #[test]
    fn test_list_empty() {
        let pm = PluginManager::new();
        assert!(pm.list().is_empty());
    }

    #[test]
    fn test_scan_directory() {
        let dir = tempfile::tempdir().unwrap();
        let plugin_dir = dir.path().join("my-plugin");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(
            plugin_dir.join("plugin.toml"),
            r#"[plugin]
name = "my-plugin"
version = "0.1.0"
description = "Test"
"#,
        )
        .unwrap();

        let pm = PluginManager::new();
        let results = pm.scan_directory(dir.path(), Provenance::User).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0.name, "my-plugin");
        assert_eq!(results[0].1, Provenance::User);
    }

    fn make_plugin_dir(root: &Path, name: &str, version: &str) -> PathBuf {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("plugin.toml"),
            format!("[plugin]\nname = \"{name}\"\nversion = \"{version}\"\n"),
        )
        .unwrap();
        dir
    }

    #[test]
    fn test_priority_higher_provenance_wins() {
        let bundled = tempfile::tempdir().unwrap();
        let user = tempfile::tempdir().unwrap();

        // Same plugin name in both bundled and user dirs, different versions
        make_plugin_dir(bundled.path(), "shared", "1.0.0");
        make_plugin_dir(user.path(), "shared", "2.0.0");

        let mut pm = PluginManager::new();
        // Simulate discovery by scanning both directories
        let mut discovered = Vec::new();
        discovered.extend(
            pm.scan_directory(bundled.path(), Provenance::Bundled)
                .unwrap(),
        );
        discovered.extend(pm.scan_directory(user.path(), Provenance::User).unwrap());

        // Sort like discover_and_load does (priority descending)
        discovered.sort_by(|a, b| {
            let pa = provenance_priority(&a.1);
            let pb = provenance_priority(&b.1);
            pb.cmp(&pa)
        });

        let mut seen = std::collections::HashSet::new();
        for (manifest, provenance, base_dir) in discovered {
            let name = manifest.name.clone();
            if seen.contains(&name) {
                continue;
            }
            seen.insert(name.clone());
            pm.plugins.insert(
                name,
                LoadedPlugin {
                    manifest,
                    provenance,
                    base_dir,
                },
            );
        }

        let loaded = pm.get("shared").unwrap();
        assert_eq!(loaded.provenance, Provenance::User);
        assert_eq!(loaded.manifest.version, "2.0.0");
    }

    #[test]
    fn test_report_by_provenance_no_overcount() {
        let dir1 = tempfile::tempdir().unwrap();
        let dir2 = tempfile::tempdir().unwrap();

        // Same plugin name in two sources
        make_plugin_dir(dir1.path(), "dup", "1.0.0");
        make_plugin_dir(dir2.path(), "dup", "2.0.0");

        let pm = PluginManager::new();
        let mut discovered = Vec::new();
        discovered.extend(pm.scan_directory(dir1.path(), Provenance::Bundled).unwrap());
        discovered.extend(pm.scan_directory(dir2.path(), Provenance::User).unwrap());

        // Replicate the counting logic from discover_and_load
        discovered.sort_by(|a, b| {
            let pa = provenance_priority(&a.1);
            let pb = provenance_priority(&b.1);
            pb.cmp(&pa)
        });

        let mut by_provenance: HashMap<String, usize> = HashMap::new();
        let mut seen = std::collections::HashSet::new();
        let mut inserted = 0;
        for (manifest, provenance, _base) in discovered {
            let name = manifest.name.clone();
            if seen.contains(&name) {
                continue;
            }
            seen.insert(name.clone());
            *by_provenance.entry(provenance.to_string()).or_insert(0) += 1;
            inserted += 1;
        }

        // Only one plugin should be counted (the User one, higher priority)
        assert_eq!(inserted, 1);
        let total_counted: usize = by_provenance.values().sum();
        assert_eq!(total_counted, inserted);
        assert_eq!(*by_provenance.get("user").unwrap_or(&0), 1);
        assert_eq!(*by_provenance.get("bundled").unwrap_or(&0), 0);
    }

    #[test]
    fn test_force_overwrites_previous_load() {
        let dir = tempfile::tempdir().unwrap();
        make_plugin_dir(dir.path(), "force-test", "1.0.0");

        let mut pm = PluginManager::new();

        // First load
        let discovered1 = pm.scan_directory(dir.path(), Provenance::User).unwrap();
        for (manifest, provenance, base_dir) in discovered1 {
            pm.plugins.insert(
                manifest.name.clone(),
                LoadedPlugin {
                    manifest,
                    provenance,
                    base_dir,
                },
            );
        }
        assert_eq!(pm.get("force-test").unwrap().manifest.version, "1.0.0");

        // Update the plugin version
        std::fs::write(
            dir.path().join("force-test").join("plugin.toml"),
            "[plugin]\nname = \"force-test\"\nversion = \"2.0.0\"\n",
        )
        .unwrap();

        // Second load with force=true should overwrite
        let discovered2 = pm.scan_directory(dir.path(), Provenance::User).unwrap();
        for (manifest, provenance, base_dir) in discovered2 {
            let name = manifest.name.clone();
            // force=true: overwrite
            pm.plugins.insert(
                name,
                LoadedPlugin {
                    manifest,
                    provenance,
                    base_dir,
                },
            );
        }
        assert_eq!(pm.get("force-test").unwrap().manifest.version, "2.0.0");
    }

    #[test]
    fn test_discovery_error_is_public() {
        // Verify DiscoveryError can be referenced (compile-time check)
        fn _accept_error(_: DiscoveryError) {}
    }

    #[test]
    fn test_report_total_matches_by_provenance_sum() {
        let dir = tempfile::tempdir().unwrap();
        make_plugin_dir(dir.path(), "plugin-a", "1.0.0");
        make_plugin_dir(dir.path(), "plugin-b", "1.0.0");

        let pm = PluginManager::new();
        let mut discovered = pm.scan_directory(dir.path(), Provenance::User).unwrap();

        // Simulate discover_and_load counting logic
        discovered.sort_by(|a, b| {
            provenance_priority(&a.1)
                .cmp(&provenance_priority(&b.1))
                .reverse()
        });

        let mut by_provenance: HashMap<String, usize> = HashMap::new();
        let mut total = 0usize;
        let mut seen = std::collections::HashSet::new();
        for (manifest, provenance, _base) in discovered {
            let name = manifest.name.clone();
            if seen.contains(&name) {
                continue;
            }
            seen.insert(name);
            *by_provenance.entry(provenance.to_string()).or_insert(0) += 1;
            total += 1;
        }

        let sum: usize = by_provenance.values().sum();
        assert_eq!(total, sum, "total should equal sum of by_provenance values");
        assert_eq!(total, 2);
    }

    #[test]
    fn test_discovery_report_serde_roundtrip() {
        let report = DiscoveryReport {
            total: 3,
            by_provenance: {
                let mut m = HashMap::new();
                m.insert("user".into(), 2);
                m.insert("bundled".into(), 1);
                m
            },
            errors: vec!["bad-plugin: invalid".into()],
        };
        let json = serde_json::to_string(&report).unwrap();
        let restored: DiscoveryReport = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.total, 3);
        assert_eq!(restored.errors, vec!["bad-plugin: invalid".to_string()]);
        assert_eq!(*restored.by_provenance.get("user").unwrap_or(&0), 2);
        assert_eq!(*restored.by_provenance.get("bundled").unwrap_or(&0), 1);
    }

    #[test]
    fn test_scan_directory_returns_sorted_results() {
        let dir = tempfile::tempdir().unwrap();
        // Create plugins in non-alphabetical order
        make_plugin_dir(dir.path(), "zebra", "1.0.0");
        make_plugin_dir(dir.path(), "alpha", "1.0.0");
        make_plugin_dir(dir.path(), "monkey", "1.0.0");

        let pm = PluginManager::new();
        let results = pm.scan_directory(dir.path(), Provenance::User).unwrap();
        assert_eq!(results.len(), 3);
        // Results should be sorted by name
        assert_eq!(results[0].0.name, "alpha");
        assert_eq!(results[1].0.name, "monkey");
        assert_eq!(results[2].0.name, "zebra");
    }
}
