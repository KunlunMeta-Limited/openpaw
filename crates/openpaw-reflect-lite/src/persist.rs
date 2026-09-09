//! Direct persistence of reflection candidates.
//!
//! Applies the upstream deferred-staging idea (a queue of candidates that
//! survives the turn) directly to store writes: every candidate the
//! reflection produced is written straight to the filesystem stores and
//! reported back as a (path, id) manifest.
//!
//! Generation-gap red line: NO merging and NO dedup. Every candidate
//! becomes a brand-new artifact even when an identical one already exists
//! (fresh ids are allocated per write); consolidation is a downstream
//! (evolve / curator) concern. Conflict candidates are also not persisted
//! here — there is no curation layer in Lite.

use std::path::PathBuf;

use serde_yml::Value;
use thiserror::Error;

use openpaw_memory::skills::{
    FsSkillStore, Scope as SkillScope, SkillFrontmatter, SkillStore, SkillStoreError,
};
use openpaw_memory::{FsMemoryStore, MemoryFrontmatter, MemoryStore, MemoryStoreError, Source};

use crate::output::{MemoryCandidate, ReflectionOutput, SkillCandidate};

#[derive(Debug, Error)]
pub enum PersistError {
    #[error(transparent)]
    Memory(#[from] MemoryStoreError),
    #[error(transparent)]
    Skill(#[from] SkillStoreError),
}

/// One persisted artifact: its id plus the file it was written to.
#[derive(Debug, Clone)]
pub struct PersistedItem {
    pub id: String,
    pub path: PathBuf,
}

/// Manifest of everything written by one `persist` run.
#[derive(Debug, Clone, Default)]
pub struct PersistReport {
    pub memories: Vec<PersistedItem>,
    pub skills: Vec<PersistedItem>,
}

/// Write every memory / skill candidate to the given stores.
///
/// Memory candidates land in their candidate `scope`; skill candidates land
/// in the user skill domain (the candidate schema carries no scope).
pub fn persist(
    output: &ReflectionOutput,
    mem_store: &FsMemoryStore,
    skill_store: &FsSkillStore,
) -> Result<PersistReport, PersistError> {
    let mut report = PersistReport::default();

    for candidate in &output.memory_candidates {
        let fm = memory_frontmatter(candidate);
        let id = fm.id.clone();
        let path = MemoryStore::put(mem_store, candidate.scope, fm, &candidate.fact)?;
        report.memories.push(PersistedItem { id, path });
    }

    for candidate in &output.skill_candidates {
        let fm = skill_frontmatter(candidate);
        let name = fm.name.clone();
        let path = skill_store.put(SkillScope::User, fm, &candidate.body)?;
        report.skills.push(PersistedItem { id: name, path });
    }

    Ok(report)
}

/// `unverified` channel decision: both `MemoryFrontmatter` and
/// `SkillFrontmatter` expose a `#[serde(flatten)] extra: serde_yml::Mapping`
/// catch-all that survives a store round-trip, so the forced flag travels
/// through `extra` — no schema change and no YAML synthesis layer needed.
const UNVERIFIED_KEY: &str = "unverified";

fn force_unverified(extra: &mut serde_yml::Mapping) {
    extra.insert(Value::String(UNVERIFIED_KEY.into()), Value::Bool(true));
}

fn memory_frontmatter(c: &MemoryCandidate) -> MemoryFrontmatter {
    let mut fm = MemoryFrontmatter::new(Source::Reflection, c.confidence, c.tags.clone());
    fm.zone = c.zone.clone();
    fm.description = MemoryFrontmatter::derive_description(&c.fact);
    fm.memory_type = c.memory_type.unwrap_or_default();
    fm.supersedes = c.supersedes.clone();
    force_unverified(&mut fm.extra);
    // Preserve reasoning-path provenance that has no dedicated frontmatter
    // field (the COT fields of the same-source schema).
    if let Some(v) = &c.thinking_path {
        fm.extra.insert(
            Value::String("thinking_path".into()),
            Value::String(v.clone()),
        );
    }
    if let Some(v) = &c.concrete_instance {
        fm.extra.insert(
            Value::String("concrete_instance".into()),
            Value::String(v.clone()),
        );
    }
    if let Some(v) = &c.problem_type {
        fm.extra.insert(
            Value::String("problem_type".into()),
            Value::String(v.clone()),
        );
    }
    fm
}

fn skill_frontmatter(c: &SkillCandidate) -> SkillFrontmatter {
    let mut fm = SkillFrontmatter {
        name: c.name.clone(),
        description: c.description.clone(),
        triggers: c.triggers.clone(),
        source: Some("reflect-lite".into()),
        ..Default::default()
    };
    force_unverified(&mut fm.extra);
    fm
}
