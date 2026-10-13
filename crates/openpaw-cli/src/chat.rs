//! `openpaw chat`：组装共享模型配置、AgentLoop、工具、审批和 JSONL 持久化。
//!
//! 默认 mock 离线运行；OpenAI/Anthropic 兼容服务支持自定义地址和模型，密钥
//! 从环境读取。终端输入由单一所有者路由，人工审批沿用共享 gate 和工具限制。
//! 执行及故障回滚复用 Engine，消息事务复用 Store；本入口每次创建新会话。
//! Host HTTP/Office 适配不进入终端交互；关联 CHG-2026-0023、CHG-2026-0025。

use std::io::{IsTerminal, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use openpaw_core::{Session, SessionEvent};
use openpaw_engine::turn_execution::{self, TurnOptions};
use openpaw_engine::{AgentEvent, AgentLoop};
use openpaw_store::journal;

const EXIT_WORDS: [&str; 4] = ["exit", "quit", "/exit", "/quit"];

use openpaw_cli::cli::ChatArgs;

/// 功能：执行交互或管道聊天。
/// 参数：ChatArgs。
/// 返回：退出码。
/// 错误：配置 2，运行和持久化 1。
/// 状态：JSONL 会话。
/// 并发与取消：AgentLoop 管理回合取消。
/// 权限：工具仍遵守工作区策略。
/// 关联：共享 LLM 配置。
pub fn run(args: ChatArgs) -> i32 {
    let workspace = args
        .workspace
        .clone()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

    let config = match openpaw_llm::config::ConfigSnapshot::from_env().resolve(
        &args.provider,
        openpaw_llm::config::Overrides {
            base_url: args.base_url.clone(),
            model: args.model.clone(),
            max_output_tokens: args.max_output_tokens.clone(),
        },
    ) {
        Ok(config) => config,
        Err(error) => {
            eprintln!("error: {error}\nwhy: provider configuration is invalid.\nnext: check provider, base URL, model, API key environment and output token settings.");
            return 2;
        }
    };
    let provider = config.build();

    let tool_host = openpaw_tools::arc_registry(&workspace);

    let mut session = Session::new(&config.model, provider.name());
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
    let specs = rt.block_on(tool_host.list_tools()).unwrap_or_default();
    let agent = AgentLoop::new(provider.clone())
        .with_model(&config.model)
        .with_max_tokens(config.max_output_tokens)
        .with_tool_host(tool_host);

    let (approval_tx, mut approval_rx) = tokio::sync::mpsc::unbounded_channel();
    let next_id = std::sync::atomic::AtomicU64::new(0);
    let approvals = openpaw_engine::approval::ApprovalGate::new(
        Default::default(),
        move || {
            format!(
                "cli-{}",
                next_id.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            )
        },
        move |notice| {
            let _ = approval_tx.send(notice);
        },
    );
    let agent =
        openpaw_engine::approval::configure(agent, args.permissions == "manual", approvals.clone());

    // Session transcript: <home>/sessions/<id>.jsonl (plain files).
    let home = args.home.clone().unwrap_or_else(openpaw_core::openpaw_home);
    let sessions_dir = home.join("sessions");
    let transcript_path = sessions_dir.join(format!("{}.jsonl", session.meta.id));
    let mut writer = Some(
        match openpaw_store::SessionWriter::create(&transcript_path) {
            Ok(w) => w,
            Err(e) => {
                eprintln!("error: cannot create session file: {e}\nwhy: the sessions directory must be writable.\nnext: check OPENPAW_HOME permissions.");
                return 1;
            }
        },
    );
    if let Err(e) = journal::append(&mut writer, &SessionEvent::Meta(session.meta.clone())) {
        eprintln!("error: cannot write session meta: {e}");
        return 1;
    }

    // SQLite 仅缓存可见消息并支持全文搜索；list/show 直接读取共享 JSONL 目录。
    // 索引失败只警告，不能终止聊天或建议删除包含审计数据的整个数据库。
    let db_path = home.join("index.db");
    let session_db = match openpaw_store::sqlite::SessionDb::open(&db_path) {
        Ok(db) => Some(db),
        Err(e) => {
            eprintln!(
                "warn: session index disabled (cannot open {}: {e})",
                db_path.display()
            );
            None
        }
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

    let input = openpaw_cli::approval::InputOwner::new(std::io::BufReader::new(std::io::stdin()));
    let mut out = std::io::stdout();

    while let Some(line) = rt.block_on(input.read_line()) {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if EXIT_WORDS.contains(&trimmed.to_ascii_lowercase().as_str()) {
            break;
        }

        let user_msg = openpaw_core::Message::user_text(trimmed.to_string());
        let pre_len = session.messages.len();
        // The transcript is the durable conversation record: a failed append
        // means the session on disk no longer matches this run, so the
        // command must fail loudly instead of returning 0.
        if let Err(error) = journal::append(&mut writer, &SessionEvent::Message(user_msg.clone())) {
            return report_persistence_error("persist the user message", &transcript_path, &error);
        }
        session.messages.push(user_msg);
        let log: Arc<Mutex<Vec<AgentEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let sink_log = Arc::clone(&log);
        let mut sink = move |ev: AgentEvent| {
            sink_log.lock().unwrap_or_else(|e| e.into_inner()).push(ev);
        };

        approvals.begin_turn();
        let execution = rt.block_on(async {
            let turn = turn_execution::run_turn(&agent, &mut session, &specs, &mut sink, TurnOptions::default());
            tokio::pin!(turn);
            loop {
                tokio::select! {
                    result = &mut turn => break result,
                    Some(notice) = approval_rx.recv() => {
                        if let openpaw_engine::approval::ApprovalNotice::Asked { id, request } = notice {
                            let answer = openpaw_cli::approval::ask(&input, &mut std::io::stderr(), interactive, &request.tool_name).await;
                            use openpaw_cli::approval::Answer;
                            approvals.reply(&id, answer != Answer::Reject, answer == Answer::Session, "terminal approval decision".into());
                        }
                    }
                }
            }
        });
        approvals.abort_all();
        let checkpoint = execution.checkpoint;
        let stop = match execution.result {
            Ok(stop) => stop,
            Err(e) => {
                // Three-segment error shape: what / why / next step.
                eprintln!("error: {e}");
                eprintln!("why: the agent loop failed during this turn.");
                eprintln!("next: re-run the message; if it persists, report the error above.");
                return 1;
            }
        };

        // 原始输入已提交；产出可以包含 User 角色的工具结果，必须完整保存。
        let events = log.lock().unwrap_or_else(|e| e.into_inner());
        let completions: Vec<_> = events
            .iter()
            .filter_map(|event| {
                if let AgentEvent::ToolResult { name, is_error, .. } = event {
                    Some(SessionEvent::ToolCallComplete {
                        call_id: String::new(),
                        tool_name: name.clone().unwrap_or_default(),
                        success: !*is_error,
                        duration_ms: None,
                    })
                } else {
                    None
                }
            })
            .collect();
        let persisted = session
            .messages
            .get(pre_len + 1..)
            .ok_or_else(|| "会话历史的持久化边界不一致".to_string())
            .and_then(|messages| journal::append_messages(&mut writer, messages, &completions));
        if let Err(error) = persisted {
            checkpoint.restore(&mut session);
            return report_persistence_error(
                "persist the generated messages",
                &transcript_path,
                &error,
            );
        }
        if let Some(db) = &session_db {
            if let Err(error) = db.refresh_transcript(&home, &session.meta.id) {
                eprintln!("warn: {error}");
            }
        }
        if interactive {
            for event in events.iter() {
                if let AgentEvent::ToolResult {
                    name,
                    is_error,
                    content,
                    ..
                } = event
                {
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
        let _ = stop; // end_turn/tool_use both simply continue to the next prompt
    }

    if interactive {
        println!("bye. session saved to {}", transcript_path.display());
    }
    0
}

/// 功能：报告 transcript 写入失败并返回非零退出码。
/// 参数：`operation` 是失败的写入动作，`path` 是 session 文件，`error` 是存储错误。
/// 返回：失败操作固定返回 1。
/// 安全：只输出路径与错误文本，不输出消息正文。
fn report_persistence_error(operation: &str, path: &std::path::Path, error: &str) -> i32 {
    eprintln!("error: failed to {operation}: {error}");
    eprintln!(
        "why: the session file {} could not be written completely.",
        path.display()
    );
    eprintln!("next: check disk space and permissions under OPENPAW_HOME, then re-run.");
    1
}
