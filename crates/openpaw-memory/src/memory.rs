//! Memory data types.
//!
//! A "memory" is a single durable fact / lesson / convention the agent has
//! decided is worth keeping across sessions. One memory = one short
//! statement. Aggregation, summarisation, and conflict resolution happen at
//! the curation layer (later), not by stuffing many points into one file.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Where a memory lives on disk. Mirrors the skills-side `Scope` enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    /// `~/.openpaw/memories/`
    User,
    /// `./.openpaw/memories/`
    Project,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    /// Distilled by the reflection pipeline.
    Reflection,
    /// Written or edited by a human.
    User,
    /// Imported from another project / agent.
    Imported,
    /// Written by the agent itself (LLM tool call). Agent-written
    /// memories must never masquerade as `User` (human-authored).
    Agent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Confidence {
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum MemoryType {
    #[default]
    Fact,
    Procedure,
    Preference,
    Episode,
    Context,
    Insight,
    /// Reusable thinking path — structured reasoning approach for a class of problems.
    Reasoning,
    /// Autonomous goal generation: a structured goal with success criteria,
    /// priority, and budget. Stored in zone "goals". Decay half-life: 180 days.
    Goal,
    /// Autonomous goal generation: a short-lived intention derived from a goal.
    /// Decay half-life: 7 days.
    Intention,
}

impl MemoryType {
    /// Decay half-life in days for this memory type.
    pub fn decay_half_life_days(&self) -> f64 {
        match self {
            MemoryType::Fact => 90.0,
            MemoryType::Procedure => 60.0,
            MemoryType::Preference => 120.0,
            MemoryType::Episode => 30.0,
            MemoryType::Context => 7.0,
            MemoryType::Insight => 45.0,
            MemoryType::Reasoning => 60.0,
            MemoryType::Goal => 180.0,
            MemoryType::Intention => 7.0,
        }
    }

    /// Parse a memory type from a snake_case string, returning `None` if
    /// the input doesn't match any variant.
    pub fn from_str_lossy(s: &str) -> Option<Self> {
        match s {
            "fact" => Some(MemoryType::Fact),
            "procedure" => Some(MemoryType::Procedure),
            "preference" => Some(MemoryType::Preference),
            "episode" => Some(MemoryType::Episode),
            "context" => Some(MemoryType::Context),
            "insight" => Some(MemoryType::Insight),
            "reasoning" => Some(MemoryType::Reasoning),
            "goal" => Some(MemoryType::Goal),
            "intention" => Some(MemoryType::Intention),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryFrontmatter {
    /// Stable identifier. Use [`MemoryFrontmatter::new`] to allocate.
    pub id: String,

    pub created: chrono::DateTime<chrono::Utc>,

    pub source: Source,

    pub confidence: Confidence,

    /// `true` ⇒ load on every session (analogous to Claude Code's `core.md`).
    /// `false` ⇒ episodic; included by relevance only.
    #[serde(default)]
    pub pinned: bool,

    #[serde(default)]
    pub tags: Vec<String>,

    /// IDs of older memories this one replaces. Listed by [`list_active`]
    /// causes those ids to be filtered out.
    ///
    /// [`list_active`]: crate::store::MemoryStore::list_active
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub supersedes: Vec<String>,

    #[serde(default = "default_zone")]
    pub zone: String,

    /// Short description / summary of this memory.
    #[serde(default)]
    pub description: String,

    /// Memory type classification.
    #[serde(default)]
    pub memory_type: MemoryType,

    /// SM-2 spaced repetition: number of successful repetitions.
    #[serde(default)]
    pub sm2_repetition: u32,

    /// SM-2 spaced repetition: ease factor (minimum 1.3).
    #[serde(default = "default_sm2_ease_factor")]
    pub sm2_ease_factor: f64,

    /// SM-2 spaced repetition: current interval in days.
    #[serde(default)]
    pub sm2_interval_days: u32,

    /// SM-2 spaced repetition: next review date (RFC3339 / YYYY-MM-DD).
    #[serde(default)]
    pub sm2_next_review: Option<String>,

    /// Phase 7: 关联的看板任务 ID。允许按任务检索历史记忆。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    /// runloop-goal-system: 关联的目标 ID。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal_id: Option<String>,

    #[serde(flatten)]
    pub extra: serde_yml::Mapping,
}

fn default_zone() -> String {
    "general".to_string()
}

fn default_sm2_ease_factor() -> f64 {
    2.5
}

impl MemoryFrontmatter {
    pub fn new(source: Source, confidence: Confidence, tags: Vec<String>) -> Self {
        Self {
            id: format!("mem_{}", uuid::Uuid::new_v4().simple()),
            created: chrono::Utc::now(),
            source,
            confidence,
            pinned: false,
            tags,
            supersedes: Vec::new(),
            zone: default_zone(),
            description: String::new(),
            memory_type: MemoryType::default(),
            sm2_repetition: 0,
            sm2_ease_factor: default_sm2_ease_factor(),
            sm2_interval_days: 0,
            sm2_next_review: None,
            task_id: None,
            goal_id: None,
            extra: serde_yml::Mapping::new(),
        }
    }

    pub fn with_zone(mut self, zone: impl Into<String>) -> Self {
        self.zone = zone.into();
        self
    }

    /// Derive a short single-line description from a body of text.
    ///
    /// Uses the first non-empty line, truncated to 120 characters.  This gives
    /// `search_memories_tfidf` and `inject_memories` something to work with
    /// when the caller doesn't supply an explicit description.
    pub fn derive_description(body: &str) -> String {
        let first_line = body
            .lines()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("")
            .trim();
        if first_line.chars().count() <= 120 {
            first_line.to_string()
        } else {
            let truncated: String = first_line.chars().take(117).collect();
            format!("{truncated}...")
        }
    }
}

#[derive(Debug, Clone)]
pub struct LoadedMemory {
    pub frontmatter: MemoryFrontmatter,
    pub body: String,
    pub source_path: PathBuf,
    pub scope: Scope,
}

impl LoadedMemory {
    pub fn id(&self) -> &str {
        &self.frontmatter.id
    }
}

#[cfg(test)]
mod cot_tests {
    use super::*;

    #[test]
    fn test_reasoning_type_serde() {
        let fm = MemoryFrontmatter::new(Source::Reflection, Confidence::Medium, vec![]);
        let yaml = serde_yml::to_string(&fm).unwrap();
        let parsed: MemoryFrontmatter = serde_yml::from_str(&yaml).unwrap();
        assert_eq!(parsed.memory_type, MemoryType::Fact);
    }

    #[test]
    fn test_reasoning_half_life() {
        let days = MemoryType::Reasoning.decay_half_life_days();
        assert_eq!(days, 60.0);
    }

    #[test]
    fn test_old_memory_without_type_defaults_to_fact() {
        let yaml =
            "id: mem_test\ncreated: 2024-01-01T00:00:00Z\nsource: user\nconfidence: medium\n";
        let fm: MemoryFrontmatter = serde_yml::from_str(yaml).unwrap();
        assert_eq!(fm.memory_type, MemoryType::Fact);
    }

    #[test]
    fn test_reasoning_from_str_lossy() {
        assert_eq!(
            MemoryType::from_str_lossy("reasoning"),
            Some(MemoryType::Reasoning)
        );
        assert_eq!(MemoryType::from_str_lossy("fact"), Some(MemoryType::Fact));
        assert_eq!(MemoryType::from_str_lossy("unknown"), None);
    }

    // === Autonomous goal generation: Goal/Intention memory type tests ===

    #[test]
    fn test_goal_type_from_str_lossy() {
        assert_eq!(MemoryType::from_str_lossy("goal"), Some(MemoryType::Goal));
    }

    #[test]
    fn test_intention_type_from_str_lossy() {
        assert_eq!(
            MemoryType::from_str_lossy("intention"),
            Some(MemoryType::Intention)
        );
    }

    #[test]
    fn test_goal_half_life() {
        assert_eq!(MemoryType::Goal.decay_half_life_days(), 180.0);
    }

    #[test]
    fn test_intention_half_life() {
        assert_eq!(MemoryType::Intention.decay_half_life_days(), 7.0);
    }

    #[test]
    fn test_goal_type_serde_roundtrip() {
        let mut fm = MemoryFrontmatter::new(Source::Reflection, Confidence::Medium, vec![]);
        fm.memory_type = MemoryType::Goal;
        fm.zone = "goals".to_string();
        fm.description = "Learn Rust async patterns".to_string();
        let yaml = serde_yml::to_string(&fm).unwrap();
        let parsed: MemoryFrontmatter = serde_yml::from_str(&yaml).unwrap();
        assert_eq!(parsed.memory_type, MemoryType::Goal);
        assert_eq!(parsed.zone, "goals");
    }

    #[test]
    fn test_intention_type_serde_roundtrip() {
        let mut fm = MemoryFrontmatter::new(Source::Reflection, Confidence::Low, vec![]);
        fm.memory_type = MemoryType::Intention;
        let yaml = serde_yml::to_string(&fm).unwrap();
        let parsed: MemoryFrontmatter = serde_yml::from_str(&yaml).unwrap();
        assert_eq!(parsed.memory_type, MemoryType::Intention);
    }
}
