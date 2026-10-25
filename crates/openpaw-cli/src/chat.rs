//! `openpaw chat` — the interactive conversation entry point (M2 wiring).
//!
//! Wires AgentLoop + ToolRegistry + session JSONL persistence. Offline by
//! default (mock/echo providers); real network providers plug into the
//! same `--provider` seam as they land.

use std::io::{BufRead, IsTerminal, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use openpaw_core::{Session, SessionEvent};
use openpaw_engine::{AgentEvent, AgentLoop, RunContext, RunObserver};

const EXIT_WORDS: [&str; 4] = ["exit", "quit", "/exit", "/quit"];

/// 用途：交互聊天装配参数
/// 字段：Provider、模型、端点、工作区和数据根
/// 来源去向：CLI到引擎
/// 约束：密钥只从服务端环境读取
/// 稳定性：新增可选项
/// 关联：需求/设计 REQ-OP-003。
#[derive(Debug, clap::Args)]
pub struct ChatArgs {
    /// 用途：显式Provider；缺省使用统一环境或原mock默认。
    /// 字段：可选Provider名称。
    /// 来源去向：CLI到原模型装配。
    /// 约束：未知或空值拒绝。
    /// 稳定性：保持原命令兼容。
    /// 关联：需求/设计REQ-OP-003；DES-OP-002。
    #[arg(long)]
    pub provider: Option<String>,
    /// 用途：选择共用模型配置文件。
    /// 字段：可选.env路径。
    /// 来源去向：CLI到ModelEnvironment。
    /// 约束：只读且不向工具环境注入。
    /// 稳定性：新增可选字段。
    /// 关联：需求/设计REQ-OP-003；DES-OP-002。
    #[arg(long)]
    pub env_file: Option<PathBuf>,
    /// 用途：显式覆盖Provider默认模型
    /// 字段：可选名称
    /// 来源去向：CLI到共享装配
    /// 约束：非空
    /// 稳定性：新增
    /// 关联：需求/设计 REQ-OP-003。
    #[arg(long)]
    pub model: Option<String>,
    /// 用途：已授权Provider端点
    /// 字段：可选URL
    /// 来源去向：CLI到Provider
    /// 约束：不包含密钥
    /// 稳定性：新增
    /// 关联：需求/设计 DES-OP-002。
    #[arg(long)]
    pub base_url: Option<String>,
    /// Workspace the tool host operates in (default: current directory).
    #[arg(long)]
    pub workspace: Option<PathBuf>,
    /// OpenPaw home for session storage (default: OPENPAW_HOME or
    /// ~/.openpaw).
    #[arg(long)]
    pub home: Option<PathBuf>,
}

/// 功能：用共享Provider装配、原引擎与可靠JSONL观察器运行聊天。
/// 参数：args为已解析的CLI配置。
/// 返回：成功为0，配置、运行或持久化失败为1。
/// 错误：错误写stderr；存储失败不自动重试工具。
/// 状态：保存完整用户/assistant/工具链/usage/终态，SQLite失败仅警告。
/// 并发与取消：每轮独立上下文，关键磁盘写通过spawn_blocking等待完成。
/// 权限：工具沿用现有工作区检查；不增加Office权限。
/// 关联：需求/设计 REQ-OP-003、REQ-OP-004；DES-OP-002、DES-OP-003。
pub fn run(args: ChatArgs) -> i32 {
    let workspace = args
        .workspace
        .clone()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

    // `--home` is the CLI's explicit data/config root. Resolve it before
    // loading the model environment so an isolated invocation cannot fall
    // through to the user's global ~/.openpaw/.env.
    let home = args.home.clone().unwrap_or_else(openpaw_core::openpaw_home);
    let settings = match openpaw_llm::model_config::ModelEnvironment::load(
        &home,
        args.env_file.as_deref(),
    ) {
        Ok(settings) => settings,
        Err(_) => {
            eprintln!("error: 模型.env读取失败。\nwhy: 文件缺失、不可读或格式错误，内容已隐藏。\nnext: 检查--env-file或OpenPaw数据目录的.env。");
            return 1;
        }
    };
    let (provider, model) = match settings.assemble(
        args.provider.as_deref(),
        args.model.as_deref(),
        args.base_url.as_deref(),
        None,
        "mock",
    ) {
        Ok(assembled) => assembled,
        Err(e) => {
            eprintln!("error: {e}\nwhy: provider configuration is not usable.\nnext: check --provider, --model and the provider API key environment variable.");
            return 1;
        }
    };

    let tool_host = openpaw_tools::arc_registry(&workspace);

    let mut session = Session::new(&model, provider.name());
    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("error: failed to start async runtime: {e}");
            return 1;
        }
    };

    // Declare the registered tools to the model so it can issue tool_use
    // calls; the ToolHost (moved into the agent below) executes them.
    // Offline providers (mock/echo) ignore the declaration and keep their
    // deterministic replies. Fetch once — the registry is static per run.
    let specs = match rt.block_on(tool_host.list_tools()) {
        Ok(specs) => specs,
        Err(e) => {
            eprintln!("error: cannot list tools: {e}");
            return 1;
        }
    };
    let agent = AgentLoop::new(provider.clone())
        .with_model(model)
        .with_tool_host(tool_host);

    // Session transcript: <home>/sessions/<id>.jsonl (plain files).
    let sessions_dir = home.join("sessions");
    let transcript_path = sessions_dir.join(format!("{}.jsonl", session.meta.id));
    let mut writer = match openpaw_store::SessionWriter::create(&transcript_path) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("error: cannot create session file: {e}\nwhy: the sessions directory must be writable.\nnext: check OPENPAW_HOME permissions.");
            return 1;
        }
    };
    if let Err(e) = writer.append(&SessionEvent::Meta(session.meta.clone())) {
        eprintln!("error: cannot write session meta: {e}");
        return 1;
    }

    // Best-effort SQLite index (`<home>/index.db`) alongside the JSONL
    // transcript: enables `openpaw session list/show/search`. Index
    // failures never abort the conversation; they only degrade search
    // coverage, so every write below is warned and ignored.
    let db_path = home.join("index.db");
    let session_db = match openpaw_store::sqlite::SessionDb::open(&db_path) {
        Ok(db) => Some(Arc::new(db)),
        Err(e) => {
            eprintln!(
                "warn: session index disabled (cannot open {}: {e})",
                db_path.display()
            );
            None
        }
    };
    if let Some(db) = &session_db {
        if let Err(e) = db.upsert_session(&session.meta, &transcript_path) {
            eprintln!("warn: failed to index session meta: {e}");
        }
    }

    let mut transcript = Transcript {
        writer: Arc::new(Mutex::new(writer)),
        db: session_db,
        session_id: session.meta.id.clone(),
        seq: 1,
    };

    let interactive = std::io::stdin().is_terminal();
    if interactive {
        println!(
            "OpenPaw chat — provider `{}`, workspace `{}`. Type 'exit' to quit.",
            provider.name(),
            workspace.display()
        );
        println!("session: {}", transcript_path.display());
    }

    let stdin = std::io::stdin();
    let mut out = std::io::stdout();

    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if EXIT_WORDS.contains(&trimmed.to_ascii_lowercase().as_str()) {
            break;
        }

        let user_msg = openpaw_core::Message::user_text(trimmed.to_string());
        if let Err(e) = rt.block_on(transcript.record(SessionEvent::Message(user_msg.clone()))) {
            eprintln!("error: cannot persist user message: {e}");
            return 1;
        }
        session.messages.push(user_msg);

        let log: Arc<Mutex<Vec<AgentEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let sink_log = Arc::clone(&log);
        let mut sink = move |ev: AgentEvent| {
            sink_log.lock().unwrap_or_else(|e| e.into_inner()).push(ev);
        };

        let outcome = rt.block_on(agent.run_with_context(
            &mut session,
            None,
            &specs,
            &RunContext::default(),
            &mut transcript,
            &mut sink,
        ));
        if let Some(e) = outcome.error {
            eprintln!("error: {e}\nwhy: the agent loop or transcript persistence failed during this turn.\nnext: inspect the saved session before retrying; tools may already have performed side effects.");
            return 1;
        }

        let events = log.lock().unwrap_or_else(|e| e.into_inner());
        for ev in events.iter() {
            if let AgentEvent::ToolResult {
                name,
                is_error,
                content,
                ..
            } = ev
            {
                if interactive {
                    let mark = if *is_error { "!" } else { "·" };
                    let preview: String = content.chars().take(80).collect();
                    writeln!(
                        out,
                        "  {mark} {} — {}",
                        name.clone().unwrap_or_default(),
                        preview
                    )
                    .ok();
                }
            }
        }
        let reply = session
            .messages
            .last()
            .filter(|m| m.role == openpaw_core::Role::Assistant)
            .map(|m| m.text())
            .unwrap_or_default();
        if interactive {
            writeln!(out, "Paw: {reply}").ok();
        } else {
            writeln!(out, "{reply}").ok();
        }
        let _ = out.flush();
    }

    if interactive {
        println!("bye. session saved to {}", transcript_path.display());
    }
    0
}

// JSONL是事实来源；同步磁盘写在专用阻塞任务内完成，异步观察边界等待结果。
// SQLite失败只警告，不能使已成功的JSONL事实丢失或重新执行工具。
struct Transcript {
    writer: Arc<Mutex<openpaw_store::SessionWriter>>,
    db: Option<Arc<openpaw_store::SessionDb>>,
    session_id: String,
    seq: usize,
}
#[async_trait]
impl RunObserver for Transcript {
    async fn record(&mut self, event: SessionEvent) -> openpaw_core::Result<()> {
        let writer = self.writer.clone();
        let db = self.db.clone();
        let session_id = self.session_id.clone();
        let seq = self.seq;
        tokio::task::spawn_blocking(move || -> openpaw_core::Result<()> {
            writer.lock().map_err(|_| openpaw_core::Error::Store("transcript writer poisoned".into()))?
                .append(&event).map_err(|e| openpaw_core::Error::Store(e.to_string()))?;
            if let Some(db) = db {
                if let Err(e) = db.index_event(&session_id, seq, &event) { eprintln!("warn: failed to index event: {e}"); }
                if matches!(&event, SessionEvent::Message(msg) if msg.role == openpaw_core::Role::User && msg.content.iter().any(|block| matches!(block, openpaw_core::ContentBlock::Text { .. }))) {
                    if let Err(e) = db.touch_session(&session_id) { eprintln!("warn: failed to update session activity: {e}"); }
                }
            }
            Ok(())
        }).await.map_err(|e| openpaw_core::Error::Store(format!("transcript task failed: {e}")))??;
        self.seq += 1;
        Ok(())
    }
}
