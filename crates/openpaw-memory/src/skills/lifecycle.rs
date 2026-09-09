use std::collections::HashSet;
use std::path::PathBuf;

use crate::skills::skill::{LoadedSkill, Scope, SkillFrontmatter, SkillStatus};
use crate::skills::store::{FsSkillStore, Result, SkillStore};

pub trait SkillLifecycle: Send + Sync {
    fn archive(&self, scope: Scope, name: &str) -> Result<()>;

    fn merge(
        &self,
        scope: Scope,
        sources: &[String],
        merged_name: &str,
        merged_body: &str,
    ) -> Result<PathBuf>;

    fn find_duplicates(&self, skills: &[LoadedSkill]) -> Vec<DuplicateGroup>;

    fn mark_stale(&self, scope: Scope, name: &str) -> Result<()>;

    fn reactivate(&self, scope: Scope, name: &str) -> Result<()>;

    fn restore(&self, scope: Scope, name: &str) -> Result<()>;

    fn can_transition(&self, from: SkillStatus, to: SkillStatus) -> bool;
}

pub struct DuplicateGroup {
    pub names: Vec<String>,
    pub similarity: f64,
}

impl SkillLifecycle for FsSkillStore {
    fn archive(&self, _scope: Scope, name: &str) -> Result<()> {
        let mut skill = self.get(name)?.ok_or_else(|| {
            crate::skills::store::SkillStoreError::Config(format!("skill '{name}' not found"))
        })?;
        if skill.frontmatter.pinned {
            return Err(crate::skills::store::SkillStoreError::Config(format!(
                "Cannot archive pinned skill '{name}'"
            )));
        }
        skill.frontmatter.status = SkillStatus::Archived;
        // Write back to the skill's actual scope, not the caller-specified
        // scope. This prevents creating a duplicate in the wrong scope when
        // a skill lives in Project but the caller passes Scope::User.
        self.put(skill.scope, skill.frontmatter, &skill.body)?;
        Ok(())
    }

    fn merge(
        &self,
        scope: Scope,
        sources: &[String],
        merged_name: &str,
        merged_body: &str,
    ) -> Result<PathBuf> {
        for src_name in sources {
            if let Some(mut skill) = self.get(src_name)? {
                skill.frontmatter.status = SkillStatus::Merged;
                skill.frontmatter.superseded_by = Some(merged_name.to_string());
                // Write each source back to its own scope
                self.put(skill.scope, skill.frontmatter, &skill.body)?;
            }
        }

        let merged_fm = SkillFrontmatter {
            name: merged_name.to_string(),
            description: format!("Merged from: {}", sources.join(", ")),
            triggers: sources
                .iter()
                .filter_map(|n| self.get(n).ok().flatten())
                .flat_map(|s| s.frontmatter.triggers)
                .collect::<HashSet<String>>()
                .into_iter()
                .collect(),
            version: Some("1.0.0".into()),
            license: None,
            status: SkillStatus::Active,
            superseded_by: None,
            priority: None,
            tags: Vec::new(),
            author: None,
            category: None,
            pinned: false,
            source: Some("merged".into()),
            prerequisites: Vec::new(),
            parent: None,
            difficulty: None,
            concepts: Vec::new(),
            ..Default::default()
        };

        self.put(scope, merged_fm, merged_body)
    }

    fn find_duplicates(&self, skills: &[LoadedSkill]) -> Vec<DuplicateGroup> {
        let active: Vec<&LoadedSkill> = skills
            .iter()
            .filter(|s| {
                matches!(
                    s.frontmatter.status,
                    SkillStatus::Active | SkillStatus::Reactivated
                )
            })
            .collect();

        let mut groups = Vec::new();
        let mut seen = HashSet::new();

        for i in 0..active.len() {
            if seen.contains(active[i].name()) {
                continue;
            }
            let mut group_names = vec![active[i].name().to_string()];
            let triggers_i: HashSet<&str> = active[i]
                .frontmatter
                .triggers
                .iter()
                .map(|s| s.as_str())
                .collect();

            for skill in active.iter().skip(i + 1) {
                if seen.contains(skill.name()) {
                    continue;
                }
                let triggers_j: HashSet<&str> = skill
                    .frontmatter
                    .triggers
                    .iter()
                    .map(|s| s.as_str())
                    .collect();
                let similarity = jaccard_similarity(&triggers_i, &triggers_j);

                if similarity > 0.5 {
                    group_names.push(skill.name().to_string());
                    seen.insert(skill.name());
                }
            }

            if group_names.len() > 1 {
                // Reuse triggers_i (already computed above) instead of recomputing
                let similarity = group_names
                    .iter()
                    .skip(1)
                    .filter_map(|name| active.iter().find(|s| s.name() == name.as_str()))
                    .map(|s| {
                        let other: HashSet<&str> =
                            s.frontmatter.triggers.iter().map(|t| t.as_str()).collect();
                        jaccard_similarity(&triggers_i, &other)
                    })
                    .fold(0.0_f64, |a, b| a.max(b));

                seen.insert(active[i].name());
                groups.push(DuplicateGroup {
                    names: group_names,
                    similarity,
                });
            }
        }

        groups
    }

    fn mark_stale(&self, _scope: Scope, name: &str) -> Result<()> {
        let mut skill = self.get(name)?.ok_or_else(|| {
            crate::skills::store::SkillStoreError::Config(format!("skill '{name}' not found"))
        })?;
        if skill.frontmatter.pinned {
            return Err(crate::skills::store::SkillStoreError::Config(format!(
                "Cannot mark pinned skill '{name}' as stale"
            )));
        }
        if !self.can_transition(skill.frontmatter.status, SkillStatus::Stale) {
            return Err(crate::skills::store::SkillStoreError::Config(format!(
                "Cannot mark skill '{name}' as stale from {:?}",
                skill.frontmatter.status
            )));
        }
        skill.frontmatter.status = SkillStatus::Stale;
        self.put(skill.scope, skill.frontmatter, &skill.body)?;
        Ok(())
    }

    fn reactivate(&self, _scope: Scope, name: &str) -> Result<()> {
        let mut skill = self.get(name)?.ok_or_else(|| {
            crate::skills::store::SkillStoreError::Config(format!("skill '{name}' not found"))
        })?;
        if !self.can_transition(skill.frontmatter.status, SkillStatus::Reactivated) {
            return Err(crate::skills::store::SkillStoreError::Config(format!(
                "Cannot reactivate skill '{name}' from {:?}",
                skill.frontmatter.status
            )));
        }
        skill.frontmatter.status = SkillStatus::Reactivated;
        self.put(skill.scope, skill.frontmatter, &skill.body)?;
        Ok(())
    }

    fn restore(&self, _scope: Scope, name: &str) -> Result<()> {
        let mut skill = self.get(name)?.ok_or_else(|| {
            crate::skills::store::SkillStoreError::Config(format!("skill '{name}' not found"))
        })?;
        if !self.can_transition(skill.frontmatter.status, SkillStatus::Active) {
            return Err(crate::skills::store::SkillStoreError::Config(format!(
                "Cannot restore skill '{name}' from {:?} to Active",
                skill.frontmatter.status
            )));
        }
        skill.frontmatter.status = SkillStatus::Active;
        self.put(skill.scope, skill.frontmatter, &skill.body)?;
        Ok(())
    }

    fn can_transition(&self, from: SkillStatus, to: SkillStatus) -> bool {
        matches!(
            (from, to),
            (SkillStatus::Active, SkillStatus::Stale)
                | (SkillStatus::Stale, SkillStatus::Archived)
                | (SkillStatus::Stale, SkillStatus::Reactivated)
                | (SkillStatus::Reactivated, SkillStatus::Stale)
                | (SkillStatus::Reactivated, SkillStatus::Archived)
                | (SkillStatus::Active, SkillStatus::Archived)
                | (SkillStatus::Archived, SkillStatus::Active)
                | (SkillStatus::Merged, SkillStatus::Archived)
        )
    }
}

fn jaccard_similarity<T: std::hash::Hash + Eq>(a: &HashSet<T>, b: &HashSet<T>) -> f64 {
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    let intersection = a.intersection(b).count();
    let union = a.union(b).count();
    // Explicit zero-check to prevent division by zero / NaN.
    if union > 0 {
        intersection as f64 / union as f64
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jaccard_identical() {
        let a: HashSet<&str> = ["a", "b", "c"].into_iter().collect();
        let b: HashSet<&str> = ["a", "b", "c"].into_iter().collect();
        assert!((jaccard_similarity(&a, &b) - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn jaccard_disjoint() {
        let a: HashSet<&str> = ["a", "b"].into_iter().collect();
        let b: HashSet<&str> = ["c", "d"].into_iter().collect();
        assert!((jaccard_similarity(&a, &b)).abs() < f64::EPSILON);
    }

    #[test]
    fn jaccard_partial() {
        let a: HashSet<&str> = ["a", "b", "c"].into_iter().collect();
        let b: HashSet<&str> = ["b", "c", "d"].into_iter().collect();
        let sim = jaccard_similarity(&a, &b);
        assert!((sim - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn test_can_transition() {
        let store = FsSkillStore::new(tempfile::tempdir().unwrap().path().to_path_buf(), None);
        assert!(store.can_transition(SkillStatus::Active, SkillStatus::Stale));
        assert!(store.can_transition(SkillStatus::Stale, SkillStatus::Archived));
        assert!(store.can_transition(SkillStatus::Stale, SkillStatus::Reactivated));
        assert!(store.can_transition(SkillStatus::Reactivated, SkillStatus::Stale));
        assert!(store.can_transition(SkillStatus::Reactivated, SkillStatus::Archived));
        assert!(store.can_transition(SkillStatus::Active, SkillStatus::Archived));
        assert!(store.can_transition(SkillStatus::Archived, SkillStatus::Active));
        assert!(store.can_transition(SkillStatus::Merged, SkillStatus::Archived));
        // Merged skills cannot be restored to Active (they're superseded)
        assert!(!store.can_transition(SkillStatus::Merged, SkillStatus::Active));
        // Archived skills cannot go directly to Stale
        assert!(!store.can_transition(SkillStatus::Archived, SkillStatus::Stale));
    }

    #[test]
    fn test_restore_archived_skill() {
        let dir = tempfile::tempdir().unwrap();
        let store = FsSkillStore::new(dir.path().to_path_buf(), None);

        let fm = SkillFrontmatter {
            name: "restore-me".to_string(),
            description: "test".to_string(),
            triggers: vec!["test".into()],
            version: None,
            license: None,
            status: SkillStatus::Active,
            superseded_by: None,
            priority: None,
            tags: Vec::new(),
            author: None,
            category: None,
            pinned: false,
            source: None,
            prerequisites: Vec::new(),
            parent: None,
            difficulty: None,
            concepts: Vec::new(),
            ..Default::default()
        };
        store.put(Scope::User, fm, "body").unwrap();

        // Archive then restore
        store.archive(Scope::User, "restore-me").unwrap();
        let skill = store.get("restore-me").unwrap().unwrap();
        assert_eq!(skill.frontmatter.status, SkillStatus::Archived);

        store.restore(Scope::User, "restore-me").unwrap();
        let skill = store.get("restore-me").unwrap().unwrap();
        assert_eq!(skill.frontmatter.status, SkillStatus::Active);
    }

    #[test]
    fn test_archive_pinned_skill_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let store = FsSkillStore::new(dir.path().to_path_buf(), None);

        let fm = SkillFrontmatter {
            name: "pinned-skill".to_string(),
            description: "test".to_string(),
            triggers: vec!["test".into()],
            version: None,
            license: None,
            status: SkillStatus::Active,
            superseded_by: None,
            priority: None,
            tags: Vec::new(),
            author: None,
            category: None,
            pinned: true,
            source: None,
            prerequisites: Vec::new(),
            parent: None,
            difficulty: None,
            concepts: Vec::new(),
            ..Default::default()
        };
        store.put(Scope::User, fm, "body").unwrap();

        let result = store.archive(Scope::User, "pinned-skill");
        assert!(result.is_err());
        let skill = store.get("pinned-skill").unwrap().unwrap();
        assert_eq!(skill.frontmatter.status, SkillStatus::Active);
    }

    #[test]
    fn test_mark_stale_pinned_skill_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let store = FsSkillStore::new(dir.path().to_path_buf(), None);

        let fm = SkillFrontmatter {
            name: "pinned-stale".to_string(),
            description: "test".to_string(),
            triggers: vec!["test".into()],
            version: None,
            license: None,
            status: SkillStatus::Active,
            superseded_by: None,
            priority: None,
            tags: Vec::new(),
            author: None,
            category: None,
            pinned: true,
            source: None,
            prerequisites: Vec::new(),
            parent: None,
            difficulty: None,
            concepts: Vec::new(),
            ..Default::default()
        };
        store.put(Scope::User, fm, "body").unwrap();

        let result = store.mark_stale(Scope::User, "pinned-stale");
        assert!(result.is_err());
        let skill = store.get("pinned-stale").unwrap().unwrap();
        assert_eq!(skill.frontmatter.status, SkillStatus::Active);
    }
}
