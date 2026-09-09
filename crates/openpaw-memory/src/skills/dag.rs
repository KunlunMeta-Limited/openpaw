//! Skill DAG (Directed Acyclic Graph) engine.
//!
//! Provides prerequisite tracking, cycle detection, and learning path planning.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};

use crate::skills::LoadedSkill;

/// The Skill DAG.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SkillDag {
    /// Adjacency list: skill_name → set of prerequisite skill names.
    edges: HashMap<String, HashSet<String>>,
}

/// Result of DAG validation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DagValidationResult {
    /// Whether the DAG is valid (no cycles, no dangling references).
    pub is_valid: bool,
    /// Detected cycles (each cycle is a list of skill names forming the cycle).
    pub cycles: Vec<Vec<String>>,
    /// Skills referenced as prerequisites but not found in the DAG.
    pub dangling_references: Vec<String>,
}

/// A node in the learning path.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LearningPathNode {
    pub skill_name: String,
    /// Depth in the topological order (0 = no prerequisites).
    pub depth: usize,
    /// Whether this skill is already mastered.
    pub mastered: bool,
}

/// Result of learning path computation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LearningPath {
    /// Skills in topological order.
    pub steps: Vec<LearningPathNode>,
}

impl SkillDag {
    /// Create a new empty DAG.
    pub fn new() -> Self {
        Self::default()
    }

    /// Build a DAG from loaded skills' prerequisites.
    pub fn from_skills(skills: &[LoadedSkill]) -> Self {
        let mut dag = Self::default();
        for skill in skills {
            if !skill.frontmatter.prerequisites.is_empty() {
                dag.edges.insert(
                    skill.frontmatter.name.clone(),
                    skill.frontmatter.prerequisites.iter().cloned().collect(),
                );
            } else {
                // Ensure node exists even without prerequisites
                dag.edges.entry(skill.frontmatter.name.clone()).or_default();
            }
        }
        dag
    }

    /// Add a skill with its prerequisites.
    pub fn add_skill(&mut self, name: &str, prerequisites: &[String]) {
        self.edges
            .insert(name.to_string(), prerequisites.iter().cloned().collect());
    }

    /// Remove a skill from the DAG.
    pub fn remove_skill(&mut self, name: &str) {
        self.edges.remove(name);
        // Also remove from other skills' prerequisites
        for prereqs in self.edges.values_mut() {
            prereqs.remove(name);
        }
    }

    /// Get prerequisites of a skill.
    pub fn prerequisites(&self, name: &str) -> Vec<String> {
        self.edges
            .get(name)
            .map(|s| s.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Get all skill names in the DAG.
    pub fn skill_names(&self) -> Vec<String> {
        let mut names: HashSet<String> = self.edges.keys().cloned().collect();
        for prereqs in self.edges.values() {
            for p in prereqs {
                names.insert(p.clone());
            }
        }
        names.into_iter().collect()
    }

    /// Number of skills in the DAG.
    pub fn len(&self) -> usize {
        self.edges.len()
    }

    /// Whether the DAG is empty.
    pub fn is_empty(&self) -> bool {
        self.edges.is_empty()
    }

    /// Validate the DAG: check for cycles and dangling references.
    pub fn validate(&self) -> DagValidationResult {
        let all_skills: HashSet<&str> = self.edges.keys().map(|s| s.as_str()).collect();

        // Check for dangling references
        let mut dangling = Vec::new();
        for prereqs in self.edges.values() {
            for prereq in prereqs {
                if !all_skills.contains(prereq.as_str()) {
                    dangling.push(prereq.clone());
                }
            }
        }

        // Cycle detection using DFS
        let cycles = self.detect_cycles();

        DagValidationResult {
            is_valid: cycles.is_empty() && dangling.is_empty(),
            cycles,
            dangling_references: dangling,
        }
    }

    /// Detect cycles using DFS.
    fn detect_cycles(&self) -> Vec<Vec<String>> {
        let mut cycles = Vec::new();
        let mut visited: HashSet<String> = HashSet::new();
        let mut in_stack: HashSet<String> = HashSet::new();
        let mut path: Vec<String> = Vec::new();

        for node in self.edges.keys() {
            if !visited.contains(node) {
                self.dfs_cycle(node, &mut visited, &mut in_stack, &mut path, &mut cycles);
            }
        }

        cycles
    }

    fn dfs_cycle(
        &self,
        node: &str,
        visited: &mut HashSet<String>,
        in_stack: &mut HashSet<String>,
        path: &mut Vec<String>,
        cycles: &mut Vec<Vec<String>>,
    ) {
        visited.insert(node.to_string());
        in_stack.insert(node.to_string());
        path.push(node.to_string());

        if let Some(prereqs) = self.edges.get(node) {
            for prereq in prereqs {
                if in_stack.contains(prereq) {
                    // Found a cycle: extract the cycle from path
                    let start = path.iter().position(|p| p == prereq).unwrap_or(0);
                    let cycle: Vec<String> = path[start..].to_vec();
                    cycles.push(cycle);
                } else if !visited.contains(prereq) {
                    // Only recurse into nodes that exist in the DAG.
                    // Prerequisites not in edges are dangling references,
                    // handled separately by validate().
                    if self.edges.contains_key(prereq) {
                        self.dfs_cycle(prereq, visited, in_stack, path, cycles);
                    } else {
                        // Mark dangling prereq as visited to avoid re-checking
                        visited.insert(prereq.clone());
                    }
                }
            }
        }

        path.pop();
        in_stack.remove(node);
    }

    /// Compute learning path using topological sort.
    /// Returns skills in order of dependency (prerequisites first).
    /// Only skills that exist as nodes in the DAG are included; dangling
    /// prerequisite references are silently ignored.
    pub fn learning_path(&self, mastered: &HashSet<String>) -> LearningPath {
        let all_skills = self.skill_names();
        // Only consider skills that are actual nodes in the DAG (have an
        // entry in edges). Dangling prerequisite references are excluded.
        let real_nodes: HashSet<&str> = self.edges.keys().map(|s| s.as_str()).collect();

        let mut in_degree: HashMap<String, usize> = HashMap::new();
        let mut dependents: HashMap<String, Vec<String>> = HashMap::new();

        for skill in &all_skills {
            // Skip dangling references — they're not real nodes
            if !real_nodes.contains(skill.as_str()) {
                continue;
            }
            in_degree.entry(skill.clone()).or_insert(0);
            dependents.entry(skill.clone()).or_default();

            if let Some(prereqs) = self.edges.get(skill) {
                // Only count unmet prerequisites that are real nodes.
                // Dangling references are treated as already satisfied
                // (they don't block the skill from appearing in the path).
                let unmet: Vec<String> = prereqs
                    .iter()
                    .filter(|p| !mastered.contains(*p))
                    .filter(|p| real_nodes.contains(p.as_str()))
                    .cloned()
                    .collect();
                *in_degree.entry(skill.clone()).or_insert(0) += unmet.len();

                for prereq in &unmet {
                    dependents
                        .entry(prereq.clone())
                        .or_default()
                        .push(skill.clone());
                }
            }
        }

        // BFS topological sort (Kahn's algorithm)
        let mut queue: VecDeque<String> = in_degree
            .iter()
            .filter(|(_, &deg)| deg == 0)
            .map(|(s, _)| s.clone())
            .collect();

        // Mark mastered skills that are real nodes
        for skill in mastered {
            if real_nodes.contains(skill.as_str())
                && in_degree.contains_key(skill)
                && *in_degree.get(skill).unwrap_or(&0) == 0
                && !queue.contains(skill)
            {
                queue.push_back(skill.clone());
            }
        }

        let mut result = Vec::new();
        let mut depth_map: HashMap<String, usize> = HashMap::new();

        while let Some(skill) = queue.pop_front() {
            let depth = depth_map.get(&skill).copied().unwrap_or(0);
            let is_mastered = mastered.contains(&skill);

            result.push(LearningPathNode {
                skill_name: skill.clone(),
                depth,
                mastered: is_mastered,
            });

            if let Some(deps) = dependents.get(&skill) {
                for dep in deps {
                    let deg = in_degree.entry(dep.clone()).or_insert(0);
                    *deg = deg.saturating_sub(1);
                    if *deg == 0 {
                        depth_map.insert(dep.clone(), depth + 1);
                        queue.push_back(dep.clone());
                    }
                }
            }
        }

        LearningPath { steps: result }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_dag_is_valid() {
        let dag = SkillDag::new();
        let result = dag.validate();
        assert!(result.is_valid);
        assert!(result.cycles.is_empty());
    }

    #[test]
    fn simple_dag_no_cycle() {
        let mut dag = SkillDag::new();
        dag.add_skill("rust-async", &["rust-basic".to_string()]);
        dag.add_skill("rust-basic", &[]);
        let result = dag.validate();
        assert!(result.is_valid);
    }

    #[test]
    fn cycle_detected() {
        let mut dag = SkillDag::new();
        dag.add_skill("a", &["b".to_string()]);
        dag.add_skill("b", &["a".to_string()]);
        let result = dag.validate();
        assert!(!result.is_valid);
        assert!(!result.cycles.is_empty());
    }

    #[test]
    fn dangling_reference_detected() {
        let mut dag = SkillDag::new();
        dag.add_skill("a", &["nonexistent".to_string()]);
        let result = dag.validate();
        assert!(!result.is_valid);
        assert!(result
            .dangling_references
            .contains(&"nonexistent".to_string()));
    }

    #[test]
    fn learning_path_order() {
        let mut dag = SkillDag::new();
        dag.add_skill("c", &["b".to_string()]);
        dag.add_skill("b", &["a".to_string()]);
        dag.add_skill("a", &[]);

        let path = dag.learning_path(&HashSet::new());
        let names: Vec<&str> = path.steps.iter().map(|n| n.skill_name.as_str()).collect();
        // 'a' should come before 'b', 'b' before 'c'
        let a_pos = names.iter().position(|&n| n == "a").unwrap();
        let b_pos = names.iter().position(|&n| n == "b").unwrap();
        let c_pos = names.iter().position(|&n| n == "c").unwrap();
        assert!(a_pos < b_pos);
        assert!(b_pos < c_pos);
    }

    #[test]
    fn learning_path_mastered_skills() {
        let mut dag = SkillDag::new();
        dag.add_skill("c", &["b".to_string()]);
        dag.add_skill("b", &["a".to_string()]);
        dag.add_skill("a", &[]);

        let mastered: HashSet<String> = ["a".to_string()].into_iter().collect();
        let path = dag.learning_path(&mastered);
        let a_node = path.steps.iter().find(|n| n.skill_name == "a").unwrap();
        assert!(a_node.mastered);
    }

    #[test]
    fn remove_skill_cleans_references() {
        let mut dag = SkillDag::new();
        dag.add_skill("b", &["a".to_string()]);
        dag.add_skill("a", &[]);
        dag.remove_skill("a");
        let prereqs = dag.prerequisites("b");
        assert!(!prereqs.contains(&"a".to_string()));
    }

    #[test]
    fn three_node_cycle_detected() {
        let mut dag = SkillDag::new();
        dag.add_skill("a", &["c".to_string()]);
        dag.add_skill("b", &["a".to_string()]);
        dag.add_skill("c", &["b".to_string()]);
        let result = dag.validate();
        assert!(!result.is_valid);
        assert!(!result.cycles.is_empty());
    }

    #[test]
    fn from_skills_construction() {
        use crate::skills::{LoadedSkill, Scope, SkillFrontmatter};
        use std::path::PathBuf;

        let fm = SkillFrontmatter {
            name: "rust-async".to_string(),
            description: "Async programming".to_string(),
            triggers: vec!["async".to_string()],
            prerequisites: vec!["rust-basic".to_string()],
            difficulty: Some(0.7),
            ..Default::default()
        };

        let skill = LoadedSkill {
            frontmatter: fm,
            body: "Async stuff".to_string(),
            source: PathBuf::from("/tmp/test.md"),
            scope: Scope::User,
        };

        let dag = SkillDag::from_skills(&[skill]);
        assert_eq!(dag.len(), 1);
        assert!(dag
            .prerequisites("rust-async")
            .contains(&"rust-basic".to_string()));
    }

    #[test]
    fn learning_path_with_dangling_prereq_excludes_it() {
        // Skill "a" has a dangling prereq "nonexistent" that is not a node
        // in the DAG. The learning path should include "a" but not
        // "nonexistent".
        let mut dag = SkillDag::new();
        dag.add_skill("a", &["nonexistent".to_string()]);

        let path = dag.learning_path(&HashSet::new());
        let names: Vec<&str> = path.steps.iter().map(|n| n.skill_name.as_str()).collect();
        assert!(names.contains(&"a"));
        assert!(!names.contains(&"nonexistent"));
    }

    #[test]
    fn detect_cycles_with_dangling_prereq_no_crash() {
        // Should not panic or infinite-loop when a prereq is dangling
        let mut dag = SkillDag::new();
        dag.add_skill("a", &["nonexistent".to_string()]);
        dag.add_skill("b", &["a".to_string()]);
        let result = dag.validate();
        // Has dangling reference but no cycle
        assert!(!result.dangling_references.is_empty());
        assert!(result.cycles.is_empty());
    }
}
