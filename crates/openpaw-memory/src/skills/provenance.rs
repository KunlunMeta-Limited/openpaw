use std::collections::HashSet;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::skills::telemetry::SkillTelemetry;

/// 技能来源 — 随遥测数据一起持久化（JSON 序列化为 snake_case）。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum SkillProvenance {
    #[default]
    Unknown,
    Bundled,
    Hub,
    Agent,
    User,
}

pub struct ProvenanceGuard {
    bundled_manifest: HashSet<String>,
    hub_lock: HashSet<String>,
}

impl ProvenanceGuard {
    pub fn new(root: &std::path::Path) -> Self {
        let bundled = root.join(".bundled_manifest.json");
        let hub = root.join(".hub_lock.json");

        let bundled_manifest = load_string_set(&bundled);
        let hub_lock = load_string_set(&hub);

        Self {
            bundled_manifest,
            hub_lock,
        }
    }

    pub fn is_curator_managed(&self, name: &str, telemetry: &SkillTelemetry) -> bool {
        if telemetry.pinned {
            return false;
        }
        if self.bundled_manifest.contains(name) {
            return false;
        }
        if self.hub_lock.contains(name) {
            return false;
        }
        telemetry.created_by == SkillProvenance::Agent
    }

    pub fn is_bundled(&self, name: &str) -> bool {
        self.bundled_manifest.contains(name)
    }

    pub fn is_hub_locked(&self, name: &str) -> bool {
        self.hub_lock.contains(name)
    }

    pub fn register_bundled(&mut self, name: &str, root: &std::path::Path) -> std::io::Result<()> {
        self.bundled_manifest.insert(name.to_string());
        save_string_set(&self.bundled_manifest, &root.join(".bundled_manifest.json"))
    }

    pub fn register_hub(&mut self, name: &str, root: &std::path::Path) -> std::io::Result<()> {
        self.hub_lock.insert(name.to_string());
        save_string_set(&self.hub_lock, &root.join(".hub_lock.json"))
    }
}

fn load_string_set(path: &PathBuf) -> HashSet<String> {
    if !path.exists() {
        return HashSet::new();
    }
    std::fs::read_to_string(path)
        .ok()
        .and_then(|content| serde_json::from_str::<HashSet<String>>(&content).ok())
        .unwrap_or_default()
}

fn save_string_set(set: &HashSet<String>, path: &PathBuf) -> std::io::Result<()> {
    let content = serde_json::to_string_pretty(set)?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, &content)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skills::telemetry::SkillTelemetry;

    fn make_guard() -> ProvenanceGuard {
        let dir = tempfile::tempdir().unwrap();
        ProvenanceGuard::new(dir.path())
    }

    #[test]
    fn test_agent_created_is_curator_managed() {
        let guard = make_guard();
        let t = SkillTelemetry {
            created_by: SkillProvenance::Agent,
            ..Default::default()
        };
        assert!(guard.is_curator_managed("my-skill", &t));
    }

    #[test]
    fn test_bundled_not_curator_managed() {
        let mut guard = make_guard();
        let dir = tempfile::tempdir().unwrap();
        guard.register_bundled("bundled-skill", dir.path()).unwrap();

        let t = SkillTelemetry {
            created_by: SkillProvenance::Agent,
            ..Default::default()
        };
        assert!(!guard.is_curator_managed("bundled-skill", &t));
    }

    #[test]
    fn test_pinned_not_curator_managed() {
        let guard = make_guard();
        let t = SkillTelemetry {
            created_by: SkillProvenance::Agent,
            pinned: true,
            ..Default::default()
        };
        assert!(!guard.is_curator_managed("pinned-skill", &t));
    }

    #[test]
    fn test_hub_locked_not_curator_managed() {
        let mut guard = make_guard();
        let dir = tempfile::tempdir().unwrap();
        guard.register_hub("hub-skill", dir.path()).unwrap();

        let t = SkillTelemetry {
            created_by: SkillProvenance::Agent,
            ..Default::default()
        };
        assert!(!guard.is_curator_managed("hub-skill", &t));
    }

    #[test]
    fn test_user_created_not_curator_managed() {
        let guard = make_guard();
        let t = SkillTelemetry {
            created_by: SkillProvenance::User,
            ..Default::default()
        };
        assert!(!guard.is_curator_managed("user-skill", &t));
    }
}
