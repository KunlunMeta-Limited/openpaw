//! Skill manifest — metadata for publishing and discovering skills on a Hub.
//!
//! A `SkillManifest` is the "package.json" of a skill. It contains all the
//! metadata needed to publish, discover, install, and verify a skill on the
//! marketplace, while the actual `SkillFrontmatter` + body live in `SKILL.md`.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillManifest {
    pub name: String,
    pub version: String,
    pub description: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub license: Option<String>,
    #[serde(default)]
    pub homepage: Option<String>,
    #[serde(default)]
    pub repository: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub triggers: Vec<String>,
    #[serde(default)]
    pub categories: Vec<String>,
    #[serde(default)]
    pub compatibility: Compatibility,
    #[serde(default)]
    pub dependencies: Vec<SkillDependency>,
    #[serde(default)]
    pub scripts: HashMap<String, String>,
    #[serde(default)]
    pub checksum: Option<String>,
    #[serde(default)]
    pub size_bytes: Option<u64>,
    #[serde(default)]
    pub installed_version: Option<String>,
    #[serde(default)]
    pub installed_at: Option<String>,
    #[serde(default)]
    pub source_hub: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Compatibility {
    #[serde(default)]
    pub openpaw_min: Option<String>,
    #[serde(default)]
    pub openpaw_max: Option<String>,
    #[serde(default)]
    pub platforms: Vec<String>,
    #[serde(default)]
    pub features: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillDependency {
    pub name: String,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub hub: Option<String>,
}

impl SkillManifest {
    pub fn is_compatible(&self, current_version: &str) -> bool {
        if let Some(ref min) = self.compatibility.openpaw_min {
            if semver_cmp(current_version, min) == Ordering::Less {
                return false;
            }
        }
        if let Some(ref max) = self.compatibility.openpaw_max {
            if semver_cmp(current_version, max) == Ordering::Greater {
                return false;
            }
        }
        true
    }
}

use std::cmp::Ordering;

fn semver_cmp(a: &str, b: &str) -> Ordering {
    let parse_parts = |s: &str| -> Vec<u32> {
        s.trim_start_matches('v')
            .split('.')
            .filter_map(|p| p.parse::<u32>().ok())
            .collect()
    };
    let pa = parse_parts(a);
    let pb = parse_parts(b);
    for i in 0..pa.len().max(pb.len()) {
        let va = pa.get(i).copied().unwrap_or(0);
        let vb = pb.get(i).copied().unwrap_or(0);
        match va.cmp(&vb) {
            Ordering::Equal => continue,
            other => return other,
        }
    }
    Ordering::Equal
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HubSearchResult {
    pub manifest: SkillManifest,
    pub score: f64,
    pub downloads: u64,
    pub rating: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HubPublishRequest {
    pub manifest: SkillManifest,
    pub body: String,
    pub readme: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HubPublishResponse {
    pub name: String,
    pub version: String,
    pub url: String,
    pub checksum: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HubInstallResponse {
    pub name: String,
    pub version: String,
    pub body: String,
    pub manifest: SkillManifest,
    pub checksum: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semver_cmp_basic() {
        assert_eq!(semver_cmp("1.0.0", "1.0.0"), Ordering::Equal);
        assert_eq!(semver_cmp("2.0.0", "1.0.0"), Ordering::Greater);
        assert_eq!(semver_cmp("1.0.0", "2.0.0"), Ordering::Less);
        assert_eq!(semver_cmp("1.2.3", "1.2.4"), Ordering::Less);
        assert_eq!(semver_cmp("1.10.0", "1.9.0"), Ordering::Greater);
    }

    #[test]
    fn semver_cmp_with_v_prefix() {
        assert_eq!(semver_cmp("v1.0.0", "1.0.0"), Ordering::Equal);
    }

    #[test]
    fn compatibility_check() {
        let mut m = SkillManifest {
            name: "test".into(),
            version: "1.0.0".into(),
            description: "test".into(),
            author: String::new(),
            license: None,
            homepage: None,
            repository: None,
            tags: vec![],
            triggers: vec![],
            categories: vec![],
            compatibility: Compatibility::default(),
            dependencies: vec![],
            scripts: HashMap::new(),
            checksum: None,
            size_bytes: None,
            installed_version: None,
            installed_at: None,
            source_hub: None,
        };
        assert!(m.is_compatible("1.0.0"));

        m.compatibility.openpaw_min = Some("0.5.0".into());
        assert!(m.is_compatible("1.0.0"));
        assert!(!m.is_compatible("0.4.0"));

        m.compatibility.openpaw_max = Some("2.0.0".into());
        assert!(m.is_compatible("1.5.0"));
        assert!(!m.is_compatible("2.1.0"));
    }

    #[test]
    fn manifest_roundtrip() {
        let m = SkillManifest {
            name: "rust-refactor".into(),
            version: "1.2.0".into(),
            description: "Refactor Rust code".into(),
            author: "example".into(),
            license: Some("MIT".into()),
            homepage: None,
            repository: Some("https://github.com/example/rust-refactor".into()),
            tags: vec!["rust".into(), "refactor".into()],
            triggers: vec!["rust".into(), "refactor".into()],
            categories: vec!["code-quality".into()],
            compatibility: Compatibility {
                openpaw_min: Some("0.1.0".into()),
                openpaw_max: None,
                platforms: vec![],
                features: vec![],
            },
            dependencies: vec![SkillDependency {
                name: "bash".into(),
                version: None,
                hub: None,
            }],
            scripts: HashMap::new(),
            checksum: Some("sha256:abc123".into()),
            size_bytes: Some(2048),
            installed_version: None,
            installed_at: None,
            source_hub: None,
        };
        let json = serde_json::to_string(&m).unwrap();
        let back: SkillManifest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.name, "rust-refactor");
        assert_eq!(back.dependencies.len(), 1);
    }
}
