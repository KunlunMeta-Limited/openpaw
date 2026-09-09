use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum PluginKind {
    #[default]
    Standalone,
    Backend,
    Exclusive,
    Platform,
    ModelProvider,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PluginDependencies {
    #[serde(default)]
    pub env: Vec<String>,
    #[serde(default)]
    pub features: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PluginProvides {
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default)]
    pub hooks: Vec<String>,
    #[serde(default)]
    pub commands: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PluginPermissions {
    #[serde(default)]
    pub network: bool,
    #[serde(default)]
    pub filesystem: bool,
    #[serde(default)]
    pub subprocess: bool,

    // Fine-grained permissions
    /// Allowed network domains (empty = all if network=true, none if network=false)
    #[serde(default)]
    pub allowed_domains: Vec<String>,
    /// Allowed filesystem paths (empty = all if filesystem=true, none if filesystem=false)
    #[serde(default)]
    pub allowed_paths: Vec<String>,
    /// Maximum subprocess execution time in seconds (0 = unlimited)
    #[serde(default)]
    pub max_subprocess_duration_secs: u64,
    /// Whether the plugin can read environment variables
    #[serde(default)]
    pub read_env: bool,
    /// Whether the plugin can write to stdout/stderr
    #[serde(default)]
    pub write_stdout: bool,
}

impl PluginPermissions {
    /// Check if a domain is allowed for network access.
    ///
    /// Uses label-boundary matching to prevent suffix confusion:
    /// `"not-evil.com"` must NOT match `"evil.com"`, but `"sub.example.com"`
    /// SHOULD match `"example.com"`.
    pub fn is_domain_allowed(&self, domain: &str) -> bool {
        if !self.network {
            return false;
        }
        if self.allowed_domains.is_empty() {
            return true; // network=true with no domain restrictions = all allowed
        }
        self.allowed_domains.iter().any(|d| {
            // Exact match, or proper subdomain: domain ends with "." + allowed
            domain == d || domain.ends_with(&format!(".{d}"))
        })
    }

    /// Check if a path is allowed for filesystem access.
    ///
    /// Uses component-boundary matching to prevent prefix confusion:
    /// `"/tmp/plugin-secret"` must NOT match `"/tmp/plugin"`, but
    /// `"/tmp/plugin/data.txt"` SHOULD match `"/tmp/plugin"`.
    pub fn is_path_allowed(&self, path: &str) -> bool {
        if !self.filesystem {
            return false;
        }
        if self.allowed_paths.is_empty() {
            return true; // filesystem=true with no path restrictions = all allowed
        }
        // Normalize: strip trailing slashes so "/tmp/plugin" and "/tmp/plugin/"
        // are treated identically.
        let normalized = path.trim_end_matches('/');
        self.allowed_paths.iter().any(|p| {
            let p = p.trim_end_matches('/');
            // Exact match, or path is inside the allowed directory
            normalized == p || normalized.starts_with(&format!("{p}/"))
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PluginManifest {
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub kind: PluginKind,
    #[serde(default)]
    pub dependencies: PluginDependencies,
    #[serde(default)]
    pub provides: PluginProvides,
    #[serde(default)]
    pub permissions: PluginPermissions,
}

impl PluginManifest {
    pub fn parse_toml(content: &str) -> std::result::Result<Self, toml::de::Error> {
        #[derive(Deserialize)]
        struct Root {
            plugin: PluginManifest,
        }
        let root: Root = toml::from_str(content)?;
        Ok(root.plugin)
    }

    pub fn validate(&self) -> std::result::Result<(), String> {
        if self.name.is_empty() {
            return Err("plugin name cannot be empty".into());
        }
        if self.version.is_empty() {
            return Err("plugin version cannot be empty".into());
        }
        for env_var in &self.dependencies.env {
            if env_var.is_empty() {
                return Err("dependency env var name cannot be empty".into());
            }
        }
        for feature in &self.dependencies.features {
            if feature.is_empty() {
                return Err("dependency feature name cannot be empty".into());
            }
        }
        for tool in &self.provides.tools {
            if tool.is_empty() {
                return Err("provided tool name cannot be empty".into());
            }
        }
        for hook in &self.provides.hooks {
            if hook.is_empty() {
                return Err("provided hook name cannot be empty".into());
            }
        }
        for command in &self.provides.commands {
            if command.is_empty() {
                return Err("provided command name cannot be empty".into());
            }
        }
        // Validate fine-grained permission consistency
        if !self.permissions.allowed_domains.is_empty() && !self.permissions.network {
            return Err("allowed_domains requires network permission to be true".into());
        }
        if !self.permissions.allowed_paths.is_empty() && !self.permissions.filesystem {
            return Err("allowed_paths requires filesystem permission to be true".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_plugin_toml() {
        let content = r#"
[plugin]
name = "spotify"
version = "1.0.0"
description = "Spotify playback control"
kind = "backend"

[plugin.dependencies]
env = ["SPOTIFY_CLIENT_ID"]

[plugin.provides]
tools = ["spotify_playback"]
hooks = ["post_tool_call"]

[plugin.permissions]
network = true
"#;
        let manifest = PluginManifest::parse_toml(content).unwrap();
        assert_eq!(manifest.name, "spotify");
        assert_eq!(manifest.version, "1.0.0");
        assert_eq!(manifest.kind, PluginKind::Backend);
        assert_eq!(manifest.dependencies.env, vec!["SPOTIFY_CLIENT_ID"]);
        assert_eq!(manifest.provides.tools, vec!["spotify_playback"]);
        assert!(manifest.permissions.network);
        assert!(!manifest.permissions.filesystem);
    }

    #[test]
    fn test_validate_empty_name() {
        let manifest = PluginManifest {
            name: "".into(),
            version: "1.0.0".into(),
            ..Default::default()
        };
        assert!(manifest.validate().is_err());
    }

    #[test]
    fn test_validate_ok() {
        let manifest = PluginManifest {
            name: "test".into(),
            version: "1.0.0".into(),
            ..Default::default()
        };
        assert!(manifest.validate().is_ok());
    }

    #[test]
    fn test_validate_empty_feature() {
        let manifest = PluginManifest {
            name: "test".into(),
            version: "1.0.0".into(),
            dependencies: PluginDependencies {
                env: vec![],
                features: vec!["".into()],
            },
            ..Default::default()
        };
        assert!(manifest.validate().is_err());
    }

    #[test]
    fn test_validate_features_ok() {
        let manifest = PluginManifest {
            name: "test".into(),
            version: "1.0.0".into(),
            dependencies: PluginDependencies {
                env: vec!["API_KEY".into()],
                features: vec!["sqlite".into(), "browser".into()],
            },
            ..Default::default()
        };
        assert!(manifest.validate().is_ok());
    }

    #[test]
    fn test_validate_empty_tool_name() {
        let manifest = PluginManifest {
            name: "test".into(),
            version: "1.0.0".into(),
            provides: PluginProvides {
                tools: vec!["".into()],
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(manifest.validate().is_err());
    }

    #[test]
    fn test_validate_empty_hook_name() {
        let manifest = PluginManifest {
            name: "test".into(),
            version: "1.0.0".into(),
            provides: PluginProvides {
                hooks: vec!["valid_hook".into(), "".into()],
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(manifest.validate().is_err());
    }

    #[test]
    fn test_validate_empty_command_name() {
        let manifest = PluginManifest {
            name: "test".into(),
            version: "1.0.0".into(),
            provides: PluginProvides {
                commands: vec!["".into()],
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(manifest.validate().is_err());
    }

    #[test]
    fn test_validate_provides_ok() {
        let manifest = PluginManifest {
            name: "test".into(),
            version: "1.0.0".into(),
            provides: PluginProvides {
                tools: vec!["tool_a".into()],
                hooks: vec!["hook_a".into()],
                commands: vec!["cmd_a".into()],
            },
            ..Default::default()
        };
        assert!(manifest.validate().is_ok());
    }

    #[test]
    fn test_fine_grained_network_permissions() {
        let perms = PluginPermissions {
            network: true,
            allowed_domains: vec!["example.com".into(), "api.github.com".into()],
            ..Default::default()
        };
        assert!(perms.is_domain_allowed("api.github.com"));
        assert!(perms.is_domain_allowed("sub.example.com"));
        assert!(!perms.is_domain_allowed("evil.com"));
    }

    #[test]
    fn test_fine_grained_filesystem_permissions() {
        let perms = PluginPermissions {
            filesystem: true,
            allowed_paths: vec!["/tmp/plugin".into(), "/workspace".into()],
            ..Default::default()
        };
        assert!(perms.is_path_allowed("/tmp/plugin/data.txt"));
        assert!(perms.is_path_allowed("/workspace/src/main.rs"));
        assert!(!perms.is_path_allowed("/etc/passwd"));
    }

    #[test]
    fn test_domain_suffix_confusion() {
        // "not-evil.com" must NOT match "evil.com"
        let perms = PluginPermissions {
            network: true,
            allowed_domains: vec!["evil.com".into()],
            ..Default::default()
        };
        assert!(!perms.is_domain_allowed("not-evil.com"));
        assert!(perms.is_domain_allowed("evil.com"));
        assert!(perms.is_domain_allowed("sub.evil.com"));
    }

    #[test]
    fn test_path_prefix_confusion() {
        // "/tmp/plugin-secret" must NOT match "/tmp/plugin"
        let perms = PluginPermissions {
            filesystem: true,
            allowed_paths: vec!["/tmp/plugin".into()],
            ..Default::default()
        };
        assert!(!perms.is_path_allowed("/tmp/plugin-secret"));
        assert!(perms.is_path_allowed("/tmp/plugin"));
        assert!(perms.is_path_allowed("/tmp/plugin/data.txt"));
    }

    #[test]
    fn test_network_disabled_blocks_all() {
        let perms = PluginPermissions {
            network: false,
            allowed_domains: vec!["example.com".into()],
            ..Default::default()
        };
        assert!(!perms.is_domain_allowed("example.com"));
    }

    #[test]
    fn test_validate_allowed_domains_without_network() {
        let manifest = PluginManifest {
            name: "test".into(),
            version: "1.0.0".into(),
            permissions: PluginPermissions {
                network: false,
                allowed_domains: vec!["example.com".into()],
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(manifest.validate().is_err());
    }

    #[test]
    fn test_validate_allowed_paths_without_filesystem() {
        let manifest = PluginManifest {
            name: "test".into(),
            version: "1.0.0".into(),
            permissions: PluginPermissions {
                filesystem: false,
                allowed_paths: vec!["/tmp".into()],
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(manifest.validate().is_err());
    }
}
