//! Rule trigger for the lite reflection loop.
//!
//! The upstream micro-reflection heuristic stack reduced to its one hard
//! rule: fire when `turns_since_last >= N` (default 3), or immediately when
//! the last turn contained an error.
//!
//! Generation-gap red line: NO phase signals and NO multi-signal weighting.
//! The upstream heuristics (intent keywords, tool-call counts, write/edit
//! detection, output-volume thresholds) are deliberately not ported — this
//! module stays a single fixed threshold by design.

use serde::{Deserialize, Serialize};

/// Trigger rule: the N threshold plus the error switch. `config.rs` embeds
/// this struct instead of duplicating its fields.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TriggerConfig {
    /// Fire once this many turns have passed since the last reflection.
    #[serde(default = "default_max_turns_since_last")]
    pub max_turns_since_last: u32,
    /// Also fire immediately when the last turn contained an error.
    #[serde(default = "default_reflect_on_error")]
    pub reflect_on_error: bool,
}

fn default_max_turns_since_last() -> u32 {
    3
}

fn default_reflect_on_error() -> bool {
    true
}

impl Default for TriggerConfig {
    fn default() -> Self {
        Self {
            max_turns_since_last: default_max_turns_since_last(),
            reflect_on_error: default_reflect_on_error(),
        }
    }
}

/// Per-turn facts the trigger decision needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TriggerContext {
    pub turns_since_last: u32,
    pub error_in_last_turn: bool,
}

impl TriggerConfig {
    /// Should the lite reflection loop run for this turn?
    pub fn should_trigger(&self, ctx: &TriggerContext) -> bool {
        if ctx.turns_since_last >= self.max_turns_since_last {
            return true;
        }
        self.reflect_on_error && ctx.error_in_last_turn
    }
}
