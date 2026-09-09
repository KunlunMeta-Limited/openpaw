//! openpaw-memory — memory + skills.
//!
//! Landed (M3): nine MemoryType + dual-domain fs store, relevance scoring
//! (TF-IDF + CJK bigram + effectiveness re-rank), SM-2 spaced repetition,
//! context injection with drift defense, memory palace zones, entrypoint
//! index, export/import, and effectiveness stats. Skills side: SKILL.md
//! types, fs store, five-state lifecycle, manifest/permissions, provenance
//! guard, and trimmed usage telemetry.

pub mod export;
pub mod index;
pub mod injection;
pub mod manage;
pub mod memory;
pub mod memory_tool;
pub mod palace;
pub mod relevance;
pub mod skills;
pub mod sm2;
pub mod stats;
pub mod store;

pub use export::{export_memories, import_memories, ImportResult, MemoryEntry, MemoryExport};
pub use index::{
    build_memory_index, truncate_entrypoint, ENTRYPOINT_NAME, MAX_ENTRYPOINT_BYTES,
    MAX_ENTRYPOINT_LINES,
};
pub use injection::{
    format_reasoning_memories, inject_memories, inject_memories_with_drift_defense,
    InjectionConfig, InjectionFormat, DRIFT_DEFENSE_SECTION, IGNORE_MEMORY_INSTRUCTION,
};
pub use manage::{run as manage_run, spec as manage_spec};
pub use memory::{Confidence, LoadedMemory, MemoryFrontmatter, MemoryType, Scope, Source};
pub use memory_tool::run as memory_run;
pub use palace::{
    build_palace_index_simple, get_zone, group_by_zone, load_palace_index, load_zone_summary,
    save_palace_index, save_zone_summary, ALL_ZONES, ZONE_CORE, ZONE_EPISODE, ZONE_GENERAL,
    ZONE_GOALS, ZONE_WORK,
};
pub use relevance::{
    score_with_problem_type, search_memories, search_memories_filtered, search_memories_scored,
    search_memories_tfidf, search_memories_with_effectiveness, search_memories_with_problem_type,
    tfidf_similarity,
};
pub use sm2::{event_to_quality, sm2_calculate, Sm2Result};
pub use stats::{
    clear_stats, decay_factor_typed, load_effectiveness, record_event,
    update_effectiveness_from_injections, MemoryEffectiveness, MemoryEvent,
};
pub use store::{
    standard_project_root, standard_user_root, FsMemoryStore, MemoryStore, MemoryStoreError,
};
