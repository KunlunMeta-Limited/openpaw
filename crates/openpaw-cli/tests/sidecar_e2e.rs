#![cfg(feature = "sidecar")]
//! E2E: `openpaw --sidecar --sidecar-transport stdio` — the MCP sidecar
//! driven with raw JSON-RPC frames through the child process pipes.
//!
//! The test deliberately does not use the rmcp client: MCP stdio is
//! newline-delimited JSON-RPC, so plain process pipes (same subprocess
//! pattern as `chat_e2e.rs`) are enough to exercise the protocol surface —
//! initialize → capabilities → tools/list → tools/call round-trips.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use rmcp::serde_json::{json, Value};

/// A running sidecar child with framed stdin/stdout access.
struct SidecarProc {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    /// Responses that arrived before the one currently awaited, kept so an
    /// out-of-order frame is never dropped (dropping it used to hang the next
    /// wait forever instead of failing the assertion).
    pending: std::collections::HashMap<i64, Value>,
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
        Self {
            child,
            stdin: Some(stdin),
            stdout,
            pending: std::collections::HashMap::new(),
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
        let mut line = String::new();
        let n = self
            .stdout
            .read_line(&mut line)
            .expect("read frame from sidecar stdout");
        assert!(n > 0, "sidecar closed stdout before the expected frame");
        rmcp::serde_json::from_str(line.trim()).expect("sidecar frame is valid JSON")
    }

    /// The response with `id`, buffering (not discarding) earlier frames.
    fn recv_response(&mut self, id: i64) -> Value {
        if let Some(frame) = self.pending.remove(&id) {
            return frame;
        }
        loop {
            let frame = self.recv();
            match frame.get("id").and_then(Value::as_i64) {
                Some(other) if other == id => return frame,
                Some(other) => {
                    self.pending.insert(other, frame);
                }
                // Notifications carry no id and are not responses.
                None => continue,
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
fn sidecar_concurrent_calls_serialize_set_before_get() {
    let home = tempfile::tempdir().unwrap();
    let mut proc = SidecarProc::spawn(home.path());

    proc.send(initialize_frame(0));
    let _ = proc.result_of(0);
    proc.send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));

    // Write both frames back-to-back before draining any response: the
    // server may dispatch them concurrently, and an unordered get would
    // observe the store before the in-flight set finishes writing.
    proc.send(tools_call_frame(
        1,
        "memory_set",
        json!({"id": "race-note", "content": "gate orders concurrent calls"}),
    ));
    proc.send(tools_call_frame(
        2,
        "memory_get",
        json!({"id": "race-note"}),
    ));

    let saved = proc.tool_text(1);
    assert!(saved.contains("race-note"), "memory_set said: {saved}");
    let text = proc.tool_text(2);
    assert!(
        text.contains("gate orders concurrent calls"),
        "concurrent memory_get lost the write: {text}"
    );

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
