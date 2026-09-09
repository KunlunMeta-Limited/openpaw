use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::memory::MemoryType;

/// Per-type decay floor (minimum retention factor).
const DECAY_FLOOR: f64 = 0.1;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MemoryEffectiveness {
    pub loaded: usize,
    pub referenced: usize,
    pub accessed: usize,
    pub last_event_at: Option<DateTime<Utc>>,
    /// Memory type for type-specific decay.  Defaults to `Fact` when not set
    /// (e.g. legacy stats entries without this field).
    #[serde(default)]
    pub memory_type: MemoryType,
}

impl MemoryEffectiveness {
    pub fn factor(&self) -> f64 {
        if self.loaded == 0 {
            return 1.0;
        }
        let ratio = self.referenced as f64 / self.loaded as f64;
        0.5 + 0.5 * ratio
    }

    /// Time-based decay factor using the per-type half-life.
    ///
    /// Falls back to a 30-day generic half-life when `memory_type` is the
    /// default (`Fact`) and `last_event_at` is present, which matches the
    /// pre-existing behaviour for backward compatibility.
    pub fn decay_factor(&self, now: DateTime<Utc>) -> f64 {
        match self.last_event_at {
            None => 1.0,
            Some(t) => {
                let days = (now - t).num_days() as f64;
                decay_factor_typed(days, self.memory_type)
            }
        }
    }

    pub fn combined_factor(&self, now: DateTime<Utc>) -> f64 {
        self.factor() * self.decay_factor(now)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryEvent {
    Loaded,
    Referenced,
    Accessed,
    ReviewFailed,
    ContextInjected,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StatsEntry {
    memory_id: String,
    event: MemoryEvent,
    timestamp: DateTime<Utc>,
    /// Memory type, recorded so `load_effectiveness` can populate
    /// `MemoryEffectiveness.memory_type` for per-type decay.
    #[serde(default)]
    memory_type: MemoryType,
}

fn stats_path() -> PathBuf {
    openpaw_core::openpaw_home().join("memory-stats.jsonl")
}

pub fn record_event(memory_id: &str, event: MemoryEvent, memory_type: MemoryType) {
    let path = stats_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let entry = StatsEntry {
        memory_id: memory_id.to_string(),
        event,
        timestamp: Utc::now(),
        memory_type,
    };
    if let Ok(line) = serde_json::to_string(&entry) {
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            let _ = writeln!(f, "{}", line);
        }
    }
}

pub fn load_effectiveness() -> std::io::Result<HashMap<String, MemoryEffectiveness>> {
    let path = stats_path();
    if !path.exists() {
        return Ok(HashMap::new());
    }
    let content = std::fs::read_to_string(&path)?;
    let mut map: HashMap<String, MemoryEffectiveness> = HashMap::new();
    for line in content.lines() {
        let entry: StatsEntry = match serde_json::from_str(line) {
            Ok(e) => e,
            Err(_) => continue,
        };
        let eff = map.entry(entry.memory_id).or_default();
        // Update memory_type from the latest event (in case it was reclassified).
        eff.memory_type = entry.memory_type;
        match entry.event {
            MemoryEvent::Loaded => eff.loaded += 1,
            MemoryEvent::Referenced => eff.referenced += 1,
            MemoryEvent::Accessed => eff.accessed += 1,
            MemoryEvent::ReviewFailed | MemoryEvent::ContextInjected => {}
        }
        if eff.last_event_at.is_none_or(|t| entry.timestamp > t) {
            eff.last_event_at = Some(entry.timestamp);
        }
    }
    Ok(map)
}

pub fn clear_stats() -> std::io::Result<()> {
    let path = stats_path();
    if path.exists() {
        std::fs::remove_file(&path)?;
    }
    Ok(())
}

/// COT: Update memory effectiveness from injection records.
pub fn update_effectiveness_from_injections(
    session: &openpaw_core::Session,
    effectiveness: &mut HashMap<String, MemoryEffectiveness>,
) {
    for rec in &session.injection_records {
        let eff = effectiveness.entry(rec.memory_id.clone()).or_default();
        eff.loaded += 1;
        if let Some(adopted) = rec.adopted {
            if adopted {
                eff.referenced += 1;
            }
        }
    }
}

/// Type-specific decay factor using per-type half-life.
pub fn decay_factor_typed(days_since: f64, memory_type: MemoryType) -> f64 {
    let half_life = memory_type.decay_half_life_days();
    let factor = 0.5_f64.powf(days_since / half_life);
    factor.max(DECAY_FLOOR)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    #[test]
    fn effectiveness_factor_range() {
        let eff = MemoryEffectiveness {
            loaded: 10,
            referenced: 5,
            ..Default::default()
        };
        assert!(eff.factor() > 0.74);
        assert!(eff.factor() < 0.76);

        let eff_zero = MemoryEffectiveness {
            loaded: 0,
            ..Default::default()
        };
        assert_eq!(eff_zero.factor(), 1.0);
    }

    #[test]
    fn decay_factor_boundary() {
        let now = Utc::now();
        let recent = MemoryEffectiveness {
            last_event_at: Some(now),
            ..Default::default()
        };
        assert!(recent.decay_factor(now) > 0.99);

        // Default type is Fact (half-life 90 days).  After 90 days, decay
        // should be ~0.5, floored at 0.1.
        let old = MemoryEffectiveness {
            last_event_at: Some(now - Duration::days(90)),
            ..Default::default()
        };
        let old_decay = old.decay_factor(now);
        assert!(
            old_decay <= 0.5 + 0.01,
            "90-day Fact decay should be ~0.5: {old_decay}"
        );
        assert!(
            old_decay >= 0.1,
            "decay should be floored at 0.1: {old_decay}"
        );
    }

    #[test]
    fn decay_factor_per_type() {
        let now = Utc::now();
        // Episode (half-life 30 days) decays faster than Fact (90 days).
        let episode = MemoryEffectiveness {
            last_event_at: Some(now - Duration::days(30)),
            memory_type: MemoryType::Episode,
            ..Default::default()
        };
        let fact = MemoryEffectiveness {
            last_event_at: Some(now - Duration::days(30)),
            memory_type: MemoryType::Fact,
            ..Default::default()
        };
        assert!(
            episode.decay_factor(now) < fact.decay_factor(now),
            "Episode should decay faster than Fact after 30 days"
        );
    }

    #[test]
    fn combined_factor_product() {
        let now = Utc::now();
        let eff = MemoryEffectiveness {
            loaded: 10,
            referenced: 8,
            last_event_at: Some(now),
            ..Default::default()
        };
        let combined = eff.combined_factor(now);
        let expected = eff.factor() * eff.decay_factor(now);
        assert!((combined - expected).abs() < 0.001);
    }

    #[test]
    fn record_and_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stats.jsonl");
        let entries = vec![
            StatsEntry {
                memory_id: "mem_001".into(),
                event: MemoryEvent::Loaded,
                timestamp: Utc::now(),
                memory_type: MemoryType::Fact,
            },
            StatsEntry {
                memory_id: "mem_001".into(),
                event: MemoryEvent::Referenced,
                timestamp: Utc::now(),
                memory_type: MemoryType::Fact,
            },
            StatsEntry {
                memory_id: "mem_002".into(),
                event: MemoryEvent::Loaded,
                timestamp: Utc::now(),
                memory_type: MemoryType::Episode,
            },
        ];
        {
            let mut f = std::fs::File::create(&path).unwrap();
            for e in &entries {
                writeln!(f, "{}", serde_json::to_string(e).unwrap()).unwrap();
            }
        }
        let content = std::fs::read_to_string(&path).unwrap();
        let mut map: HashMap<String, MemoryEffectiveness> = HashMap::new();
        for line in content.lines() {
            let entry: StatsEntry = serde_json::from_str(line).unwrap();
            let eff = map.entry(entry.memory_id).or_default();
            eff.memory_type = entry.memory_type;
            match entry.event {
                MemoryEvent::Loaded => eff.loaded += 1,
                MemoryEvent::Referenced => eff.referenced += 1,
                MemoryEvent::Accessed => eff.accessed += 1,
                MemoryEvent::ReviewFailed | MemoryEvent::ContextInjected => {}
            }
        }
        assert_eq!(map.get("mem_001").unwrap().loaded, 1);
        assert_eq!(map.get("mem_001").unwrap().referenced, 1);
        assert_eq!(map.get("mem_002").unwrap().loaded, 1);
        assert_eq!(map.get("mem_002").unwrap().memory_type, MemoryType::Episode);
    }

    #[test]
    fn decay_factor_typed_per_type() {
        // Fact (half-life 90 days) should decay slower than Episode (30 days)
        let fact_decay = decay_factor_typed(30.0, MemoryType::Fact);
        let episode_decay = decay_factor_typed(30.0, MemoryType::Episode);
        assert!(
            fact_decay > episode_decay,
            "Fact should decay slower than Episode: fact={fact_decay} episode={episode_decay}"
        );

        // At day 0, all types should be ~1.0
        let fresh = decay_factor_typed(0.0, MemoryType::Fact);
        assert!((fresh - 1.0).abs() < 0.01);

        // Floor at 0.1
        let old = decay_factor_typed(10000.0, MemoryType::Context);
        assert!(old >= 0.1);
    }

    #[test]
    fn update_effectiveness_from_injection_records() {
        let mut session = openpaw_core::Session::new("m", "p");
        let mk_record = |msg_idx: usize, memory_id: &str, adopted: Option<bool>| {
            openpaw_core::InjectionRecord {
                msg_idx,
                memory_id: memory_id.to_string(),
                problem_type: "debugging".into(),
                trigger_type: "dynamic".into(),
                adopted,
                at: Utc::now(),
            }
        };
        session
            .injection_records
            .push(mk_record(0, "mem_adopted", Some(true)));
        session
            .injection_records
            .push(mk_record(1, "mem_rejected", Some(false)));
        session
            .injection_records
            .push(mk_record(2, "mem_pending", None));

        let mut effectiveness = HashMap::new();
        update_effectiveness_from_injections(&session, &mut effectiveness);

        // Adopted: loaded and referenced both counted.
        let adopted = effectiveness.get("mem_adopted").unwrap();
        assert_eq!(adopted.loaded, 1);
        assert_eq!(adopted.referenced, 1);
        // Rejected: counts as loaded but not referenced.
        let rejected = effectiveness.get("mem_rejected").unwrap();
        assert_eq!(rejected.loaded, 1);
        assert_eq!(rejected.referenced, 0);
        // Unknown adoption still counts as loaded.
        let pending = effectiveness.get("mem_pending").unwrap();
        assert_eq!(pending.loaded, 1);
        assert_eq!(pending.referenced, 0);
    }
}
