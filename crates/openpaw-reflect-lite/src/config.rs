//! Lite loop configuration: the trigger N/switches plus the persist switch.

use serde::{Deserialize, Serialize};

use crate::trigger::TriggerConfig;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReflectLiteConfig {
    /// Trigger rule (N threshold + error switch).
    #[serde(default)]
    pub trigger: TriggerConfig,
    /// Persist switch: when `false`, reflection results are returned to the
    /// caller but nothing is written to disk (dry-run).
    #[serde(default = "default_persist_enabled")]
    pub persist_enabled: bool,
}

fn default_persist_enabled() -> bool {
    true
}

impl Default for ReflectLiteConfig {
    fn default() -> Self {
        Self {
            trigger: TriggerConfig::default(),
            persist_enabled: default_persist_enabled(),
        }
    }
}
