//! Skill 使用遥测（裁剪版）：数据结构 + 基于 JSON 的最小持久化。
//!
//! 仅保留 [`SkillTelemetry`] / [`SkillTelemetryState`] / [`SkillProvenance`]
//! 与 [`FsTelemetryStore`] 的读写/计数/状态推进能力；hub 上报、批量分析等
//! 复杂查询面不在本 crate 范围内。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::skills::skill::SkillStatus;

pub use crate::skills::provenance::SkillProvenance;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SkillTelemetryState {
    #[default]
    Active,
    Stale,
    Archived,
    Reactivated,
    Merged,
}

impl From<SkillTelemetryState> for SkillStatus {
    fn from(s: SkillTelemetryState) -> Self {
        match s {
            SkillTelemetryState::Active | SkillTelemetryState::Reactivated => SkillStatus::Active,
            SkillTelemetryState::Stale => SkillStatus::Stale,
            SkillTelemetryState::Archived => SkillStatus::Archived,
            SkillTelemetryState::Merged => SkillStatus::Merged,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillTelemetry {
    #[serde(default)]
    pub created_by: SkillProvenance,
    #[serde(default)]
    pub use_count: u32,
    #[serde(default)]
    pub view_count: u32,
    #[serde(default)]
    pub patch_count: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_used_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_viewed_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_patched_at: Option<DateTime<Utc>>,
    #[serde(default = "default_now")]
    pub created_at: DateTime<Utc>,
    #[serde(default)]
    pub state: SkillTelemetryState,
    #[serde(default)]
    pub pinned: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived_at: Option<DateTime<Utc>>,
}

fn default_now() -> DateTime<Utc> {
    Utc::now()
}

impl Default for SkillTelemetry {
    fn default() -> Self {
        Self {
            created_by: SkillProvenance::Unknown,
            use_count: 0,
            view_count: 0,
            patch_count: 0,
            last_used_at: None,
            last_viewed_at: None,
            last_patched_at: None,
            created_at: Utc::now(),
            state: SkillTelemetryState::Active,
            pinned: false,
            archived_at: None,
        }
    }
}

impl SkillTelemetry {
    pub fn factor(&self) -> f64 {
        if self.use_count == 0 && self.view_count == 0 {
            return 1.0;
        }
        // If the skill has been used but never "viewed" (e.g. auto-activated
        // without user interaction), we can't compute a meaningful ratio.
        // Return neutral 1.0 instead of penalizing.
        if self.view_count == 0 {
            return 1.0;
        }
        let ratio = self.use_count as f64 / self.view_count as f64;
        0.5 + 0.5 * ratio
    }

    pub fn decay_factor(&self, now: DateTime<Utc>) -> f64 {
        let last = self.last_used_at.or(self.last_viewed_at);
        match last {
            None => 1.0,
            Some(t) => {
                let days = (now - t).num_days() as f64;
                (0.5_f64.powf(days / 30.0)).max(0.3)
            }
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum TelemetryError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

pub type TelemetryResult<T> = std::result::Result<T, TelemetryError>;

pub struct FsTelemetryStore {
    root: PathBuf,
    // Mutex protects the load-modify-save cycle against concurrent
    // bump_view/bump_use/bump_patch calls that would otherwise cause
    // lost updates (two callers load the same state, both save, one
    // overwrites the other).
    lock: Mutex<()>,
}

impl FsTelemetryStore {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            lock: Mutex::new(()),
        }
    }

    fn usage_path(&self) -> PathBuf {
        self.root.join(".usage.json")
    }

    pub fn load(&self) -> TelemetryResult<HashMap<String, SkillTelemetry>> {
        let path = self.usage_path();
        if !path.exists() {
            return Ok(HashMap::new());
        }
        let content = std::fs::read_to_string(&path)?;
        let data: HashMap<String, SkillTelemetry> = serde_json::from_str(&content)?;
        Ok(data)
    }

    pub fn save(&self, data: &HashMap<String, SkillTelemetry>) -> TelemetryResult<()> {
        let path = self.usage_path();
        let content = serde_json::to_string_pretty(data)?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, &content)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }

    pub fn bump_view(&self, name: &str) -> TelemetryResult<()> {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut data = self.load()?;
        let entry = data.entry(name.to_string()).or_default();
        entry.view_count += 1;
        entry.last_viewed_at = Some(Utc::now());
        self.save(&data)
    }

    pub fn bump_use(&self, name: &str) -> TelemetryResult<()> {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut data = self.load()?;
        let entry = data.entry(name.to_string()).or_default();
        entry.use_count += 1;
        entry.last_used_at = Some(Utc::now());
        if entry.state == SkillTelemetryState::Stale {
            entry.state = SkillTelemetryState::Reactivated;
        }
        self.save(&data)
    }

    pub fn bump_patch(&self, name: &str) -> TelemetryResult<()> {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut data = self.load()?;
        let entry = data.entry(name.to_string()).or_default();
        entry.patch_count += 1;
        entry.last_patched_at = Some(Utc::now());
        self.save(&data)
    }

    pub fn mark_agent_created(&self, name: &str) -> TelemetryResult<()> {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut data = self.load()?;
        let entry = data.entry(name.to_string()).or_default();
        entry.created_by = SkillProvenance::Agent;
        self.save(&data)
    }

    pub fn get(&self, name: &str) -> TelemetryResult<Option<SkillTelemetry>> {
        let data = self.load()?;
        Ok(data.get(name).cloned())
    }

    pub fn set_pinned(&self, name: &str, pinned: bool) -> TelemetryResult<()> {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut data = self.load()?;
        let entry = data.entry(name.to_string()).or_default();
        entry.pinned = pinned;
        self.save(&data)
    }

    pub fn apply_automatic_transitions(
        &self,
        stale_after_days: u32,
        archive_after_days: u32,
    ) -> TelemetryResult<TransitionReport> {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut data = self.load()?;
        let mut report = TransitionReport::default();
        let now = Utc::now();

        for entry in data.values_mut() {
            if entry.pinned || entry.state == SkillTelemetryState::Archived {
                continue;
            }

            let last_activity = entry
                .last_used_at
                .or(entry.last_viewed_at)
                .or(entry.last_patched_at)
                .unwrap_or(entry.created_at);

            let days_idle = (now - last_activity).num_days() as u32;

            match entry.state {
                SkillTelemetryState::Active | SkillTelemetryState::Reactivated => {
                    if days_idle >= archive_after_days {
                        entry.state = SkillTelemetryState::Archived;
                        entry.archived_at = Some(now);
                        report.archived += 1;
                    } else if days_idle >= stale_after_days {
                        entry.state = SkillTelemetryState::Stale;
                        report.staled += 1;
                    }
                }
                SkillTelemetryState::Stale => {
                    if days_idle >= archive_after_days {
                        entry.state = SkillTelemetryState::Archived;
                        entry.archived_at = Some(now);
                        report.archived += 1;
                    }
                    // Stale skills only transition to Archived here.
                    // Reactivated is set only when a skill is actually used
                    // (in bump_use), not automatically based on historical
                    // use_count, which would cause oscillation.
                }
                SkillTelemetryState::Archived | SkillTelemetryState::Merged => {}
            }
        }

        self.save(&data)?;
        Ok(report)
    }

    /// 每日一次的自动生命周期转换入口（带节流）。
    ///
    /// [`Self::apply_automatic_transitions`] 的 30/90 天 Stale/Archived
    /// 转换没有常驻调用方。此方法供技能列表加载路径惰性调用：
    /// - 节流：在 store root 下写入 `.auto_transition_date` 标记文件记录
    ///   上次执行日期，同一天内重复调用直接跳过，避免每次 list 都扫描。
    /// - 安全：读/写/转换任何一步失败都返回 `None`，
    ///   不影响技能列表的正常返回。
    pub fn apply_automatic_transitions_daily(
        &self,
        stale_after_days: u32,
        archive_after_days: u32,
    ) -> Option<TransitionReport> {
        let marker = self.root.join(".auto_transition_date");
        let today = Utc::now().format("%Y-%m-%d").to_string();
        if let Ok(last) = std::fs::read_to_string(&marker) {
            if last.trim() == today {
                // 今天已执行过，节流跳过
                return None;
            }
        }
        match self.apply_automatic_transitions(stale_after_days, archive_after_days) {
            Ok(report) => {
                // 写节流标记；写失败只意味着下次会重复执行（转换幂等），可接受
                let _ = std::fs::write(&marker, &today);
                Some(report)
            }
            Err(_) => None,
        }
    }
}

#[derive(Debug, Default, Serialize)]
pub struct TransitionReport {
    pub staled: u32,
    pub archived: u32,
    pub reactivated: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store() -> FsTelemetryStore {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();
        std::mem::forget(dir);
        FsTelemetryStore::new(path)
    }

    #[test]
    fn test_bump_view() {
        let store = temp_store();
        store.bump_view("test-skill").unwrap();
        let t = store.get("test-skill").unwrap().unwrap();
        assert_eq!(t.view_count, 1);
        assert!(t.last_viewed_at.is_some());
    }

    #[test]
    fn test_bump_use() {
        let store = temp_store();
        store.bump_use("test-skill").unwrap();
        store.bump_use("test-skill").unwrap();
        let t = store.get("test-skill").unwrap().unwrap();
        assert_eq!(t.use_count, 2);
    }

    #[test]
    fn test_bump_patch() {
        let store = temp_store();
        store.bump_patch("test-skill").unwrap();
        let t = store.get("test-skill").unwrap().unwrap();
        assert_eq!(t.patch_count, 1);
    }

    #[test]
    fn test_mark_agent_created() {
        let store = temp_store();
        store.mark_agent_created("agent-skill").unwrap();
        let t = store.get("agent-skill").unwrap().unwrap();
        assert_eq!(t.created_by, SkillProvenance::Agent);
    }

    #[test]
    fn test_set_pinned() {
        let store = temp_store();
        store.bump_view("pinned-skill").unwrap();
        store.set_pinned("pinned-skill", true).unwrap();
        let t = store.get("pinned-skill").unwrap().unwrap();
        assert!(t.pinned);
    }

    #[test]
    fn test_automatic_transitions_stale() {
        let store = temp_store();
        let mut data = HashMap::new();
        let t = SkillTelemetry {
            created_by: SkillProvenance::Agent,
            last_used_at: Some(Utc::now() - chrono::Duration::days(35)),
            ..Default::default()
        };
        data.insert("old-skill".into(), t);
        store.save(&data).unwrap();

        let report = store.apply_automatic_transitions(30, 90).unwrap();
        assert_eq!(report.staled, 1);
        let updated = store.get("old-skill").unwrap().unwrap();
        assert_eq!(updated.state, SkillTelemetryState::Stale);
    }

    #[test]
    fn test_automatic_transitions_archive() {
        let store = temp_store();
        let mut data = HashMap::new();
        let t = SkillTelemetry {
            last_used_at: Some(Utc::now() - chrono::Duration::days(95)),
            ..Default::default()
        };
        data.insert("very-old".into(), t);
        store.save(&data).unwrap();

        let report = store.apply_automatic_transitions(30, 90).unwrap();
        assert_eq!(report.archived, 1);
        let updated = store.get("very-old").unwrap().unwrap();
        assert_eq!(updated.state, SkillTelemetryState::Archived);
    }

    #[test]
    fn test_pinned_not_transitioned() {
        let store = temp_store();
        let mut data = HashMap::new();
        let t = SkillTelemetry {
            pinned: true,
            last_used_at: Some(Utc::now() - chrono::Duration::days(100)),
            ..Default::default()
        };
        data.insert("pinned".into(), t);
        store.save(&data).unwrap();

        let report = store.apply_automatic_transitions(30, 90).unwrap();
        assert_eq!(report.archived, 0);
    }

    #[test]
    fn decay_factor_boundary() {
        let now = Utc::now();
        let tel = SkillTelemetry {
            last_used_at: Some(now),
            ..Default::default()
        };
        assert!(tel.decay_factor(now) > 0.99);

        let old = SkillTelemetry {
            last_used_at: Some(now - chrono::Duration::days(60)),
            ..Default::default()
        };
        assert!(old.decay_factor(now) <= 0.3 + 0.01);
    }

    #[test]
    fn factor_no_data_is_one() {
        let tel = SkillTelemetry::default();
        assert_eq!(tel.factor(), 1.0);
    }

    #[test]
    fn factor_high_use_ratio() {
        let tel = SkillTelemetry {
            use_count: 8,
            view_count: 10,
            ..Default::default()
        };
        assert!(tel.factor() > 0.8);
    }

    #[test]
    fn factor_used_but_never_viewed_is_neutral() {
        // A skill that has been used but never "viewed" should not be
        // penalized — return neutral 1.0 since we can't compute a ratio.
        let tel = SkillTelemetry {
            use_count: 5,
            view_count: 0,
            ..Default::default()
        };
        assert_eq!(tel.factor(), 1.0);
    }

    #[test]
    fn test_stale_reactivated_when_used() {
        let store = temp_store();
        let mut data = HashMap::new();
        // Skill is Stale, idle for 35 days (within archive threshold of 90),
        // with historical use_count > 0. Automatic transitions should NOT
        // reactivate it (that would cause oscillation). It stays Stale.
        let t = SkillTelemetry {
            state: SkillTelemetryState::Stale,
            use_count: 3,
            last_used_at: Some(Utc::now() - chrono::Duration::days(35)),
            ..Default::default()
        };
        data.insert("stale-used".into(), t);
        store.save(&data).unwrap();

        let report = store.apply_automatic_transitions(30, 90).unwrap();
        assert_eq!(report.reactivated, 0);
        let updated = store.get("stale-used").unwrap().unwrap();
        assert_eq!(updated.state, SkillTelemetryState::Stale);

        // Reactivated should only happen on actual use (bump_use).
        store.bump_use("stale-used").unwrap();
        let updated = store.get("stale-used").unwrap().unwrap();
        assert_eq!(updated.state, SkillTelemetryState::Reactivated);
    }

    #[test]
    fn test_stale_archived_when_idle_too_long() {
        let store = temp_store();
        let mut data = HashMap::new();
        // Skill is Stale and idle for 95 days (past archive threshold of 90)
        // — should be archived even if it was used before.
        let t = SkillTelemetry {
            state: SkillTelemetryState::Stale,
            use_count: 5,
            last_used_at: Some(Utc::now() - chrono::Duration::days(95)),
            ..Default::default()
        };
        data.insert("stale-very-old".into(), t);
        store.save(&data).unwrap();

        let report = store.apply_automatic_transitions(30, 90).unwrap();
        assert_eq!(report.archived, 1);
        let updated = store.get("stale-very-old").unwrap().unwrap();
        assert_eq!(updated.state, SkillTelemetryState::Archived);
    }

    /// daily 入口首次调用应执行转换并写入当日节流标记。
    #[test]
    fn test_daily_transitions_first_call_executes() {
        let dir = tempfile::tempdir().unwrap();
        let store = FsTelemetryStore::new(dir.path().to_path_buf());
        let mut data = HashMap::new();
        data.insert(
            "old".into(),
            SkillTelemetry {
                last_used_at: Some(Utc::now() - chrono::Duration::days(95)),
                ..Default::default()
            },
        );
        store.save(&data).unwrap();

        let report = store
            .apply_automatic_transitions_daily(30, 90)
            .expect("first daily call should run transitions");
        assert_eq!(report.archived, 1);

        // 节流标记应记录今天的日期
        let marker = dir.path().join(".auto_transition_date");
        assert_eq!(
            std::fs::read_to_string(&marker).unwrap().trim(),
            Utc::now().format("%Y-%m-%d").to_string()
        );
    }

    /// 同一天内重复调用应被节流（返回 None 且不再改动状态）。
    #[test]
    fn test_daily_transitions_throttled_same_day() {
        let dir = tempfile::tempdir().unwrap();
        let store = FsTelemetryStore::new(dir.path().to_path_buf());
        let mut data = HashMap::new();
        data.insert(
            "aging".into(),
            SkillTelemetry {
                last_used_at: Some(Utc::now() - chrono::Duration::days(35)),
                ..Default::default()
            },
        );
        store.save(&data).unwrap();

        // 第一次：Active → Stale
        let report = store
            .apply_automatic_transitions_daily(30, 90)
            .expect("first daily call should run");
        assert_eq!(report.staled, 1);

        // 数据变化为 95 天未用（若无节流会被推进到 Archived）
        let mut data2 = HashMap::new();
        data2.insert(
            "aging".into(),
            SkillTelemetry {
                state: SkillTelemetryState::Stale,
                last_used_at: Some(Utc::now() - chrono::Duration::days(95)),
                ..Default::default()
            },
        );
        store.save(&data2).unwrap();

        // 第二次（同一天）：节流跳过，状态保持 Stale
        assert!(store.apply_automatic_transitions_daily(30, 90).is_none());
        let updated = store.get("aging").unwrap().unwrap();
        assert_eq!(updated.state, SkillTelemetryState::Stale);
    }

    /// 节流标记为往日日期时应重新执行转换。
    #[test]
    fn test_daily_transitions_reruns_when_marker_stale() {
        let dir = tempfile::tempdir().unwrap();
        let store = FsTelemetryStore::new(dir.path().to_path_buf());
        // 预写昨天的标记
        let yesterday = (Utc::now() - chrono::Duration::days(1))
            .format("%Y-%m-%d")
            .to_string();
        std::fs::write(dir.path().join(".auto_transition_date"), &yesterday).unwrap();

        let mut data = HashMap::new();
        data.insert(
            "old".into(),
            SkillTelemetry {
                last_used_at: Some(Utc::now() - chrono::Duration::days(95)),
                ..Default::default()
            },
        );
        store.save(&data).unwrap();

        let report = store
            .apply_automatic_transitions_daily(30, 90)
            .expect("stale marker should allow rerun");
        assert_eq!(report.archived, 1);
    }
}
