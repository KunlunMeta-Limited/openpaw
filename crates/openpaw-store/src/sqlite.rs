//! SQLite persistence layer for session metadata and message indexing.
//!
//! JSONL remains the primary append-only log; SQLite serves as a secondary
//! index for structured queries and full-text search (FTS5). The schema also
//! hosts the `audit_log` table with its own FTS5 virtual table.

#![allow(clippy::type_complexity)]

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use openpaw_core::{Role, SessionEvent, SessionMeta};
use rusqlite::{params, Connection};
use serde::Serialize;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum DbError {
    #[error("sqlite error: {0}")]
    Sqlite(
        #[source]
        #[from]
        rusqlite::Error,
    ),
    #[error("io error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("poisoned mutex")]
    PoisonedMutex,
}

pub type Result<T> = std::result::Result<T, DbError>;

/// Unified SQLite database handle. Thread-safe (`Send + Sync`) via an internal
/// `Mutex<Connection>`. In WAL mode read/write contention is minimal.
pub struct SessionDb {
    conn: Mutex<Connection>,
}

/// Type alias: the global unified DB is just a `SessionDb`.
pub type OpenPawDb = SessionDb;

impl SessionDb {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| DbError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        let conn = Connection::open(path)?;
        // busy_timeout makes concurrent openers (CLI + TUI starting together)
        // wait for each other's write locks instead of failing instantly with
        // SQLITE_BUSY.
        conn.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON; PRAGMA busy_timeout=5000;",
        )?;
        let db = Self {
            conn: Mutex::new(conn),
        };
        db.init_schema()?;
        Ok(db)
    }

    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON; PRAGMA busy_timeout=5000;",
        )?;
        let db = Self {
            conn: Mutex::new(conn),
        };
        db.init_schema()?;
        Ok(db)
    }

    /// Execute a closure with exclusive access to the underlying connection.
    pub fn with_conn<F, T>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&Connection) -> Result<T>,
    {
        let conn = self.conn.lock().map_err(|_| DbError::PoisonedMutex)?;
        f(&conn)
    }

    fn init_schema(&self) -> Result<()> {
        self.with_conn(|conn| {
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS schema_version (
                    version     INTEGER PRIMARY KEY,
                    applied_at  TEXT NOT NULL
                );

                CREATE TABLE IF NOT EXISTS sessions (
                    id          TEXT PRIMARY KEY,
                    model       TEXT NOT NULL,
                    provider    TEXT NOT NULL,
                    created_at  TEXT NOT NULL,
                    updated_at  TEXT NOT NULL,
                    message_count INTEGER NOT NULL DEFAULT 0,
                    jsonl_path  TEXT
                );

                CREATE TABLE IF NOT EXISTS messages (
                    id          INTEGER PRIMARY KEY AUTOINCREMENT,
                    session_id  TEXT NOT NULL REFERENCES sessions(id),
                    role        TEXT NOT NULL,
                    seq         INTEGER NOT NULL,
                    text_preview TEXT,
                    full_text   TEXT,
                    created_at  TEXT NOT NULL DEFAULT ''
                );

                CREATE VIRTUAL TABLE IF NOT EXISTS messages_fts USING fts5(
                    text_preview,
                    content='messages',
                    content_rowid='id',
                    tokenize='unicode61'
                );

                CREATE TRIGGER IF NOT EXISTS messages_ai AFTER INSERT ON messages BEGIN
                    INSERT INTO messages_fts(rowid, text_preview) VALUES (new.id, new.text_preview);
                END;

                CREATE TRIGGER IF NOT EXISTS messages_ad AFTER DELETE ON messages BEGIN
                    INSERT INTO messages_fts(messages_fts, rowid, text_preview) VALUES('delete', old.id, old.text_preview);
                END;

                CREATE INDEX IF NOT EXISTS idx_messages_session_id ON messages(session_id);
                CREATE INDEX IF NOT EXISTS idx_messages_role ON messages(role);",
            )?;
            Ok(())
        })?;
        self.run_migrations()?;
        Ok(())
    }

    fn run_migrations(&self) -> Result<()> {
        self.with_conn(|conn| {
            // Run the whole migration sequence inside a single EXCLUSIVE
            // transaction. A second process opening the same file then waits
            // on the write lock (see busy_timeout above) instead of reading
            // `current = 0` mid-migration and re-running non-idempotent
            // migrations (duplicate column / PK conflict → open() failed).
            // All-or-nothing is also strictly safer than a half-migrated
            // persistent state.
            conn.execute_batch("BEGIN EXCLUSIVE;")?;
            let result = (|| -> Result<()> {
                let current: i64 = conn.query_row(
                    "SELECT COALESCE(MAX(version), 0) FROM schema_version",
                    [],
                    |row| row.get(0),
                )?;

                let migrations: &[(&dyn Fn(&Connection) -> Result<()>, i64)] = &[
                    (&migrate_v1, 1),
                    (&migrate_v2, 2),
                    (&migrate_v3, 3),
                    (&migrate_v4, 4),
                    (&migrate_v5, 5),
                    (&migrate_v6, 6),
                    (&migrate_v7, 7),
                    // Audit trail: base table + FTS5 virtual table.
                    (&migrate_v8_audit_log, 8),
                ];

                for (migration_fn, version) in migrations {
                    if current < *version {
                        migration_fn(conn)?;
                        conn.execute(
                            "INSERT INTO schema_version (version, applied_at) VALUES (?1, ?2)",
                            params![version, now_rfc3339(conn)],
                        )?;
                    }
                }
                Ok(())
            })();

            match result {
                Ok(()) => {
                    conn.execute_batch("COMMIT;")?;
                    Ok(())
                }
                Err(e) => {
                    let _ = conn.execute_batch("ROLLBACK;");
                    Err(e)
                }
            }
        })
    }

    pub fn schema_version(&self) -> Result<i64> {
        self.with_conn(|conn| {
            let v: i64 = conn.query_row(
                "SELECT COALESCE(MAX(version), 0) FROM schema_version",
                [],
                |row| row.get(0),
            )?;
            Ok(v)
        })
    }

    // ===== Session CRUD =====

    /// 功能：在取得 SQLite 写锁之后读取并原子替换单会话可见索引。
    /// 参数：home 为目录范围，id 为合法会话身份。
    /// 返回：本次已提交快照。
    /// 错误：路径/投影/索引失败返回脱敏 CatalogError。
    /// 状态：仅替换 transcript 管理字段和消息；保留审计、标签、pin、token 等字段。
    /// 并发与取消：IMMEDIATE 事务先于快照读取，防止旧快照覆盖并发刷新。
    /// 权限：不信任旧索引路径，不改 JSONL 或 schema。
    /// 关联：CLI chat 与 session search。
    pub fn refresh_transcript(
        &self,
        home: &Path,
        id: &str,
    ) -> std::result::Result<crate::catalog::Snapshot, crate::catalog::CatalogError> {
        use crate::catalog::{self, CatalogError};
        let conn = self
            .conn
            .lock()
            .map_err(|_| CatalogError("session index lock failed".into()))?;
        let tx = rusqlite::Transaction::new_unchecked(
            &conn,
            rusqlite::TransactionBehavior::Immediate,
        )
        .map_err(|_| CatalogError("cannot acquire session index write transaction".into()))?;
        let snapshot = match catalog::read(home, id) {
            Ok(snapshot) => snapshot,
            Err(error) => {
                // 损坏记录不能继续从陈旧 FTS 暴露已隐藏或未提交正文。
                if catalog::valid_id(id) {
                    tx.execute("DELETE FROM messages WHERE session_id=?1", [id])
                        .map_err(|_| {
                            CatalogError("cannot invalidate transcript messages".into())
                        })?;
                    tx.execute("UPDATE sessions SET message_count=0 WHERE id=?1", [id])
                        .map_err(|_| CatalogError("cannot invalidate transcript count".into()))?;
                    tx.commit().map_err(|_| {
                        CatalogError("cannot commit transcript invalidation".into())
                    })?;
                }
                return Err(error);
            }
        };
        let messages = snapshot.transcript.visible_messages(id);
        let modified = snapshot
            .updated_at
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        let updated: String = tx
            .query_row(
                "SELECT strftime('%Y-%m-%dT%H:%M:%fZ', ?1, 'unixepoch')",
                [modified],
                |row| row.get(0),
            )
            .map_err(|_| CatalogError("cannot resolve transcript update timestamp".into()))?;
        let parent = snapshot
            .transcript
            .mutation
            .fork_origin
            .as_ref()
            .map(|origin| &origin.source_session_id);
        let result = (|| -> Result<()> {
            tx.execute("INSERT INTO sessions (id,model,provider,created_at,updated_at,message_count,jsonl_path,title,workspace_id,parent_session_id)
                VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)
                ON CONFLICT(id) DO UPDATE SET model=excluded.model,provider=excluded.provider,
                created_at=excluded.created_at,updated_at=excluded.updated_at,message_count=excluded.message_count,
                jsonl_path=excluded.jsonl_path,title=excluded.title,workspace_id=excluded.workspace_id,
                parent_session_id=COALESCE(excluded.parent_session_id,sessions.parent_session_id)",
                params![id,snapshot.meta.model,snapshot.meta.provider,snapshot.meta.created_at.to_rfc3339(),updated,
                    messages.iter().filter(|m| m.role == Role::User).count() as i64,snapshot.path.to_string_lossy(),
                    snapshot.transcript.title(),snapshot.meta.workspace_id.as_deref().unwrap_or("default"),parent])?;
            tx.execute("DELETE FROM messages WHERE session_id=?1", [id])?;
            for (seq, message) in messages.iter().enumerate() {
                let text = message.text();
                if text.is_empty() {
                    continue;
                }
                let preview: String = text.chars().take(500).collect();
                let role = if message.role == Role::User {
                    "user"
                } else {
                    "assistant"
                };
                tx.execute("INSERT INTO messages (session_id,role,seq,text_preview,full_text,created_at) VALUES (?1,?2,?3,?4,?5,?6)",
                    params![id,role,seq as i64,preview,text,updated])?;
            }
            tx.commit()?;
            Ok(())
        })();
        result.map_err(|_| CatalogError(format!("session {id}: cannot refresh index; check index.db permissions and preserve audit data")))?;
        Ok(snapshot)
    }

    pub fn upsert_session(&self, meta: &SessionMeta, jsonl_path: &Path) -> Result<()> {
        let workspace_id = meta
            .workspace_id
            .clone()
            .unwrap_or_else(|| "default".to_string());
        self.with_conn(|conn| {
            let now = now_rfc3339(conn);
            conn.execute(
                "INSERT INTO sessions (id, model, provider, created_at, updated_at, message_count, jsonl_path, workspace_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, 0, ?6, ?7)
                 ON CONFLICT(id) DO UPDATE SET
                    model = excluded.model,
                    provider = excluded.provider,
                    updated_at = excluded.updated_at,
                    jsonl_path = excluded.jsonl_path,
                    workspace_id = excluded.workspace_id",
                params![
                    meta.id,
                    meta.model,
                    meta.provider,
                    meta.created_at.to_rfc3339(),
                    now,
                    jsonl_path.to_string_lossy().to_string(),
                    workspace_id,
                ],
            )?;
            Ok(())
        })
    }

    pub fn index_message(
        &self,
        session_id: &str,
        role: &str,
        seq: usize,
        text: &str,
    ) -> Result<()> {
        let preview: String = text.chars().take(500).collect();
        self.with_conn(|conn| {
            let now = now_rfc3339(conn);
            conn.execute(
                "INSERT INTO messages (session_id, role, seq, text_preview, full_text, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![session_id, role, seq as i64, preview, text, now],
            )?;
            Ok(())
        })
    }

    /// Index a message with an optional tool_name (for ToolUse / ToolResult blocks).
    pub fn index_message_with_tool(
        &self,
        session_id: &str,
        role: &str,
        seq: usize,
        text: &str,
        tool_name: Option<&str>,
    ) -> Result<()> {
        let preview: String = text.chars().take(500).collect();
        self.with_conn(|conn| {
            let now = now_rfc3339(conn);
            conn.execute(
                "INSERT INTO messages (session_id, role, seq, text_preview, full_text, created_at, tool_name)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![session_id, role, seq as i64, preview, text, now, tool_name],
            )?;
            Ok(())
        })
    }

    pub fn index_event(&self, session_id: &str, seq: usize, event: &SessionEvent) -> Result<()> {
        match event {
            SessionEvent::Meta(meta) => {
                let default_path = PathBuf::from(format!("{}.jsonl", meta.id));
                self.upsert_session(meta, &default_path)
            }
            SessionEvent::Message(msg) => {
                let role_str = match msg.role {
                    Role::User => "user",
                    Role::Assistant => "assistant",
                };
                let text = msg.text();
                if !text.is_empty() {
                    self.index_message(session_id, role_str, seq, &text)
                } else {
                    Ok(())
                }
            }
            SessionEvent::Usage(_) => Ok(()),
            _ => Ok(()),
        }
    }

    pub fn touch_session(&self, session_id: &str) -> Result<()> {
        self.with_conn(|conn| {
            let now = now_rfc3339(conn);
            conn.execute(
                "UPDATE sessions SET updated_at = ?1, message_count = message_count + 1 WHERE id = ?2",
                params![now, session_id],
            )?;
            Ok(())
        })
    }

    pub fn session_exists(&self, session_id: &str) -> Result<bool> {
        self.with_conn(|conn| {
            let count: i64 = conn.query_row(
                "SELECT COUNT(*) FROM sessions WHERE id = ?1",
                params![session_id],
                |row| row.get(0),
            )?;
            Ok(count > 0)
        })
    }

    pub fn get_session_count(&self) -> Result<i64> {
        self.with_conn(|conn| {
            let count: i64 =
                conn.query_row("SELECT COUNT(*) FROM sessions", [], |row| row.get(0))?;
            Ok(count)
        })
    }

    pub fn get_message_count(&self, session_id: &str) -> Result<i64> {
        self.with_conn(|conn| {
            let count: i64 = conn.query_row(
                "SELECT COUNT(*) FROM messages WHERE session_id = ?1",
                params![session_id],
                |row| row.get(0),
            )?;
            Ok(count)
        })
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionSummary {
    pub id: String,
    pub model: String,
    pub provider: String,
    pub created_at: String,
    pub updated_at: String,
    pub message_count: i64,
    pub title: String,
    pub jsonl_path: Option<String>,
    #[serde(default)]
    pub workspace_id: String,
    #[serde(default)]
    pub parent_session_id: Option<String>,
    #[serde(default)]
    pub is_sub_agent: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct SearchResult {
    pub session_id: String,
    pub role: String,
    pub seq: usize,
    pub text_preview: String,
    /// FTS5 `snippet()` highlight of the matched column, markers wrapped in
    /// square brackets. Empty when the match carries no highlightable text.
    pub snippet: String,
    pub rank: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct MessageRecord {
    pub id: i64,
    pub session_id: String,
    pub role: String,
    pub seq: usize,
    pub text_preview: Option<String>,
    pub full_text: Option<String>,
    pub created_at: String,
    pub tool_name: Option<String>,
}

// ===== Session listing / search impl =====

impl SessionDb {
    fn map_session_row(row: &rusqlite::Row) -> rusqlite::Result<SessionSummary> {
        Ok(SessionSummary {
            id: row.get(0)?,
            model: row.get(1)?,
            provider: row.get(2)?,
            created_at: row.get(3)?,
            updated_at: row.get(4)?,
            message_count: row.get(5)?,
            title: row.get(6)?,
            jsonl_path: row.get(7)?,
            workspace_id: row.get(8)?,
            parent_session_id: row.get(9)?,
            is_sub_agent: row.get::<_, i64>(10)? != 0,
        })
    }

    const SESSION_SELECT: &'static str =
        "SELECT id, model, provider, created_at, updated_at, message_count, \
         COALESCE(title, ''), jsonl_path, COALESCE(workspace_id, 'default'), \
         parent_session_id, COALESCE(is_sub_agent, 0)";

    pub fn list_sessions(&self) -> Result<Vec<SessionSummary>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(&format!(
                "{} FROM sessions ORDER BY updated_at DESC",
                Self::SESSION_SELECT
            ))?;
            let rows = stmt.query_map([], Self::map_session_row)?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        })
    }

    pub fn filter_sessions(
        &self,
        model: Option<&str>,
        provider: Option<&str>,
    ) -> Result<Vec<SessionSummary>> {
        self.with_conn(|conn| {
            let mut sql = format!("{} FROM sessions WHERE 1=1", Self::SESSION_SELECT);
            let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
            if let Some(m) = model {
                sql.push_str(" AND model = ?");
                param_values.push(Box::new(m.to_string()));
            }
            if let Some(p) = provider {
                sql.push_str(" AND provider = ?");
                param_values.push(Box::new(p.to_string()));
            }
            sql.push_str(" ORDER BY updated_at DESC");
            let params_refs: Vec<&dyn rusqlite::types::ToSql> =
                param_values.iter().map(|p| p.as_ref()).collect();
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map(params_refs.as_slice(), Self::map_session_row)?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        })
    }

    pub fn delete_session(&self, session_id: &str) -> Result<()> {
        self.with_conn(|conn| {
            let tx = conn.unchecked_transaction()?;
            tx.execute("DELETE FROM messages WHERE session_id = ?", [session_id])?;
            tx.execute("DELETE FROM sessions WHERE id = ?", [session_id])?;
            tx.commit()?;
            Ok(())
        })
    }

    pub fn get_session_by_id(&self, session_id: &str) -> Result<Option<SessionSummary>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(&format!(
                "{} FROM sessions WHERE id = ?1",
                Self::SESSION_SELECT
            ))?;
            let mut rows = stmt.query_map(params![session_id], Self::map_session_row)?;
            match rows.next() {
                Some(Ok(s)) => Ok(Some(s)),
                Some(Err(e)) => Err(DbError::Sqlite(e)),
                None => Ok(None),
            }
        })
    }

    pub fn get_jsonl_path(&self, session_id: &str) -> Result<Option<PathBuf>> {
        self.with_conn(|conn| {
            let path: Option<String> = conn
                .query_row(
                    "SELECT jsonl_path FROM sessions WHERE id = ?1",
                    params![session_id],
                    |row| row.get(0),
                )
                .ok();
            Ok(path.map(PathBuf::from))
        })
    }

    pub fn update_session_title(&self, session_id: &str, title: &str) -> Result<()> {
        self.with_conn(|conn| {
            conn.execute(
                "UPDATE sessions SET title = ?1 WHERE id = ?2",
                params![title, session_id],
            )?;
            Ok(())
        })
    }

    pub fn update_session_tokens(
        &self,
        session_id: &str,
        input_tokens: u32,
        output_tokens: u32,
    ) -> Result<()> {
        self.with_conn(|conn| {
            conn.execute(
                "UPDATE sessions SET total_input_tokens = ?1, total_output_tokens = ?2 WHERE id = ?3",
                params![input_tokens as i64, output_tokens as i64, session_id],
            )?;
            Ok(())
        })
    }

    /// Accumulate session token usage (distinct from the overwrite semantics
    /// of `update_session_tokens`). When the session row does not exist yet a
    /// minimal record is created so gateway-style callers keyed by a session
    /// string can still accumulate stats. Each call receives the delta for
    /// this invocation, not the session total.
    pub fn add_session_tokens(
        &self,
        session_id: &str,
        input_tokens: u32,
        output_tokens: u32,
    ) -> Result<()> {
        self.with_conn(|conn| {
            let now = now_rfc3339(conn);
            conn.execute(
                "INSERT INTO sessions (id, model, provider, created_at, updated_at, message_count, total_input_tokens, total_output_tokens)
                 VALUES (?1, 'gateway', 'gateway', ?2, ?2, 0, ?3, ?4)
                 ON CONFLICT(id) DO UPDATE SET
                    updated_at = excluded.updated_at,
                    total_input_tokens = total_input_tokens + excluded.total_input_tokens,
                    total_output_tokens = total_output_tokens + excluded.total_output_tokens",
                params![session_id, now, input_tokens as i64, output_tokens as i64],
            )?;
            Ok(())
        })
    }

    pub fn get_total_tokens(&self) -> Result<(u32, u32)> {
        self.with_conn(|conn| {
            let (inp, out): (i64, i64) = conn.query_row(
                "SELECT COALESCE(SUM(total_input_tokens), 0), COALESCE(SUM(total_output_tokens), 0) FROM sessions",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            Ok((
                inp.min(u32::MAX as i64) as u32,
                out.min(u32::MAX as i64) as u32,
            ))
        })
    }

    pub fn get_session_messages(
        &self,
        session_id: &str,
        limit: usize,
    ) -> Result<Vec<MessageRecord>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, session_id, role, seq, text_preview, full_text, created_at, tool_name
                 FROM messages WHERE session_id = ?1 ORDER BY seq ASC LIMIT ?2",
            )?;
            let rows = stmt.query_map(params![session_id, limit as i64], Self::map_message_row)?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        })
    }

    fn map_message_row(row: &rusqlite::Row) -> rusqlite::Result<MessageRecord> {
        Ok(MessageRecord {
            id: row.get(0)?,
            session_id: row.get(1)?,
            role: row.get(2)?,
            seq: (row.get::<_, i64>(3)?).max(0) as usize,
            text_preview: row.get(4)?,
            full_text: row.get(5)?,
            created_at: row.get(6)?,
            tool_name: row.get(7)?,
        })
    }

    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchResult>> {
        // User input containing FTS5 syntax characters would make a raw MATCH
        // fail; always escape first.
        let escaped = Self::escape_fts_query(query);
        if escaped.is_empty() {
            return Ok(Vec::new());
        }
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT m.session_id, m.role, m.seq, m.text_preview, \
                        messages_fts.rank, \
                        snippet(messages_fts, 0, '[', ']', '…', 12) \
                 FROM messages_fts \
                 JOIN messages m ON m.id = messages_fts.rowid \
                 WHERE messages_fts MATCH ? \
                 ORDER BY messages_fts.rank \
                 LIMIT ?",
            )?;
            let rows = stmt.query_map(params![escaped, limit as i64], Self::map_search_row)?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        })
    }

    fn map_search_row(row: &rusqlite::Row) -> rusqlite::Result<SearchResult> {
        Ok(SearchResult {
            session_id: row.get(0)?,
            role: row.get(1)?,
            seq: (row.get::<_, i64>(2)?).max(0) as usize,
            text_preview: row.get(3)?,
            rank: row.get(4)?,
            snippet: row.get(5)?,
        })
    }

    /// Escape user input into an FTS5-safe MATCH query.
    ///
    /// In FTS5 MATCH syntax `-`, `:`, `"` and the `AND`/`OR`/`NOT` keywords
    /// are all syntax characters; raw user input would raise an SQL error.
    /// Strategy: split on whitespace, wrap each token in a double-quoted
    /// phrase (escaping embedded quotes) and join with spaces (FTS5 implicit
    /// AND). Pure syntax tokens without any alphanumeric character (e.g. a
    /// lone `-`) are dropped; when everything is dropped an empty string is
    /// returned and the caller skips the MATCH query entirely.
    fn escape_fts_query(query: &str) -> String {
        query
            .split_whitespace()
            .filter(|t| t.chars().any(|c| c.is_alphanumeric()))
            .map(|t| format!("\"{}\"", t.replace('"', "\"\"")))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

// ===== audit_log (with FTS5) =====

impl SessionDb {
    pub fn insert_audit_raw(
        &self,
        source: &str,
        actor: Option<&str>,
        event: &str,
        target: Option<&str>,
        detail: Option<&str>,
        ts: &str,
    ) -> Result<()> {
        self.with_conn(|conn| {
            conn.execute(
                "INSERT INTO audit_log (source, actor, event, target, detail, ts)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![source, actor, event, target, detail, ts],
            )?;
            Ok(())
        })
    }

    pub fn list_audit_raw(
        &self,
        source: Option<&str>,
        limit: usize,
    ) -> Result<
        Vec<(
            i64,
            String,
            String,
            Option<String>,
            Option<String>,
            Option<String>,
            String,
        )>,
    > {
        self.with_conn(|conn| {
            let mut stmt;
            let rows: rusqlite::Rows;
            if let Some(src) = source {
                stmt = conn.prepare(
                    "SELECT id, source, event, actor, target, detail, ts FROM audit_log WHERE source = ?1 ORDER BY ts DESC LIMIT ?2",
                )?;
                rows = stmt.query(params![src, limit as i64])?;
            } else {
                stmt = conn.prepare(
                    "SELECT id, source, event, actor, target, detail, ts FROM audit_log ORDER BY ts DESC LIMIT ?1",
                )?;
                rows = stmt.query(params![limit as i64])?;
            }
            let mut out = Vec::new();
            let mut rows = rows;
            while let Some(row) = rows.next()? {
                out.push((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?));
            }
            Ok(out)
        })
    }

    pub fn search_audit_raw(
        &self,
        query: &str,
        source: Option<&str>,
        limit: usize,
    ) -> Result<
        Vec<(
            i64,
            String,
            String,
            Option<String>,
            Option<String>,
            Option<String>,
            String,
        )>,
    > {
        // Same escaping discipline as `search`: raw MATCH on user input
        // breaks on FTS5 syntax characters.
        let escaped = Self::escape_fts_query(query);
        if escaped.is_empty() {
            return Ok(Vec::new());
        }
        self.with_conn(|conn| {
            let mut stmt;
            let rows: rusqlite::Rows;
            if let Some(src) = source {
                stmt = conn.prepare(
                    "SELECT a.id, a.source, a.event, a.actor, a.target, a.detail, a.ts
                     FROM audit_log_fts f JOIN audit_log a ON a.id = f.rowid
                     WHERE audit_log_fts MATCH ?1 AND a.source = ?2
                     ORDER BY f.rank LIMIT ?3",
                )?;
                rows = stmt.query(params![escaped, src, limit as i64])?;
            } else {
                stmt = conn.prepare(
                    "SELECT a.id, a.source, a.event, a.actor, a.target, a.detail, a.ts
                     FROM audit_log_fts f JOIN audit_log a ON a.id = f.rowid
                     WHERE audit_log_fts MATCH ?1
                     ORDER BY f.rank LIMIT ?2",
                )?;
                rows = stmt.query(params![escaped, limit as i64])?;
            }
            let mut out = Vec::new();
            let mut rows = rows;
            while let Some(row) = rows.next()? {
                out.push((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                ));
            }
            Ok(out)
        })
    }
}

// ===== Migration functions =====

fn migrate_v1(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "ALTER TABLE sessions ADD COLUMN title TEXT DEFAULT '';
         ALTER TABLE sessions ADD COLUMN parent_session_id TEXT;",
    )?;
    Ok(())
}

fn migrate_v2(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "ALTER TABLE messages ADD COLUMN tool_name TEXT;
         CREATE INDEX IF NOT EXISTS idx_messages_tool ON messages(tool_name);",
    )?;
    Ok(())
}

fn migrate_v3(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "ALTER TABLE sessions ADD COLUMN tags TEXT DEFAULT '[]';
         ALTER TABLE sessions ADD COLUMN is_pinned INTEGER NOT NULL DEFAULT 0;",
    )?;
    Ok(())
}

fn migrate_v4(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "ALTER TABLE sessions ADD COLUMN total_input_tokens INTEGER NOT NULL DEFAULT 0;
         ALTER TABLE sessions ADD COLUMN total_output_tokens INTEGER NOT NULL DEFAULT 0;",
    )?;
    Ok(())
}

fn migrate_v5(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "ALTER TABLE sessions ADD COLUMN workspace_id TEXT NOT NULL DEFAULT 'default';
         CREATE INDEX IF NOT EXISTS idx_sessions_workspace ON sessions(workspace_id);",
    )?;
    Ok(())
}

fn migrate_v6(conn: &Connection) -> Result<()> {
    let has_column: i64 = conn.query_row(
        "SELECT COUNT(*) FROM pragma_table_info('messages') WHERE name='full_text'",
        [],
        |row| row.get(0),
    )?;
    if has_column == 0 {
        conn.execute_batch("ALTER TABLE messages ADD COLUMN full_text TEXT;")?;
    }
    Ok(())
}

fn migrate_v7(conn: &Connection) -> Result<()> {
    let has_column: i64 = conn.query_row(
        "SELECT COUNT(*) FROM pragma_table_info('sessions') WHERE name='is_sub_agent'",
        [],
        |row| row.get(0),
    )?;
    if has_column == 0 {
        conn.execute_batch(
            "ALTER TABLE sessions ADD COLUMN is_sub_agent INTEGER NOT NULL DEFAULT 0;
             CREATE INDEX IF NOT EXISTS idx_sessions_parent ON sessions(parent_session_id);",
        )?;
    }
    Ok(())
}

/// v8: audit_log base table + FTS5 virtual table (external content on
/// `event` and `detail`, kept in sync by AFTER INSERT / AFTER DELETE
/// triggers).
fn migrate_v8_audit_log(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS audit_log (
            id      INTEGER PRIMARY KEY AUTOINCREMENT,
            source  TEXT NOT NULL,
            actor   TEXT,
            event   TEXT NOT NULL,
            target  TEXT,
            detail  TEXT,
            ts      TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_audit_source ON audit_log(source);
        CREATE INDEX IF NOT EXISTS idx_audit_ts ON audit_log(ts);

        CREATE VIRTUAL TABLE IF NOT EXISTS audit_log_fts USING fts5(
            event, detail,
            content='audit_log',
            content_rowid='id',
            tokenize='unicode61'
        );

        CREATE TRIGGER IF NOT EXISTS audit_log_ai AFTER INSERT ON audit_log BEGIN
            INSERT INTO audit_log_fts(rowid, event, detail) VALUES (new.id, new.event, new.detail);
        END;

        CREATE TRIGGER IF NOT EXISTS audit_log_ad AFTER DELETE ON audit_log BEGIN
            INSERT INTO audit_log_fts(audit_log_fts, rowid, event, detail) VALUES('delete', old.id, old.event, old.detail);
        END;",
    )?;
    Ok(())
}

/// Current UTC time as a fixed-width RFC 3339 string, taken from SQLite's
/// own clock so this crate needs no additional time dependency. The
/// fixed-width format keeps lexicographic ordering equal to chronological
/// ordering (relied upon by `ORDER BY updated_at DESC` queries).
fn now_rfc3339(conn: &Connection) -> String {
    conn.query_row("SELECT strftime('%Y-%m-%dT%H:%M:%fZ', 'now')", [], |row| {
        row.get(0)
    })
    .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    #[test]
    fn transcript_refresh_preserves_audit_and_flags_and_never_accumulates_messages() {
        use crate::transcript::{SessionMutationEvent, SessionRevertStagedV1};
        let home = tempfile::tempdir().unwrap();
        let meta = openpaw_core::SessionMeta::new("echo-1", "echo");
        let path = home
            .path()
            .join("sessions")
            .join(format!("{}.jsonl", meta.id));
        let mut writer = crate::SessionWriter::create(&path).unwrap();
        writer.append(&SessionEvent::Meta(meta.clone())).unwrap();
        let mut message = openpaw_core::Message::user_text("hidden cache sentinel");
        message.uuid = Some("user-id".into());
        writer.append(&SessionEvent::Message(message)).unwrap();
        let db = SessionDb::open(home.path().join("index.db")).unwrap();
        db.refresh_transcript(home.path(), &meta.id).unwrap();
        db.with_conn(|conn| {
            conn.execute_batch("INSERT INTO audit_log(source,actor,event,target,detail,ts) VALUES ('test','user','preserve','audit','sentinel','2026'); UPDATE sessions SET is_pinned=1,tags='[\"sentinel\"]',total_input_tokens=123;")?;
            Ok(())
        }).unwrap();
        let version = db.schema_version().unwrap();
        let before = std::fs::read(&path).unwrap();
        std::thread::scope(|scope| {
            for _ in 0..4 {
                let home = home.path();
                let id = &meta.id;
                scope.spawn(move || {
                    let db = SessionDb::open(home.join("index.db")).unwrap();
                    for _ in 0..4 {
                        db.refresh_transcript(home, id).unwrap();
                    }
                });
            }
        });
        assert_eq!(db.get_session_messages(&meta.id, 100).unwrap().len(), 1);
        assert_eq!(std::fs::read(&path).unwrap(), before);
        writer
            .append(
                &SessionMutationEvent::RevertStaged(SessionRevertStagedV1 {
                    message_id: "user-id".into(),
                })
                .to_session_event(),
            )
            .unwrap();
        db.refresh_transcript(home.path(), &meta.id).unwrap();
        assert!(db.search("sentinel", 10).unwrap().is_empty());
        writer
            .append(&SessionMutationEvent::RevertCleared.to_session_event())
            .unwrap();
        db.refresh_transcript(home.path(), &meta.id).unwrap();
        assert_eq!(db.search("sentinel", 10).unwrap().len(), 1);
        let corrupt =
            crate::transcript::encode(&serde_json::json!({"title":123}), "session_metadata_v1");
        writer.append(&corrupt).unwrap();
        assert!(db.refresh_transcript(home.path(), &meta.id).is_err());
        assert!(db.search("sentinel", 10).unwrap().is_empty());
        assert_eq!(db.schema_version().unwrap(), version);
        db.with_conn(|conn| {
            let count: i64 = conn.query_row(
                "SELECT COUNT(*) FROM audit_log WHERE event='preserve'",
                [],
                |r| r.get(0),
            )?;
            assert_eq!(count, 1);
            let fields: (i64, String, i64) = conn.query_row(
                "SELECT is_pinned,tags,total_input_tokens FROM sessions",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?;
            assert_eq!(fields, (1, "[\"sentinel\"]".into(), 123));
            Ok(())
        })
        .unwrap();
    }
    use super::*;
    use openpaw_core::Message;

    #[test]
    fn open_creates_schema() {
        let db = SessionDb::open_in_memory().unwrap();
        assert_eq!(db.get_session_count().unwrap(), 0);
    }

    #[test]
    fn upsert_and_list_sessions() {
        let db = SessionDb::open_in_memory().unwrap();
        let meta = SessionMeta::new("test-model", "test-provider");
        db.upsert_session(&meta, Path::new("test.jsonl")).unwrap();

        let sessions = db.list_sessions().unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, meta.id);
        assert_eq!(sessions[0].model, "test-model");
    }

    #[test]
    fn index_and_search_messages() {
        let db = SessionDb::open_in_memory().unwrap();
        let meta = SessionMeta::new("m", "p");
        db.upsert_session(&meta, Path::new("s.jsonl")).unwrap();

        db.index_message(&meta.id, "user", 0, "How do I use Rust?")
            .unwrap();
        db.index_message(
            &meta.id,
            "assistant",
            1,
            "Rust is a systems programming language...",
        )
        .unwrap();
        db.index_message(&meta.id, "user", 2, "What about Python?")
            .unwrap();

        db.touch_session(&meta.id).unwrap();

        let results = db.search("Rust", 10).unwrap();
        assert!(!results.is_empty());
        assert!(results.iter().any(|r| r.text_preview.contains("Rust")));
    }

    #[test]
    fn filter_sessions_by_model() {
        let db = SessionDb::open_in_memory().unwrap();
        let m1 = SessionMeta::new("model-a", "provider-x");
        let m2 = SessionMeta::new("model-b", "provider-x");
        db.upsert_session(&m1, Path::new("a.jsonl")).unwrap();
        db.upsert_session(&m2, Path::new("b.jsonl")).unwrap();

        let filtered = db.filter_sessions(Some("model-a"), None).unwrap();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].model, "model-a");
    }

    #[test]
    fn index_event_handles_messages() {
        let db = SessionDb::open_in_memory().unwrap();
        let meta = SessionMeta::new("m", "p");

        db.index_event(&meta.id, 0, &SessionEvent::Meta(meta.clone()))
            .unwrap();
        db.index_event(
            &meta.id,
            1,
            &SessionEvent::Message(Message::user_text("hello world")),
        )
        .unwrap();

        assert!(db.session_exists(&meta.id).unwrap());
        assert_eq!(db.get_message_count(&meta.id).unwrap(), 1);
    }

    #[test]
    fn schema_version_is_eight() {
        let db = SessionDb::open_in_memory().unwrap();
        let v = db.schema_version().unwrap();
        assert_eq!(
            v, 8,
            "base tables plus the audit trail should land on version 8"
        );
    }

    #[test]
    fn escape_fts_query_quotes_tokens() {
        assert_eq!(SessionDb::escape_fts_query("rust-lang"), "\"rust-lang\"");
        assert_eq!(
            SessionDb::escape_fts_query("say \"hi\" now"),
            "\"say\" \"\"\"hi\"\"\" \"now\""
        );
        // Pure syntax tokens (no letters/digits) are dropped; quoted keyword
        // tokens degenerate into literal phrases and stop acting as FTS5
        // boolean syntax.
        assert_eq!(SessionDb::escape_fts_query("- :"), "");
        assert_eq!(SessionDb::escape_fts_query("AND"), "\"AND\"");
        assert_eq!(SessionDb::escape_fts_query("   "), "");
    }

    #[test]
    fn add_session_tokens_accumulates() {
        let db = SessionDb::open_in_memory().unwrap();
        // A session row is created on demand when it does not exist yet.
        db.add_session_tokens("gw-session-1", 100, 50).unwrap();
        db.add_session_tokens("gw-session-1", 30, 20).unwrap();

        let (inp, out) = db.get_total_tokens().unwrap();
        assert_eq!(inp, 130, "input tokens should accumulate");
        assert_eq!(out, 70, "output tokens should accumulate");
    }
}
