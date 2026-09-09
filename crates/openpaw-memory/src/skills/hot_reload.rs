//! Skill hot-reload — watches the skill directories for changes and
//! reloads skills automatically when files are modified.
//!
//! 移植适配：母仓基于 tokio 的异步轮询循环，在本 crate（无 tokio 依赖）
//! 中改为 std 线程 + `std::sync::mpsc` 的同步轮询实现，事件类型与
//! 快照对比逻辑保持一致。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use crate::skills::{FsSkillStore, SkillStore};

#[derive(Debug, Clone)]
pub enum SkillChangeEvent {
    Created { name: String, path: PathBuf },
    Modified { name: String, path: PathBuf },
    Deleted { name: String },
}

/// A single skill's watch state captured between polls.
pub struct SkillSnapshot {
    pub path: PathBuf,
    pub mtime: SystemTime,
}

pub struct SkillWatcher {
    store: FsSkillStore,
    poll_interval: Duration,
}

impl SkillWatcher {
    pub fn new(store: FsSkillStore) -> Self {
        Self {
            store,
            poll_interval: Duration::from_secs(5),
        }
    }

    pub fn with_poll_interval(mut self, interval: Duration) -> Self {
        self.poll_interval = interval;
        self
    }

    /// Snapshot the current skills known to the store. `None` means the
    /// listing failed and the previous state should be kept.
    fn snapshot_skills(&self) -> Option<HashMap<String, SkillSnapshot>> {
        let skills = match self.store.list() {
            Ok(s) => s,
            Err(_) => return None,
        };
        let mut map = HashMap::new();
        for s in skills {
            let mtime = std::fs::metadata(&s.source)
                .and_then(|m| m.modified())
                .unwrap_or(SystemTime::UNIX_EPOCH);
            map.insert(
                s.name().to_string(),
                SkillSnapshot {
                    path: s.source.clone(),
                    mtime,
                },
            );
        }
        Some(map)
    }

    /// Diff `known` (previous snapshot, updated in place) against a fresh
    /// snapshot. Returns the observed change events. Shared by the watch
    /// loop and tests.
    pub fn poll_changes(
        &self,
        known: &mut HashMap<String, SkillSnapshot>,
    ) -> Vec<SkillChangeEvent> {
        let current = match self.snapshot_skills() {
            Some(c) => c,
            // Listing failed: keep previous state, report no changes.
            None => return Vec::new(),
        };
        let mut events = Vec::new();

        for (name, entry) in &current {
            match known.get(name) {
                None => events.push(SkillChangeEvent::Created {
                    name: name.clone(),
                    path: entry.path.clone(),
                }),
                Some(old) => {
                    // Detect modification by comparing mtime (path alone
                    // doesn't change when content is edited in-place).
                    if old.path != entry.path || old.mtime != entry.mtime {
                        events.push(SkillChangeEvent::Modified {
                            name: name.clone(),
                            path: entry.path.clone(),
                        });
                    }
                }
            }
        }

        for name in known.keys() {
            if !current.contains_key(name) {
                events.push(SkillChangeEvent::Deleted { name: name.clone() });
            }
        }

        *known = current;
        events
    }

    /// Run the watch loop on a background thread until `stop` is set or
    /// the receiver is dropped. This replaces the parent repo's tokio
    /// async loop (this crate has no tokio dependency).
    pub fn watch(
        self,
        tx: std::sync::mpsc::Sender<SkillChangeEvent>,
        stop: Arc<AtomicBool>,
    ) -> std::thread::JoinHandle<()> {
        std::thread::spawn(move || {
            let mut known = self.snapshot_skills().unwrap_or_default();
            while !stop.load(Ordering::Relaxed) {
                std::thread::sleep(self.poll_interval);
                if stop.load(Ordering::Relaxed) {
                    return;
                }
                for event in self.poll_changes(&mut known) {
                    if tx.send(event).is_err() {
                        return; // receiver dropped, stop watching
                    }
                }
            }
        })
    }

    /// Convenience wrapper: `watch` with a shared stop flag already
    /// allocated. Returns the handle plus the flag used to stop it.
    pub fn watch_shared(
        self,
        tx: std::sync::mpsc::Sender<SkillChangeEvent>,
    ) -> (std::thread::JoinHandle<()>, Arc<AtomicBool>) {
        let stop = Arc::new(AtomicBool::new(false));
        let handle = self.watch(tx, Arc::clone(&stop));
        (handle, stop)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// 串行化会启动线程的 watcher 测试。
    static WATCH_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn snapshot_captures_skills() {
        let dir = tempfile::tempdir().unwrap();
        let store = FsSkillStore::new(dir.path().to_path_buf(), None);
        let watcher = SkillWatcher::new(store);

        let snapshot = watcher.snapshot_skills();
        assert!(snapshot.unwrap_or_default().is_empty());
    }

    #[test]
    fn change_event_types() {
        let created = SkillChangeEvent::Created {
            name: "test".into(),
            path: PathBuf::from("/tmp/test"),
        };
        let modified = SkillChangeEvent::Modified {
            name: "test".into(),
            path: PathBuf::from("/tmp/test"),
        };
        let deleted = SkillChangeEvent::Deleted {
            name: "test".into(),
        };

        match created {
            SkillChangeEvent::Created { name, .. } => assert_eq!(name, "test"),
            _ => panic!("wrong variant"),
        }
        match modified {
            SkillChangeEvent::Modified { name, .. } => assert_eq!(name, "test"),
            _ => panic!("wrong variant"),
        }
        match deleted {
            SkillChangeEvent::Deleted { name } => assert_eq!(name, "test"),
            _ => panic!("wrong variant"),
        }
    }

    /// poll_changes 应能检测到新建技能（Created 事件）。
    #[test]
    fn poll_changes_detects_created() {
        let _g = WATCH_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        // watcher 与写入方分别持有同一 root 的 store 实例
        let watcher = SkillWatcher::new(FsSkillStore::new(dir.path().to_path_buf(), None));
        let write_store = FsSkillStore::new(dir.path().to_path_buf(), None);

        let mut known = HashMap::new();
        assert!(watcher.poll_changes(&mut known).is_empty());

        use crate::skills::{Scope, SkillFrontmatter};
        let fm = SkillFrontmatter {
            name: "hot-skill".into(),
            description: "hot reload test".into(),
            ..Default::default()
        };
        write_store.put(Scope::User, fm, "body").unwrap();

        let events = watcher.poll_changes(&mut known);
        assert_eq!(events.len(), 1);
        match &events[0] {
            SkillChangeEvent::Created { name, .. } => assert_eq!(name, "hot-skill"),
            other => panic!("expected Created, got {other:?}"),
        }

        // 第二次轮询无变化
        assert!(watcher.poll_changes(&mut known).is_empty());
    }

    /// watch 线程应在 stop 置位后退出。
    #[test]
    fn watch_thread_stops_on_flag() {
        let _g = WATCH_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let store = FsSkillStore::new(dir.path().to_path_buf(), None);
        let (tx, _rx) = std::sync::mpsc::channel();
        let watcher = SkillWatcher::new(store).with_poll_interval(Duration::from_millis(10));
        let (handle, stop) = watcher.watch_shared(tx);
        stop.store(true, Ordering::Relaxed);
        handle.join().unwrap();
    }
}
