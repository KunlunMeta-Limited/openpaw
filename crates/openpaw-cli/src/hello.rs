//! `openpaw hello` — zero-config echo conversation (M1 exit demo).
//!
//! Interactive mode shows a small banner and a prompt; piped (non-tty)
//! mode processes every line without prompts so scripts and CI can drive
//! it. First response latency is effectively zero: the echo provider does
//! no network I/O (CI asserts < 30 s end-to-end).

use std::io::{BufRead, IsTerminal, Write};

use openpaw_core::{CompletionRequest, LlmProvider, Message};
use openpaw_llm::EchoProvider;

const EXIT_WORDS: [&str; 4] = ["exit", "quit", "/exit", "/quit"];

pub fn run() -> i32 {
    let interactive = std::io::stdin().is_terminal();
    let provider = EchoProvider::new();
    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("error: failed to start async runtime: {e}");
            return 1;
        }
    };

    if interactive {
        println!("OpenPaw hello — echo model. Type 'exit' to quit.");
    }

    let stdin = std::io::stdin();
    let mut out = std::io::stdout();
    let mut session = Vec::new();

    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if EXIT_WORDS.contains(&trimmed.to_ascii_lowercase().as_str()) {
            break;
        }

        let user = Message::user_text(trimmed.to_string());
        session.push(user);

        let req = CompletionRequest {
            model: provider.default_model().to_string(),
            messages: session.clone(),
            ..Default::default()
        };
        match rt.block_on(provider.complete(req)) {
            Ok(resp) => {
                let reply = resp.text();
                if interactive {
                    writeln!(out, "Paw: {reply}").ok();
                } else {
                    writeln!(out, "{reply}").ok();
                }
                let _ = out.flush();
                session.push(Message::assistant_text(reply));
            }
            Err(e) => {
                // Three-segment error shape: what / why / next step.
                eprintln!("error: {e}");
                eprintln!("why: the echo provider failed to build a response.");
                eprintln!(
                    "next: re-run `openpaw hello`; if this persists, report the message above."
                );
                return 1;
            }
        }
    }

    if interactive {
        println!("bye.");
    }
    0
}
