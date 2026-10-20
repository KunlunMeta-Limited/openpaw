#![cfg(feature = "sidecar")]
//! E2E: `openpaw --sidecar --sidecar-transport stdio` — the MCP sidecar
//! driven with raw JSON-RPC frames through the child process pipes.
//!
//! The test deliberately does not use the rmcp client: MCP stdio is
//! newline-delimited JSON-RPC, so plain process pipes (same subprocess
//! pattern as `chat_e2e.rs`) are enough to exercise the protocol surface —
//! initialize → capabilities → tools/list → tools/call round-trips.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use rmcp::serde_json::{json, Value};

/// A running sidecar child with framed stdin/stdout access.
struct SidecarProc {
    child: Child,
    stdin: Option<ChildStdin>,
    frames: Receiver<Value>,
    pending: HashMap<i64, Value>,
}

impl SidecarProc {
    fn spawn(home: &std::path::Path) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_openpaw"))
            .arg("--sidecar")
            .arg("--sidecar-transport")
            .arg("stdio")
            .env("OPENPAW_HOME", home)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn openpaw --sidecar");
        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = BufReader::new(child.stdout.take().expect("piped stdout"));
        let (tx, frames) = mpsc::channel();
        std::thread::spawn(move || {
            for line in stdout.lines() {
                let Ok(line) = line else { break };
                let Ok(frame) = rmcp::serde_json::from_str(&line) else {
                    break;
                };
                if tx.send(frame).is_err() {
                    break;
                }
            }
        });
        Self {
            child,
            stdin: Some(stdin),
            frames,
            pending: HashMap::new(),
        }
    }

    /// Write one newline-delimited JSON-RPC frame.
    fn send(&mut self, frame: Value) {
        let stdin = self.stdin.as_mut().expect("stdin still open");
        writeln!(stdin, "{frame}").expect("write frame");
        stdin.flush().expect("flush stdin");
    }

    /// Read one newline-delimited JSON-RPC frame from stdout.
    fn recv(&mut self) -> Value {
        self.frames
            .recv_timeout(Duration::from_secs(30))
            .expect("sidecar must return a valid frame within 30 seconds")
    }

    /// Frames until the response with `id` arrives (defensive skip of
    /// anything else the server might emit first).
    fn recv_response(&mut self, id: i64) -> Value {
        if let Some(frame) = self.pending.remove(&id) {
            return frame;
        }
        loop {
            let frame = self.recv();
            if frame.get("id").and_then(Value::as_i64) == Some(id) {
                return frame;
            }
            // Response order is not guaranteed; retain other calls' frames.
            if let Some(other_id) = frame.get("id").and_then(Value::as_i64) {
                self.pending.insert(other_id, frame);
            }
        }
    }

    /// The `result` object of the response with `id`, asserting success.
    fn result_of(&mut self, id: i64) -> Value {
        let frame = self.recv_response(id);
        let error = frame.get("error");
        assert!(
            error.is_none(),
            "unexpected JSON-RPC error for id {id}: {frame:?}"
        );
        frame
            .get("result")
            .cloned()
            .expect("response carries result")
    }

    /// The text payload of a successful `tools/call` response.
    fn tool_text(&mut self, id: i64) -> String {
        let result = self.result_of(id);
        assert_ne!(
            result.get("isError").and_then(Value::as_bool),
            Some(true),
            "tool call reported is_error: {result:?}"
        );
        let text = result["content"][0]["text"]
            .as_str()
            .expect("tool result carries text content");
        text.to_string()
    }

    /// Close stdin (EOF ends the stdio transport) and collect the exit.
    fn finish(mut self) -> std::process::ExitStatus {
        drop(self.stdin.take());
        self.child
            .wait()
            .expect("wait for the sidecar process to exit")
    }
}

impl Drop for SidecarProc {
    fn drop(&mut self) {
        // A timeout/assertion must not leak the child or its pipe-reader thread.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn initialize_frame(id: i64) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": {"name": "sidecar-e2e", "version": "0.0.0"}
        }
    })
}

fn tools_call_frame(id: i64, name: &str, arguments: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "tools/call",
        "params": {"name": name, "arguments": arguments}
    })
}

#[test]
fn sidecar_stdio_initialize_tools_list_and_tool_roundtrips() {
    let home = tempfile::tempdir().unwrap();
    let mut proc = SidecarProc::spawn(home.path());

    // ── initialize → server capabilities ────────────────────────────────
    proc.send(initialize_frame(0));
    let result = proc.result_of(0);
    assert!(
        result["capabilities"]["tools"].is_object(),
        "tools capability expected, got: {result:?}"
    );
    assert!(
        result["protocolVersion"].is_string(),
        "protocol version negotiated, got: {result:?}"
    );

    // ── notifications/initialized (no response expected) ────────────────
    proc.send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));

    // ── tools/list → all three capability families present ──────────────
    proc.send(json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {}}));
    let result = proc.result_of(1);
    let names: Vec<String> = result["tools"]
        .as_array()
        .expect("tools array")
        .iter()
        .filter_map(|t| t["name"].as_str().map(str::to_string))
        .collect();
    for expected in [
        "ask",
        "memory_get",
        "memory_set",
        "memory_search",
        "evolve_status",
    ] {
        assert!(
            names.iter().any(|n| n == expected),
            "tool `{expected}` missing from tools/list, got: {names:?}"
        );
    }

    // ── tools/call memory_set → memory_get round-trip ───────────────────
    proc.send(tools_call_frame(
        2,
        "memory_set",
        json!({
            "id": "sidecar-e2e-note",
            "content": "paw remembers the red ball",
            "tags": ["e2e"]
        }),
    ));
    let text = proc.tool_text(2);
    assert!(text.contains("sidecar-e2e-note"), "memory_set said: {text}");

    proc.send(tools_call_frame(
        3,
        "memory_get",
        json!({"id": "sidecar-e2e-note"}),
    ));
    let text = proc.tool_text(3);
    assert!(
        text.contains("paw remembers the red ball"),
        "memory_get round-trip failed, got: {text}"
    );

    // ── tools/call memory_search → the written memory is a hit ──────────
    proc.send(tools_call_frame(
        4,
        "memory_search",
        json!({"query": "red ball"}),
    ));
    let text = proc.tool_text(4);
    assert!(
        text.contains("sidecar-e2e-note"),
        "memory_search missed the written memory, got: {text}"
    );

    // ── tools/call evolve_status → carries the unverified ratio ─────────
    proc.send(tools_call_frame(5, "evolve_status", json!({})));
    let text = proc.tool_text(5);
    assert!(
        text.contains("unverified ratio"),
        "evolve_status missing ratio, got: {text}"
    );

    // ── tools/call ask → agent-loop single-turn reply ───────────────────
    proc.send(tools_call_frame(
        6,
        "ask",
        json!({"input": "hello sidecar"}),
    ));
    let text = proc.tool_text(6);
    assert!(
        text.contains("mock-reply-0"),
        "ask expected the mock counter reply, got: {text}"
    );

    // ── clean shutdown: stdin EOF ends the stdio transport ──────────────
    let status = proc.finish();
    assert!(status.success(), "sidecar exited with {status}");
}

#[test]
fn sidecar_concurrent_calls_preserve_responses_and_committed_writes() {
    let home = tempfile::tempdir().unwrap();
    let mut proc = SidecarProc::spawn(home.path());

    proc.send(initialize_frame(0));
    let _ = proc.result_of(0);
    proc.send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));

    // Concurrent JSON-RPC requests have no dependency ordering. Exercise
    // both responses, then issue dependent reads after the write acknowledgments.
    proc.send(tools_call_frame(
        1,
        "memory_set",
        json!({"id": "race-note", "content": "gate orders concurrent calls"}),
    ));
    proc.send(tools_call_frame(
        2,
        "memory_set",
        json!({"id": "other-note", "content": "independent concurrent write"}),
    ));

    let saved = proc.tool_text(1);
    assert!(saved.contains("race-note"), "memory_set said: {saved}");
    assert!(proc.tool_text(2).contains("other-note"));
    for (id, note, expected) in [
        (3, "race-note", "gate orders concurrent calls"),
        (4, "other-note", "independent concurrent write"),
    ] {
        proc.send(tools_call_frame(id, "memory_get", json!({"id": note})));
        assert!(proc.tool_text(id).contains(expected));
    }

    let status = proc.finish();
    assert!(status.success(), "sidecar exited with {status}");
}

#[test]
fn sidecar_unknown_transport_fails_with_three_segment_error() {
    let home = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_openpaw"))
        .arg("--sidecar")
        .arg("--sidecar-transport")
        .arg("warp-drive")
        .env("OPENPAW_HOME", home.path())
        .output()
        .expect("run sidecar with unknown transport");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("unknown sidecar transport `warp-drive`"));
    assert!(stderr.contains("why:"));
    assert!(stderr.contains("next:"));
}
