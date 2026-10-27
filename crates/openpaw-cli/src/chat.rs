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
    let home = args.home.clone().unwrap_or_else(openpaw_core::openpaw_home);
    let original_path = home
        .join("sessions")
        .join(format!("{}.jsonl", session.meta.id));
    let owner = session.meta.id.clone();
    let reader: openpaw_engine::context::OriginalReader = Arc::new(move |reference| {
        let key = openpaw_engine::context::parse_reference(reference, &owner)?;
        let read =
            openpaw_store::SessionReader::read(&original_path).map_err(|_| "原件记录不可读")?;
        let history =
            openpaw_store::transcript::replay(&owner, &read.events).map_err(|_| "原件历史无效")?;
        if key.turn.starts_with("message/") {
            return openpaw_engine::context::message_original(
                &key,
                history.visible_messages(&owner),
            );
        }
        history
            .tool_original(&key.turn, key.response, &key.call)
            .ok_or_else(|| "原件不存在或未保存".into())
    });
    let tool_host = openpaw_engine::context::wrap_tools(
        tool_host,
        reader,
        provider.model_capabilities(&config.model).context_window as usize,
    );
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
    let caps = provider.model_capabilities(&config.model);
    let mut pending = Vec::new();
    for path in &args.attach {
        match rt.block_on(openpaw_tools::fs::media::read_media(path)) {
            Ok(media) => pending.push(media),
            Err(error) => {
                eprintln!("error: {error}");
                return 2;
            }
        }
    }
    if let Err(error) = openpaw_core::media::validate_media(&pending, caps) {
        eprintln!("error: {error}");
        return 2;
    }
    let mut original_history = Vec::new();
    let agent = AgentLoop::new(provider.clone())
        .with_model(&config.model)
        .with_max_tokens(config.max_output_tokens)
        .with_tool_host(tool_host)
        .with_compact_options(openpaw_engine::compaction::CompactOptions {
            model_limit: caps.context_window as usize,
            protect_head_pairs: 0,
            keep_recent_pairs: 1,
            ..Default::default()
        });

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

    let writer = Arc::new(Mutex::new(writer));
    let mut turn_number = 0u64;

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

        if trimmed == "/clear-attachments" {
            pending.clear();
            continue;
        }
        if let Some(path) = trimmed.strip_prefix("/attach ") {
            match rt.block_on(openpaw_tools::fs::media::read_media(std::path::Path::new(
                path.trim().trim_matches('"'),
            ))) {
                Ok(media) => {
                    let mut candidate = pending.clone();
                    candidate.push(media);
                    match openpaw_core::media::validate_media(&candidate, caps) {
                        Ok(()) => pending = candidate,
                        Err(error) => eprintln!("error: {error}"),
                    }
                }
                Err(error) => eprintln!("error: {error}"),
            }
            continue;
        }
        if trimmed == "/attach" {
            eprintln!("usage: /attach <path>");
            continue;
        }
        if trimmed == "/send" && pending.is_empty() {
            eprintln!("no pending attachments");
            continue;
        }

        let mut user_msg = if trimmed == "/send" {
            openpaw_core::Message::default()
        } else {
            openpaw_core::Message::user_text(trimmed.to_string())
        };
        user_msg
            .content
            .extend(pending.iter().map(openpaw_core::InlineMedia::block));
        user_msg = user_msg.identified();
        // The transcript is the durable conversation record: a failed append
        // means the session on disk no longer matches this run, so the
        // command must fail loudly instead of returning 0.
        if let Err(error) = journal::append(
            &mut writer.lock().unwrap_or_else(|e| e.into_inner()),
            &SessionEvent::Message(user_msg.clone()),
        ) {
            return report_persistence_error("persist the user message", &transcript_path, &error);
        }
        original_history.push(user_msg.clone());
        session.messages.push(user_msg);
        pending.clear();
        let log: Arc<Mutex<Vec<AgentEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let sink_log = Arc::clone(&log);
        let mut sink = move |ev: AgentEvent| {
            sink_log.lock().unwrap_or_else(|e| e.into_inner()).push(ev);
        };

        turn_number += 1;
        let recorder_writer = writer.clone();
        let turn_id = format!("turn_{turn_number}");
        let context_turn_id = turn_id.clone();
        let sequence = std::sync::atomic::AtomicU64::new(0);
        let recorder: turn_execution::ExecutionRecorder = Arc::new(move |observation| {
            let record = openpaw_store::execution_attempts::ExecutionAttemptV1 {
                turn_id: turn_id.clone(),
                sequence: sequence.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1,
                response_index: observation.response_index,
                occurred_at: i64::try_from(
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis(),
                )
                .unwrap_or(i64::MAX),
                kind: observation.kind,
                payload: observation.payload,
            };
            journal::append(
                &mut recorder_writer.lock().unwrap_or_else(|e| e.into_inner()),
                &openpaw_store::transcript::encode(&record, "execution_attempt_v1"),
            )
            .map_err(|_| openpaw_core::Error::session("execution record persistence failed"))
        });
        approvals.begin_turn();
        let execution = rt.block_on(async {
            let attachments = openpaw_store::media::attachment_snapshot(&original_history).into_values().collect();
            let turn = turn_execution::run_turn(&agent, &mut session, &specs, &mut sink, TurnOptions { context_turn_id: Some(context_turn_id), attachments, recorder:Some(recorder), ..Default::default() });
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
        let mut committed = original_history.clone();
        committed.extend(execution.generated_messages.iter().cloned());
        let context: Vec<_> = openpaw_engine::context::seal_context(&mut session, &committed)
            .into_iter()
            .collect();
        let persisted = journal::append_messages(
            &mut writer.lock().unwrap_or_else(|e| e.into_inner()),
            &execution.generated_messages,
            &context,
        );
        if let Err(error) = persisted {
            checkpoint.restore(&mut session);
            return report_persistence_error(
                "persist the generated messages",
                &transcript_path,
                &error,
            );
        }
        original_history.extend(execution.generated_messages);
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
