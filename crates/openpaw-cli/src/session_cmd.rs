//! `openpaw session ...`：通过共享持久目录列出、显示和搜索会话。
//!
//! JSONL 是事实源；list/show 无需预建 SQLite 索引。search 先按会话刷新
//! `<home>/index.db` 的可见消息/FTS 缓存，chat 也尽力同步该缓存。
//! 查询只读取已提交的可见历史，不恢复活动回合、不重写 JSONL；关联 CHG-2026-0023。

use std::path::Path;

use openpaw_core::Role;
use openpaw_store::sqlite::SessionDb;

/// `openpaw session` 的持久会话查询子命令。
#[derive(Debug, clap::Subcommand)]
pub enum Command {
    /// 列出持久目录中的近期会话，无需预建索引。
    List {
        /// Maximum number of sessions to print.
        #[arg(long, default_value_t = 20)]
        limit: usize,
        /// OpenPaw home (default: OPENPAW_HOME or ~/.openpaw).
        #[arg(long)]
        home: Option<std::path::PathBuf>,
    },
    /// Print an ordered message digest for one session id.
    Show {
        /// Session id as shown by `openpaw session list`.
        id: String,
        /// OpenPaw home (default: OPENPAW_HOME or ~/.openpaw).
        #[arg(long)]
        home: Option<std::path::PathBuf>,
    },
    /// 刷新逐会话索引后，全文搜索当前可见消息。
    Search {
        /// Search query (tokens are matched via FTS5).
        query: String,
        /// Maximum number of hits to print.
        #[arg(long, default_value_t = 20)]
        limit: usize,
        /// OpenPaw home (default: OPENPAW_HOME or ~/.openpaw).
        #[arg(long)]
        home: Option<std::path::PathBuf>,
    },
}

/// Print a three-segment error (what happened / why / what to do next).
fn report_error(what: String, why: &str, next: &str) {
    eprintln!("error: {what}");
    eprintln!("why: {why}");
    eprintln!("next: {next}");
}

/// Truncate to `n` chars for tabular output; newlines become spaces.
fn truncate(s: &str, n: usize) -> String {
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i >= n {
            out.push('…');
            break;
        }
        if c == '\n' {
            out.push(' ');
        } else {
            out.push(c);
        }
    }
    out
}

/// 功能：从持久目录列出会话，不要求预建 SQLite 索引。
/// 参数：home、结果上限。
/// 返回：退出码。
/// 错误：读取错误隔离并返回 1。
/// 状态：不写 JSONL。
/// 并发与取消：读取当次合法提交前缀。
/// 权限：只访问 home/sessions。
/// 关联：共享 catalog。
pub fn run_list(home: &Path, limit: usize) -> i32 {
    let ids = match openpaw_store::catalog::discover(home) {
        Ok(ids) => ids,
        Err(e) => return catalog_error(e),
    };
    let mut snapshots = Vec::new();
    let mut exit = 0;
    for id in ids {
        match openpaw_store::catalog::read(home, &id) {
            Ok(snapshot) => snapshots.push(snapshot),
            Err(error) => {
                catalog_error(error);
                exit = 1;
            }
        }
    }
    snapshots.sort_by_key(|s| std::cmp::Reverse(s.updated_at));
    if snapshots.is_empty() {
        println!("(no sessions)");
    } else {
        println!(
            "{:<34} {:<26} {:<20} {:>8}",
            "id", "title", "updated", "turns"
        );
    }
    for s in snapshots.iter().take(limit) {
        let updated = s
            .updated_at
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        println!(
            "{:<34} {:<26} {:<20} {:>8}",
            truncate(&s.meta.id, 32),
            truncate(&s.transcript.title(), 24),
            updated,
            s.transcript
                .visible_messages(&s.meta.id)
                .iter()
                .filter(|m| m.role == Role::User)
                .count()
        );
    }
    exit
}

/// 功能：显示当前已提交可见历史。
/// 参数：home 和会话 ID。
/// 返回：退出码。
/// 错误：缺失或损坏返回 1，禁止降级遍历原始消息。
/// 状态：不写 JSONL 或恢复活动回合。
/// 并发与取消：当次快照。
/// 权限：不采用 SQLite 中任意文件路径。
/// 关联：共享 transcript。
pub fn run_show(home: &Path, id: &str) -> i32 {
    let s = match openpaw_store::catalog::read(home, id) {
        Ok(s) => s,
        Err(e) => return catalog_error(e),
    };
    println!(
        "# {}\nmodel:    {}\nprovider: {}\ncreated:  {}\nfile:     {}\ntitle:    {}",
        s.meta.id,
        s.meta.model,
        s.meta.provider,
        s.meta.created_at.to_rfc3339(),
        s.path.display(),
        s.transcript.title()
    );
    if s.transcript.mutation.archived.is_some() {
        println!("archived: true");
    }
    if s.torn_tail_lines > 0 {
        println!(
            "(warning: {} torn tail line(s) from an interrupted write)",
            s.torn_tail_lines
        );
    }
    for (index, message) in s.transcript.visible_messages(id).iter().enumerate() {
        let role = if message.role == Role::User {
            "user"
        } else {
            "assistant"
        };
        println!(
            "{:>3}. [{role:>9}] {}",
            index + 1,
            truncate(&message.text(), 100)
        );
        for media in openpaw_core::media::message_media(std::slice::from_ref(message)) {
            println!(
                "     attachment {} {} {} ({} bytes)",
                media.id,
                media.mime,
                media.filename.as_deref().unwrap_or(""),
                media.bytes().map(|bytes| bytes.len()).unwrap_or(0)
            );
        }
        for block in &message.content {
            if let openpaw_core::ContentBlock::ToolResult { name, is_error, .. } = block {
                println!(
                    "     [{}] {}",
                    if *is_error { "tool err" } else { "tool" },
                    name.as_deref().unwrap_or("tool")
                );
            }
        }
    }
    0
}

/// 功能：刷新可见索引后全文搜索有效目录中的会话。
/// 参数：home、查询和上限。
/// 返回：退出码。
/// 错误：损坏会话隔离并失效缓存，索引失败返回 1。
/// 状态：只更新逐会话 SQLite 缓存。
/// 并发与取消：刷新先取写事务再读快照。
/// 权限：不显示外部或损坏会话的陈旧命中。
/// 关联：SessionDb::refresh_transcript。
pub fn run_search(home: &Path, query: &str, limit: usize) -> i32 {
    let ids = match openpaw_store::catalog::discover(home) {
        Ok(ids) => ids,
        Err(e) => return catalog_error(e),
    };
    if ids.is_empty() {
        println!("(no results)");
        return 0;
    }
    let db = match SessionDb::open(home.join("index.db")) {
        Ok(db) => db,
        Err(_) => {
            report_error(
                "cannot open session index".into(),
                "index.db must be writable SQLite",
                "check permissions; preserve audit and other data when repairing",
            );
            return 1;
        }
    };
    let mut valid = std::collections::HashSet::new();
    let mut exit = 0;
    for id in ids {
        match db.refresh_transcript(home, &id) {
            Ok(_) => {
                valid.insert(id);
            }
            Err(e) => {
                catalog_error(e);
                exit = 1;
            }
        }
    }
    let results = match db.search(query, i64::MAX as usize) {
        Ok(results) => results
            .into_iter()
            .filter(|r| valid.contains(&r.session_id))
            .take(limit)
            .collect::<Vec<_>>(),
        Err(_) => {
            report_error(
                "search failed".into(),
                "full-text index query failed",
                "inspect the index without deleting audit data",
            );
            return 1;
        }
    };
    if results.is_empty() {
        println!("(no results for \"{query}\")");
    }
    for r in &results {
        let text = if r.snippet.is_empty() {
            &r.text_preview
        } else {
            &r.snippet
        };
        println!(
            "[{}:{}] score={:.3} {}",
            r.session_id,
            r.seq,
            -r.rank,
            truncate(text, 120)
        );
    }
    println!("({} result(s))", results.len());
    exit
}
fn catalog_error(error: openpaw_store::catalog::CatalogError) -> i32 {
    report_error(
        error.to_string(),
        "session JSONL is the source of truth",
        "check home/sessions permissions and records; preserve index.db audit data",
    );
    1
}
