//! S2 same-source artifact: `ReflectionOutput` schema.
//!
//! Ported verbatim from the upstream reflect crate's `output.rs` with the
//! memory-crate imports retargeted to `openpaw_memory`; wire-compatible
//! with the upstream curator so persisted knowledge candidates stay
//! interchangeable (same-source registry item S2).
//!
//! Deviation: the upstream `TaskCandidate::to_task` conversion method was
//! stripped (the board/task types it targets are not ported); the schema
//! itself — every struct and every field — is untouched.

use serde::{Deserialize, Serialize};

use openpaw_memory::{Confidence, MemoryType, Scope};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReflectionOutput {
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub skill_candidates: Vec<SkillCandidate>,
    #[serde(default)]
    pub memory_candidates: Vec<MemoryCandidate>,
    #[serde(default)]
    pub conflicts: Vec<ConflictCandidate>,
    /// Task candidates proposed by the reflection LLM.
    #[serde(default)]
    pub task_candidates: Vec<TaskCandidate>,
    /// COT: structured reflection process (observe → hypothesize → validate → conclude).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reflection_process: Option<String>,
}

impl ReflectionOutput {
    pub fn is_empty(&self) -> bool {
        self.skill_candidates.is_empty()
            && self.memory_candidates.is_empty()
            && self.conflicts.is_empty()
            && self.task_candidates.is_empty()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillCandidate {
    /// kebab-case identifier; also the skill directory name.
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub triggers: Vec<String>,
    /// Markdown body for the SKILL.md (without frontmatter).
    pub body: String,
    pub rationale: String,
    pub confidence: Confidence,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryCandidate {
    pub fact: String,
    #[serde(default)]
    pub tags: Vec<String>,
    pub scope: Scope,
    pub confidence: Confidence,
    pub rationale: String,
    #[serde(default)]
    pub supersedes: Vec<String>,
    #[serde(default = "default_zone")]
    pub zone: String,
    /// COT: memory type classification. Use "reasoning" for reusable thinking paths.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_type: Option<MemoryType>,
    /// COT: abstract thinking steps separated by →. Only for reasoning type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_path: Option<String>,
    /// COT: how this path was applied in this conversation. Only for reasoning type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub concrete_instance: Option<String>,
    /// COT: category of problem this thinking path applies to. Only for reasoning type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub problem_type: Option<String>,
}

fn default_zone() -> String {
    "general".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConflictCandidate {
    /// Existing memory id this candidate is in tension with.
    pub with: String,
    /// Free-form: typically `contradiction` / `redundancy` / `scope_overlap`.
    pub kind: String,
    pub explain: String,
    /// Recommended resolution choices. Kept as free-form strings so the LLM
    /// can suggest novel options; the UI displays them verbatim.
    #[serde(default)]
    pub options: Vec<String>,
}

/// A task proposed by the reflection system. Kept because the struct is
/// defined in the same upstream file; board conversion was stripped.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskCandidate {
    pub title: String,
    #[serde(default)]
    pub description: String,
    /// "low" | "normal" | "high" | "critical"
    #[serde(default = "default_priority")]
    pub priority: String,
    #[serde(default)]
    pub labels: Vec<String>,
    /// Why this task was proposed.
    #[serde(default)]
    pub reason: String,
}

fn default_priority() -> String {
    "normal".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_candidate_deserialize() {
        let json = r#"{
            "title": "Fix login bug",
            "description": "Login fails on Safari",
            "priority": "high",
            "labels": ["bug", "auth"],
            "reason": "User reported login failure"
        }"#;
        let candidate: TaskCandidate = serde_json::from_str(json).unwrap();
        assert_eq!(candidate.title, "Fix login bug");
        assert_eq!(candidate.priority, "high");
        assert_eq!(candidate.labels, vec!["bug", "auth"]);
    }

    #[test]
    fn task_candidate_default_priority() {
        let json = r#"{"title": "Task"}"#;
        let candidate: TaskCandidate = serde_json::from_str(json).unwrap();
        assert_eq!(candidate.priority, "normal");
        assert!(candidate.labels.is_empty());
    }

    #[test]
    fn reflection_output_with_task_candidates() {
        let json = r#"{
            "summary": "test",
            "task_candidates": [
                {"title": "Task 1", "priority": "high"},
                {"title": "Task 2"}
            ]
        }"#;
        let output: ReflectionOutput = serde_json::from_str(json).unwrap();
        assert_eq!(output.task_candidates.len(), 2);
        assert_eq!(output.task_candidates[0].title, "Task 1");
        assert!(!output.is_empty());
    }

    #[test]
    fn reflection_output_empty_with_no_task() {
        let output = ReflectionOutput::default();
        assert!(output.is_empty());
        assert!(output.task_candidates.is_empty());
    }

    #[test]
    fn reflection_output_not_empty_with_task_only() {
        let output = ReflectionOutput {
            task_candidates: vec![TaskCandidate {
                title: "Test".into(),
                description: String::new(),
                priority: "normal".into(),
                labels: vec![],
                reason: String::new(),
            }],
            ..Default::default()
        };
        assert!(!output.is_empty());
    }

    #[test]
    fn test_reasoning_candidate_serde() {
        let json = r#"{
            "fact": "Debug by reproducing first",
            "tags": ["debugging"],
            "scope": "user",
            "confidence": "medium",
            "rationale": "Reusable debugging approach",
            "supersedes": [],
            "zone": "work",
            "memory_type": "reasoning",
            "thinking_path": "reproduce → isolate → hypothesize → test → conclude",
            "concrete_instance": "Reproduced the error by re-running the command",
            "problem_type": "debugging"
        }"#;
        let c: MemoryCandidate = serde_json::from_str(json).unwrap();
        assert_eq!(c.memory_type, Some(MemoryType::Reasoning));
        assert!(c.thinking_path.is_some());
        assert!(c.concrete_instance.is_some());
        assert!(c.problem_type.is_some());
    }

    #[test]
    fn test_candidate_without_optional_fields() {
        let json = r#"{
            "fact": "User prefers dark mode",
            "tags": [],
            "scope": "user",
            "confidence": "high",
            "rationale": "Explicitly stated",
            "supersedes": [],
            "zone": "personal"
        }"#;
        let c: MemoryCandidate = serde_json::from_str(json).unwrap();
        assert!(c.memory_type.is_none());
        assert!(c.thinking_path.is_none());
        assert!(c.concrete_instance.is_none());
    }

    #[test]
    fn test_reflection_process_field() {
        let json = r#"{
            "summary": "test",
            "reflection_process": "observed X → hypothesized Y → validated Z → concluded W"
        }"#;
        let output: ReflectionOutput = serde_json::from_str(json).unwrap();
        assert!(output.reflection_process.is_some());
        assert!(output
            .reflection_process
            .as_ref()
            .unwrap()
            .contains("observed"));
    }
}
