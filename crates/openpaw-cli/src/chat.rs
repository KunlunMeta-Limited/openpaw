//! `openpaw chat` — the interactive conversation entry point (M2 wiring).
//!
//! Wires AgentLoop + ToolRegistry + session JSONL persistence. Offline by
//! default (mock/echo providers); real network providers plug into the
//! same `--provider` seam as they land.

use std::io::{BufRead, IsTerminal, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use openpaw_core::{Session, SessionEvent};
use openpaw_engine::{AgentEvent, AgentLoop};

const EXIT_WORDS: [&str; 4] = ["exit", "quit", "/exit", "/quit"];

/// Resolved values for one `openpaw chat` run. The values come from the
/// global `--provider` / `--workspace` / `--home` flags (`main.rs` applies
/// the defaults), so this struct keeps no clap attributes.
#[derive(Debug)]
pub struct ChatArgs {
    /// Offline provider: `mock` (counter replies) or `echo`. Real network
    /// providers plug into this seam as they land.
    pub provider: String,
    /// Workspace the tool host operates in (default: current directory).
    pub workspace: Option<PathBuf>,
    /// OpenPaw home for session storage (default: OPENPAW_HOME or
    /// ~/.openpaw).
    pub home: Option<PathBuf>,
}

pub fn run(args: ChatArgs) -> i32 {
    let workspace = args
        .workspace
        .clone()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

    let provider: Arc<dyn openpaw_core::LlmProvider> = match args.provider.as_str() {
        "echo" => Arc::new(openpaw_llm::EchoProvider::new()),
        "mock" => Arc::new(openpaw_llm::MockProvider::counter("mock-reply-")),
        "anthropic" => match std::env::var("ANTHROPIC_API_KEY") {
            Ok(key) if !key.is_empty() => {
                Arc::new(openpaw_llm::AnthropicProvider::new(key, "claude-sonnet-4"))
            }
            _ => {
                eprintln!(
                    "error: ANTHROPIC_API_KEY is not set\nwhy: the anthropic provider reads its key from the environment (keys never touch disk).\nnext: export ANTHROPIC_API_KEY and re-run."
                );
                return 1;
            }
        },
        "openai" => match std::env::var("OPENAI_API_KEY") {
            Ok(key) if !key.is_empty() => Arc::new(openpaw_llm::OpenAiProvider::new(key, "gpt-4o")),
            _ => {
                eprintln!(
                    "error: OPENAI_API_KEY is not set\nwhy: the openai provider reads its key from the environment (keys never touch disk).\nnext: export OPENAI_API_KEY and re-run."
                );
                return 1;
            }
        },
        other => {
            eprintln!(
                "error: unknown provider `{other}`\nwhy: chat supports `mock`, `echo`, `anthropic`, and `openai`.\nnext: re-run with one of those via `--provider`."
            );
            return 1;
        }
    };

    let tool_host = openpaw_tools::arc_registry(&workspace);

    let mut session = Session::new(provider.default_model(), provider.name());
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
    let agent = AgentLoop::new(provider.clone()).with_tool_host(tool_host);

    // Session transcript: <home>/sessions/<id>.jsonl (plain files).
    let home = args.home.clone().unwrap_or_else(openpaw_core::openpaw_home);
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
        Ok(db) => Some(db),
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
        let pre_len = session.messages.len();
        session.messages.push(user_msg.clone());
        let _ = writer.append(&SessionEvent::Message(user_msg.clone()));
        if let Some(db) = &session_db {
            if let Err(e) =
                db.index_event(&session.meta.id, pre_len, &SessionEvent::Message(user_msg))
            {
                eprintln!("warn: failed to index message: {e}");
            }
            if let Err(e) = db.touch_session(&session.meta.id) {
                eprintln!("warn: failed to update session activity: {e}");
            }
        }

        let log: Arc<Mutex<Vec<AgentEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let sink_log = Arc::clone(&log);
        let mut sink = move |ev: AgentEvent| {
            sink_log.lock().unwrap_or_else(|e| e.into_inner()).push(ev);
        };

        let stop = match rt.block_on(agent.run_turn(&mut session, None, &specs, &mut sink)) {
            Ok(stop) => stop,
            Err(e) => {
                // Three-segment error shape: what / why / next step.
                eprintln!("error: {e}");
                eprintln!("why: the agent loop failed during this turn.");
                eprintln!("next: re-run the message; if it persists, report the error above.");
                return 1;
            }
        };

        // Persist the new messages and print a compact turn summary.
        // The user message was already appended (and indexed) when it was
        // entered above — appending it again here would duplicate the event
        // in the transcript. Only this turn's produced messages land here.
        for (i, msg) in session.messages[pre_len..].iter().enumerate() {
            if matches!(msg.role, openpaw_core::Role::User) {
                continue;
            }
            let _ = writer.append(&SessionEvent::Message(msg.clone()));
            if let Some(db) = &session_db {
                if let Err(e) = db.index_event(
                    &session.meta.id,
                    pre_len + i,
                    &SessionEvent::Message(msg.clone()),
                ) {
                    eprintln!("warn: failed to index message: {e}");
                }
            }
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
                let _ = writer.append(&SessionEvent::ToolCallComplete {
                    call_id: String::new(),
                    tool_name: name.clone().unwrap_or_default(),
                    success: !*is_error,
                    duration_ms: None,
                });
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
        let _ = stop; // end_turn/tool_use both simply continue to the next prompt
    }

    if interactive {
        println!("bye. session saved to {}", transcript_path.display());
    }
    0
}
