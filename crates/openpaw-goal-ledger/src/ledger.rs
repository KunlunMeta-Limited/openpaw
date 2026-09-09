//! Event ledger — append-only JSONL persistence.
//!
//! Same-source port of the upstream goal event ledger: one file per goal,
//! one `fsync` per write, and goal-id validation that blocks path injection
//! before any path is constructed.

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::{GoalError, Result};

/// Goal event — a single record in the append-only event ledger.
///
/// Uses `serde_json::Value` to carry the typed payload; the `type` field
/// discriminates event kinds.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoalEvent {
    /// Event kind identifier (e.g. "created", "status_changed", "run_summary").
    #[serde(rename = "type")]
    pub event_type: String,

    /// Time the event happened.
    pub timestamp: DateTime<Utc>,

    /// Goal id.
    pub goal_id: String,

    /// Event payload (typed JSON).
    pub payload: serde_json::Value,
}

impl GoalEvent {
    /// Create a new event.
    pub fn new(
        event_type: impl Into<String>,
        goal_id: impl Into<String>,
        payload: serde_json::Value,
    ) -> Self {
        Self {
            event_type: event_type.into(),
            timestamp: Utc::now(),
            goal_id: goal_id.into(),
            payload,
        }
    }
}

/// Goal event ledger — append-only JSONL file.
///
/// Each goal id maps to one `<goals_dir>/.events/<goal_id>.jsonl` file.
/// Every append is followed by `sync_data()`, matching the session writer
/// durability contract.
pub struct GoalEventLedger {
    events_dir: PathBuf,
}

impl GoalEventLedger {
    /// Construct the ledger from a goals directory.
    pub fn new(goals_dir: &Path) -> Self {
        Self {
            events_dir: goals_dir.join(".events"),
        }
    }

    /// Goal id format validation — must match `goal_[0-9a-f]+`.
    ///
    /// Goal ids are system-generated (`goal_` prefix + hex). Any "id"
    /// containing `/`, `\`, `..`, or other characters cannot be a legitimate
    /// goal; it is rejected before file path construction, blocking
    /// `../../etc/x` and absolute-path injection (path-injection hardening:
    /// validate before any filesystem side effect).
    pub(crate) fn validate_goal_id(goal_id: &str) -> Result<()> {
        let hex = goal_id
            .strip_prefix("goal_")
            .ok_or_else(|| GoalError::InvalidId(goal_id.to_string()))?;
        if hex.is_empty() || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(GoalError::InvalidId(goal_id.to_string()));
        }
        Ok(())
    }

    /// Event file path for a goal id.
    fn path_for(&self, goal_id: &str) -> PathBuf {
        self.events_dir.join(format!("{goal_id}.jsonl"))
    }

    /// Append an event (append-only + sync_data).
    pub fn append(&self, event: &GoalEvent) -> Result<()> {
        // Reject malformed ids before any path construction.
        Self::validate_goal_id(&event.goal_id)?;

        std::fs::create_dir_all(&self.events_dir).map_err(|source| GoalError::Io {
            path: self.events_dir.clone(),
            source,
        })?;

        let path = self.path_for(&event.goal_id);
        let line = serde_json::to_string(event)?;

        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|source| GoalError::Io {
                path: path.clone(),
                source,
            })?;

        writeln!(file, "{line}").map_err(|source| GoalError::Io {
            path: path.clone(),
            source,
        })?;

        // One fsync per write, matching the session writer contract.
        file.sync_data().map_err(|source| GoalError::Io {
            path: path.clone(),
            source,
        })?;

        Ok(())
    }

    /// Query all events of a goal.
    ///
    /// Malformed lines are skipped so a partially corrupted tail cannot
    /// poison the whole replay (logging is intentionally omitted — this
    /// crate carries no tracing dependency).
    pub fn query(&self, goal_id: &str) -> Result<Vec<GoalEvent>> {
        // Reject malformed ids before any path construction.
        Self::validate_goal_id(goal_id)?;
        let path = self.path_for(goal_id);
        if !path.exists() {
            return Ok(Vec::new());
        }

        let file = File::open(&path).map_err(|source| GoalError::Io {
            path: path.clone(),
            source,
        })?;
        let reader = BufReader::new(file);

        let mut events = Vec::new();
        for line in reader.lines() {
            let line = line.map_err(|source| GoalError::Io {
                path: path.clone(),
                source,
            })?;
            if line.trim().is_empty() {
                continue;
            }
            if let Ok(event) = serde_json::from_str::<GoalEvent>(&line) {
                events.push(event);
            }
        }
        Ok(events)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_append_and_query() {
        let tmp = tempfile::tempdir().unwrap();
        let ledger = GoalEventLedger::new(tmp.path());

        let event1 = GoalEvent::new(
            "created",
            "goal_cafe001",
            serde_json::json!({"objective": "test goal"}),
        );
        ledger.append(&event1).unwrap();

        let event2 = GoalEvent::new(
            "status_changed",
            "goal_cafe001",
            serde_json::json!({"from": "active", "to": "paused"}),
        );
        ledger.append(&event2).unwrap();

        let events = ledger.query("goal_cafe001").unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].event_type, "created");
        assert_eq!(events[1].event_type, "status_changed");
    }

    #[test]
    fn query_nonexistent_returns_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let ledger = GoalEventLedger::new(tmp.path());
        let events = ledger.query("goal_deadbeef").unwrap();
        assert!(events.is_empty());
    }

    #[test]
    fn malformed_lines_are_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        let ledger = GoalEventLedger::new(tmp.path());
        ledger
            .append(&GoalEvent::new(
                "created",
                "goal_cafe001",
                serde_json::json!({}),
            ))
            .unwrap();

        let path = tmp.path().join(".events").join("goal_cafe001.jsonl");
        let mut raw = std::fs::read_to_string(&path).unwrap();
        raw.push_str("not-json-at-all\n");
        std::fs::write(&path, raw).unwrap();

        let events = ledger.query("goal_cafe001").unwrap();
        assert_eq!(events.len(), 1, "malformed tail line must be skipped");
    }

    // Goal id format validation: hex ids accepted, traversal and paths rejected.

    #[test]
    fn validate_goal_id_accepts_hex_ids() {
        assert!(GoalEventLedger::validate_goal_id("goal_cafe001").is_ok());
        assert!(GoalEventLedger::validate_goal_id("goal_0123456789abcdef0123456789abcdef").is_ok());
    }

    #[test]
    fn validate_goal_id_rejects_traversal_and_paths() {
        assert!(GoalEventLedger::validate_goal_id("../evil").is_err());
        assert!(GoalEventLedger::validate_goal_id("goal_abc/../def").is_err());
        assert!(GoalEventLedger::validate_goal_id("/absolute").is_err());
        assert!(GoalEventLedger::validate_goal_id("goal_\\evil").is_err());
        assert!(GoalEventLedger::validate_goal_id("C:\\evil").is_err());
    }

    #[test]
    fn validate_goal_id_rejects_bad_format() {
        assert!(GoalEventLedger::validate_goal_id("").is_err());
        assert!(GoalEventLedger::validate_goal_id("goal_").is_err());
        assert!(GoalEventLedger::validate_goal_id("goal_nonexistent").is_err());
        assert!(GoalEventLedger::validate_goal_id("missing_prefix").is_err());
    }

    #[test]
    fn append_rejects_malformed_id() {
        let tmp = tempfile::tempdir().unwrap();
        let ledger = GoalEventLedger::new(tmp.path());
        let event = GoalEvent::new("created", "../evil", serde_json::json!({}));
        assert!(ledger.append(&event).is_err());
        // An injected id must not produce any file.
        assert!(!tmp.path().join(".events").exists());
    }
}
