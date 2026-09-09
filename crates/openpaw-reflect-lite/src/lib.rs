//! openpaw-reflect-lite — the three-step minimal reflection loop (S2 landed).
//!
//! Landed (M4): rule trigger (turn threshold N or error in turn), single-shot
//! LLM reflection emitting the same `ReflectionOutput` schema, direct
//! persistence with `unverified: true` forced on every artifact, unverified-
//! ratio stats, config. The trigger → reflect → persist steps compose into
//! the loop the CLI assembly drives.
//!
//! Generation-gap red lines (deliberate deviations from the upstream reflect
//! crate, also annotated in the corresponding modules):
//! - No quality gate: the upstream prompt's eight Quality Gate hard rules
//!   are stripped; candidates persist unfiltered (see `reflect`).
//! - No curation/merge: candidates are written verbatim — no merging, no
//!   dedup, no conflict resolution; conflicts are not persisted (see
//!   `persist`).
//! - No phase signals: the trigger is a single fixed threshold — no intent
//!   keywords, no tool-call/output-volume weighting (see `trigger`).
//! - Every persisted artifact carries `unverified: true`; verification is a
//!   downstream (evolve) concern tracked via `stats::scan_unverified`.
//!
//! Layering: depends on core + memory + store only. LLM calls go through an
//! injected `dyn LlmProvider`; it must not depend on engine, cli, or llm.

pub mod config;
pub mod output;
pub mod persist;
pub mod reflect;
pub mod stats;
pub mod trigger;

pub use config::ReflectLiteConfig;
pub use output::{
    ConflictCandidate, MemoryCandidate, ReflectionOutput, SkillCandidate, TaskCandidate,
};
pub use persist::{persist, PersistError, PersistReport, PersistedItem};
pub use reflect::{parse_output, reflect, ReflectContext, ReflectError, REFLECT_MAX_TOKENS};
pub use stats::{scan_unverified, UnverifiedStats};
pub use trigger::{TriggerConfig, TriggerContext};
