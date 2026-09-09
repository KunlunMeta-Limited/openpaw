//! `openpaw --sidecar` — the in-process MCP sidecar server (plan section
//! 3.9, new sidecar component).
//!
//! Exposes three capability families as MCP tools over the rmcp SDK:
//! - `ask` — one agent-loop turn (offline `mock`/`echo` providers, the
//!   minimal call shape distilled from `chat.rs`),
//! - `memory_get` / `memory_set` / `memory_search` — user-domain memory IO
//!   through `openpaw_memory::FsMemoryStore`, with the same domain rules as
//!   the `openpaw memory` command: user scope is `<OPENPAW_HOME>/memories`,
//! - `evolve_status` — the unverified-artifact ratio (reflect-lite
//!   `scan_unverified` over the user memories/skills roots) plus a
//!   goal-ledger overview digest.
//!
//! The `ServerHandler` impl is written by hand (no derive macros): the
//! workspace pins `rmcp` without pulling `serde`/`schemars` into the cli
//! as direct dependencies, so tool arguments are read straight from the
//! JSON object and tool schemas are declared as static JSON. This keeps
//! the handler transport-agnostic and unit-testable without any transport
//! attached (R9).
//!
//! Transports:
//! - `stdio` — rmcp transport-io: newline-delimited JSON-RPC on stdin/stdout
//!   (the transport owns stdout; startup chatter goes to stderr).
//! - `http` (default) — a thin Streamable-HTTP shell: POST /mcp with a
//!   JSON-RPC body gets a JSON response, notifications get 202, GET/DELETE
//!   get 405. The protocol layer is entirely rmcp (`serve_server` over
//!   in-memory channels); the shell only converts bytes to messages.
//!   Binds `127.0.0.1:0` and prints the bound URL to stdout.
//!
//! RED LINE: the sidecar runs in-process with `--sidecar`; there is no
//! daemon. `openpaw sidecar status` / `stop` only print a note pointing at
//! the owning process (Lite scope; the daemon shape is undecided in plan).

use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use openpaw_engine::AgentLoop;
use openpaw_goal_ledger::{last_next_action, FsGoalStore, GoalStore};
use openpaw_memory::{Confidence, FsMemoryStore, MemoryFrontmatter, MemoryStore, Scope, Source};
use openpaw_reflect_lite::scan_unverified;
use rmcp::model::{
    CallToolRequestParams, CallToolResult, ClientJsonRpcMessage, Content, JsonObject,
    ListToolsResult, ServerJsonRpcMessage, Tool,
};
use rmcp::serde_json::{self, Value};
use rmcp::service::RequestContext;
use rmcp::transport::Transport;
use rmcp::{ErrorData, RoleServer, ServerHandler, ServiceExt};
use std::future::Future;

// ── handler (transport-agnostic, unit-testable) ──────────────────────────

/// The MCP sidecar server handler. Holds the OpenPaw home root; every tool
/// derives its store from it, matching the CLI domain rules.
///
/// Tool calls are serialized through an internal gate: the MCP protocol
/// permits a client to issue requests concurrently, and unordered execution
/// would let a `memory_get` observe the store before an in-flight
/// `memory_set` finishes writing. Local, single-user, file-backed tools —
/// strict ordering costs nothing and makes call semantics predictable.
pub struct SidecarServer {
    home: PathBuf,
    gate: tokio::sync::Mutex<()>,
}

impl SidecarServer {
    /// Build the handler for an OpenPaw home (OPENPAW_HOME or ~/.openpaw).
    pub fn new(home: PathBuf) -> Self {
        Self {
            home,
            gate: tokio::sync::Mutex::new(()),
        }
    }

    /// User-scope memory store: `<home>/memories`, the same root the
    /// `openpaw memory` command uses without `--project`.
    fn memory_store(&self) -> FsMemoryStore {
        FsMemoryStore::new(self.home.join("memories"), None)
    }

    /// One-turn agent loop on an offline provider (the minimal call shape
    /// from `chat.rs`; no tool host, no session persistence).
    fn offline_provider(name: &str) -> Arc<dyn openpaw_core::LlmProvider> {
        match name {
            "echo" => Arc::new(openpaw_llm::EchoProvider::new()),
            // Everything else falls back to the counter mock so the tool
            // always answers deterministically.
            _ => Arc::new(openpaw_llm::MockProvider::counter("mock-reply-")),
        }
    }

    /// `ask` implementation: one agent-loop turn, fresh session per call.
    async fn ask(
        &self,
        input: String,
        provider: Option<String>,
    ) -> Result<CallToolResult, ErrorData> {
        let provider = Self::offline_provider(provider.as_deref().unwrap_or("mock"));
        let agent = AgentLoop::new(provider.clone());
        let mut session = openpaw_core::Session::new(provider.default_model(), provider.name());
        session
            .messages
            .push(openpaw_core::Message::user_text(input));
        let mut sink = |_: openpaw_engine::AgentEvent| {};
        agent
            .run_turn(&mut session, None, &[], &mut sink)
            .await
            .map_err(|e| ErrorData::internal_error(format!("agent turn failed: {e}"), None))?;
        let reply = session
            .messages
            .last()
            .filter(|m| m.role == openpaw_core::Role::Assistant)
            .map(|m| m.text())
            .unwrap_or_default();
        Ok(CallToolResult::success(vec![Content::text(reply)]))
    }

    /// `memory_set` implementation: upsert one user-scope memory by id.
    async fn memory_set(
        &self,
        id: String,
        content: String,
        tags: Vec<String>,
    ) -> Result<CallToolResult, ErrorData> {
        let store = self.memory_store();
        // Upsert semantics: update in place when the id already exists.
        if let Ok(Some(existing)) = MemoryStore::get(&store, &id) {
            store
                .update_in_place(&id, &existing.frontmatter, &content)
                .map_err(|e| {
                    ErrorData::internal_error(format!("memory update failed: {e}"), None)
                })?;
            return Ok(CallToolResult::success(vec![Content::text(format!(
                "updated memory `{id}`"
            ))]));
        }
        let mut frontmatter = MemoryFrontmatter::new(Source::User, Confidence::Medium, tags);
        frontmatter.id = id.clone();
        MemoryStore::put(&store, Scope::User, frontmatter, &content)
            .map_err(|e| ErrorData::internal_error(format!("memory write failed: {e}"), None))?;
        Ok(CallToolResult::success(vec![Content::text(format!(
            "saved memory `{id}`"
        ))]))
    }

    /// `memory_get` implementation: read one memory by id.
    async fn memory_get(&self, id: String) -> Result<CallToolResult, ErrorData> {
        let store = self.memory_store();
        let memory = MemoryStore::get(&store, &id)
            .map_err(|e| ErrorData::internal_error(format!("memory read failed: {e}"), None))?
            .ok_or_else(|| ErrorData::invalid_params(format!("no memory with id `{id}`"), None))?;
        let text = format!(
            "# {}  [{:?}]\nzone:     {}\ncreated:  {}\n\n{}",
            memory.frontmatter.id,
            memory.frontmatter.memory_type,
            memory.frontmatter.zone,
            memory.frontmatter.created.format("%Y-%m-%dT%H:%M:%SZ"),
            memory.body.trim()
        );
        Ok(CallToolResult::success(vec![Content::text(text)]))
    }

    /// `memory_search` implementation: relevance-ranked hits.
    async fn memory_search(
        &self,
        query: String,
        limit: Option<usize>,
    ) -> Result<CallToolResult, ErrorData> {
        let store = self.memory_store();
        let results = store
            .search_memories(&query, limit.unwrap_or(20))
            .map_err(|e| ErrorData::internal_error(format!("memory search failed: {e}"), None))?;
        if results.is_empty() {
            return Ok(CallToolResult::success(vec![Content::text(format!(
                "(no memories matching '{query}')"
            ))]));
        }
        let lines: Vec<String> = results
            .iter()
            .map(|(m, score)| {
                let first = m.body.lines().map(str::trim).find(|l| !l.is_empty());
                format!(
                    "{}  score={score:.3}  {}",
                    m.frontmatter.id,
                    first.unwrap_or("")
                )
            })
            .collect();
        Ok(CallToolResult::success(vec![Content::text(
            lines.join("\n"),
        )]))
    }

    /// `evolve_status` implementation: unverified ratio + goal overview.
    async fn evolve_status(&self) -> Result<CallToolResult, ErrorData> {
        let mut lines: Vec<String> = Vec::new();
        let mut total_files = 0usize;
        let mut total_unverified = 0usize;
        let roots: [(&str, PathBuf); 2] = [
            ("user memories", self.home.join("memories")),
            ("user skills", self.home.join("skills")),
        ];
        for (label, root) in roots {
            match scan_unverified(&root) {
                Ok(stats) => {
                    total_files += stats.files;
                    total_unverified += stats.unverified;
                    lines.push(format!(
                        "{label}: {}/{} unverified (ratio {:.3})",
                        stats.unverified,
                        stats.files,
                        stats.ratio()
                    ));
                }
                Err(e) => lines.push(format!("{label}: scan failed ({e})")),
            }
        }
        let overall = if total_files == 0 {
            0.0
        } else {
            total_unverified as f64 / total_files as f64
        };
        lines.push(format!(
            "unverified ratio: {overall:.3} ({total_unverified}/{total_files})"
        ));

        let goal_store = FsGoalStore::new(self.home.join("goals"));
        match GoalStore::list_goals(&goal_store) {
            Ok(goals) if goals.is_empty() => lines.push(
                "goals: (none yet - goal profiles appear under <OPENPAW_HOME>/goals)".to_string(),
            ),
            Ok(goals) => {
                lines.push(format!("goals: {}", goals.len()));
                for doc in &goals {
                    let fm = &doc.frontmatter;
                    let events = GoalStore::query_events(&goal_store, &fm.id).unwrap_or_default();
                    let next = last_next_action(&events);
                    lines.push(format!("  {}  [{:?}]  {}", fm.id, fm.status, fm.objective));
                    if let Some(next) = next {
                        lines.push(format!("      next: {next}"));
                    }
                }
            }
            Err(e) => lines.push(format!("goals: cannot list ({e})")),
        }
        Ok(CallToolResult::success(vec![Content::text(
            lines.join("\n"),
        )]))
    }
}

// ── MCP surface: static tool definitions + hand-written dispatch ─────────

/// Build one `Tool` definition from a static JSON schema value. Panicking
/// on a malformed static schema is fine: it is a compile-time constant.
fn tool_def(name: &str, description: &str, schema: Value) -> Tool {
    let map: JsonObject = serde_json::from_value(schema).expect("static tool schema is an object");
    Tool::new(name.to_string(), description.to_string(), Arc::new(map))
}

/// The five sidecar tools: conversation, memory IO, evolution status.
fn sidecar_tools() -> Vec<Tool> {
    vec![
        tool_def(
            "ask",
            "One-turn conversation: send user input, get the agent-loop reply \
             (offline mock/echo provider; single-turn, no cross-call state).",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "input": {
                        "type": "string",
                        "description": "User input text for one agent-loop turn."
                    },
                    "provider": {
                        "type": "string",
                        "description": "Offline provider: `mock` (counter replies, default) or `echo`."
                    }
                },
                "required": ["input"]
            }),
        ),
        tool_def(
            "memory_set",
            "Write a user-domain memory by id (creates it, or updates the body \
             when the id exists). Stored under <OPENPAW_HOME>/memories.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "id": {
                        "type": "string",
                        "description": "Memory id (letters, digits, '-', '_')."
                    },
                    "content": {
                        "type": "string",
                        "description": "Memory body (markdown)."
                    },
                    "tags": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Optional tags stored in the frontmatter."
                    }
                },
                "required": ["id", "content"]
            }),
        ),
        tool_def(
            "memory_get",
            "Read one memory by id and return its header and body.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "id": {
                        "type": "string",
                        "description": "Memory id as reported by memory_search or `openpaw memory list`."
                    }
                },
                "required": ["id"]
            }),
        ),
        tool_def(
            "memory_search",
            "Search user-domain memories by query words (TF-IDF ranking); \
             returns id, score, and the first line of each hit.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Query words (empty matches everything)."
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum number of hits (default 20)."
                    }
                },
                "required": ["query"]
            }),
        ),
        tool_def(
            "evolve_status",
            "Self-evolution status: unverified-artifact ratio over the user \
             memories/skills roots (reflect-lite scan) plus a goal-ledger overview.",
            serde_json::json!({"type": "object", "properties": {}}),
        ),
    ]
}

fn required_string(args: &JsonObject, key: &str) -> Result<String, ErrorData> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| ErrorData::invalid_params(format!("missing string parameter `{key}`"), None))
}

fn optional_string(args: &JsonObject, key: &str) -> Option<String> {
    args.get(key).and_then(Value::as_str).map(str::to_string)
}

fn optional_string_list(args: &JsonObject, key: &str) -> Vec<String> {
    args.get(key)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

impl ServerHandler for SidecarServer {
    fn get_info(&self) -> rmcp::model::ServerInfo {
        rmcp::model::ServerInfo::new(
            rmcp::model::ServerCapabilities::builder()
                .enable_tools()
                .build(),
        )
        .with_instructions(
            "OpenPaw sidecar: one-turn conversation, user-domain memory IO, and evolution status.",
        )
    }

    async fn list_tools(
        &self,
        _request: Option<rmcp::model::PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(sidecar_tools()))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        // Serialize all tool invocations (see struct doc).
        let _gate = self.gate.lock().await;
        let args = request.arguments.unwrap_or_default();
        match request.name.as_ref() {
            "ask" => {
                let input = required_string(&args, "input")?;
                let provider = optional_string(&args, "provider");
                self.ask(input, provider).await
            }
            "memory_set" => {
                let id = required_string(&args, "id")?;
                let content = required_string(&args, "content")?;
                let tags = optional_string_list(&args, "tags");
                self.memory_set(id, content, tags).await
            }
            "memory_get" => {
                let id = required_string(&args, "id")?;
                self.memory_get(id).await
            }
            "memory_search" => {
                let query = required_string(&args, "query")?;
                let limit = args
                    .get("limit")
                    .and_then(Value::as_u64)
                    .map(|v| v as usize);
                self.memory_search(query, limit).await
            }
            "evolve_status" => self.evolve_status().await,
            other => Err(ErrorData::invalid_params(
                format!("unknown tool `{other}`"),
                None,
            )),
        }
    }
}

// ── entry points ─────────────────────────────────────────────────────────

// Note: the `SidecarCommand` enum (status/stop) lives in the library's
// `cli` module together with the rest of the CLI surface; the binary
// dispatches it to the two printers below.

/// `openpaw sidecar status` — prints the in-process note (Lite scope; the
/// daemon shape is undecided in plan, so there is nothing to query).
pub fn run_status_note() -> i32 {
    println!("sidecar runs in-process with --sidecar; no daemon");
    println!(
        "next: start it with `openpaw --sidecar` (transport: --sidecar-transport stdio|http)."
    );
    0
}

/// `openpaw sidecar stop` — prints the in-process note; stopping means
/// terminating the owning `openpaw --sidecar` process.
pub fn run_stop_note() -> i32 {
    println!("sidecar runs in-process with --sidecar; no daemon to stop");
    println!("next: terminate the `openpaw --sidecar` process that owns the server.");
    0
}

/// `openpaw --sidecar` entry: dispatch on `--sidecar-transport`.
pub fn run_server(transport: &str) -> i32 {
    match transport {
        "http" => run_http_server(),
        "stdio" => run_stdio_server(),
        other => {
            eprintln!("error: unknown sidecar transport `{other}`");
            eprintln!("why: the sidecar speaks MCP over one of two transports: stdio or http.");
            eprintln!("next: re-run with --sidecar-transport stdio or --sidecar-transport http.");
            1
        }
    }
}

fn run_stdio_server() -> i32 {
    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("error: failed to start async runtime: {e}");
            return 1;
        }
    };
    rt.block_on(async move {
        let server = SidecarServer::new(openpaw_core::openpaw_home());
        // The stdio transport owns stdout for the protocol; nothing else may
        // print there while the server runs.
        match server.serve(rmcp::transport::stdio()).await {
            Ok(running) => match running.waiting().await {
                Ok(_) => 0,
                Err(e) => {
                    eprintln!("error: sidecar stdio server stopped: {e}");
                    1
                }
            },
            Err(e) => {
                eprintln!("error: cannot start sidecar stdio server: {e}");
                1
            }
        }
    })
}

fn run_http_server() -> i32 {
    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("error: failed to start async runtime: {e}");
            return 1;
        }
    };
    rt.block_on(http_main())
}

async fn http_main() -> i32 {
    // In-memory JSON-RPC pipes between the thin HTTP shell and the rmcp
    // service. Protocol handling is entirely rmcp; the shell never parses
    // tool payloads.
    let (in_tx, in_rx) = tokio::sync::mpsc::unbounded_channel::<ClientJsonRpcMessage>();
    let (out_tx, out_rx) = tokio::sync::mpsc::unbounded_channel::<ServerJsonRpcMessage>();

    let server = SidecarServer::new(openpaw_core::openpaw_home());
    let serve_task = tokio::spawn(async move {
        let transport = HttpPipeTransport { out_tx, in_rx };
        match server.serve(transport).await {
            Ok(running) => {
                let _ = running.waiting().await;
            }
            Err(e) => eprintln!("error: sidecar service failed to start: {e}"),
        }
    });

    // Dynamic port on loopback only; the bound URL goes to stdout so
    // scripts/launchers can pick it up (stdio keeps stdout protocol-clean).
    let listener = match std::net::TcpListener::bind("127.0.0.1:0") {
        Ok(l) => l,
        Err(e) => {
            eprintln!("error: cannot bind sidecar http listener on 127.0.0.1:0: {e}");
            eprintln!("why: the sidecar HTTP transport binds loopback only, with a dynamic port.");
            eprintln!("next: free a loopback listener slot or use --sidecar-transport stdio.");
            return 1;
        }
    };
    let addr = match listener.local_addr() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("error: cannot read bound sidecar address: {e}");
            return 1;
        }
    };
    println!("sidecar http listening on http://{addr}/mcp");
    let _ = std::io::stdout().flush();

    // The accept loop owns the single consumer side of `out_rx`; it runs on
    // a blocking thread and handles connections strictly one at a time
    // (Lite scope: no concurrent-session fan-out).
    let mut out_rx = out_rx;
    let http_thread = std::thread::spawn(move || http_accept_loop(listener, in_tx, &mut out_rx));

    // Park the async runtime forever; the process exits on Ctrl-C / kill.
    // (No graceful-shutdown plumbing: the sidecar owns no daemon state.)
    let _ = std::future::pending::<()>().await;
    drop(serve_task);
    drop(http_thread);
    0
}

// ── HTTP shell (bytes ↔ JSON-RPC messages only) ──────────────────────────

/// In-memory transport between the HTTP shell (blocking thread) and the
/// rmcp service: client messages flow in through one tokio channel, server
/// messages flow back through another. tokio senders do not implement
/// `futures::Sink`, so this small adapter implements rmcp's `Transport`
/// directly.
struct HttpPipeTransport {
    out_tx: tokio::sync::mpsc::UnboundedSender<ServerJsonRpcMessage>,
    in_rx: tokio::sync::mpsc::UnboundedReceiver<ClientJsonRpcMessage>,
}

impl Transport<RoleServer> for HttpPipeTransport {
    type Error = std::io::Error;

    fn send(
        &mut self,
        item: ServerJsonRpcMessage,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send + 'static {
        let tx = self.out_tx.clone();
        async move {
            tx.send(item).map_err(|_| {
                std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "sidecar http shell dropped the response channel",
                )
            })
        }
    }

    fn receive(&mut self) -> impl Future<Output = Option<ClientJsonRpcMessage>> + Send {
        self.in_rx.recv()
    }

    fn close(&mut self) -> impl Future<Output = Result<(), Self::Error>> + Send {
        std::future::ready(Ok(()))
    }
}

/// Request body cap (mirrors the memory write-side limit).
const MAX_HTTP_BODY: usize = 16 * 1024 * 1024;

/// Per-request socket timeouts: an idle HTTP peer must not pin the single
/// accept thread forever.
const HTTP_READ_TIMEOUT: Duration = Duration::from_secs(300);
const HTTP_WRITE_TIMEOUT: Duration = Duration::from_secs(30);

fn http_accept_loop(
    listener: std::net::TcpListener,
    in_tx: tokio::sync::mpsc::UnboundedSender<ClientJsonRpcMessage>,
    out_rx: &mut tokio::sync::mpsc::UnboundedReceiver<ServerJsonRpcMessage>,
) {
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        if let Err(e) = http_handle_connection(stream, &in_tx, out_rx) {
            eprintln!("warn: sidecar http connection failed: {e}");
        }
    }
}

fn http_handle_connection(
    stream: std::net::TcpStream,
    in_tx: &tokio::sync::mpsc::UnboundedSender<ClientJsonRpcMessage>,
    out_rx: &mut tokio::sync::mpsc::UnboundedReceiver<ServerJsonRpcMessage>,
) -> std::io::Result<()> {
    let _ = stream.set_read_timeout(Some(HTTP_READ_TIMEOUT));
    let _ = stream.set_write_timeout(Some(HTTP_WRITE_TIMEOUT));
    let mut reader = BufReader::new(stream.try_clone()?);
    match read_http_request(&mut reader) {
        Ok(ReadOutcome::Request(req)) => {
            let (status, body) = http_route(req, in_tx, out_rx);
            write_http_response(&stream, status, &body)
        }
        Ok(ReadOutcome::Eof) => Ok(()), // clean EOF before a request
        Ok(ReadOutcome::TooLarge) => write_http_response(
            &stream,
            413,
            &jsonrpc_error_body(-32600, "Payload Too Large"),
        ),
        Err(e)
            if e.kind() == std::io::ErrorKind::WouldBlock
                || e.kind() == std::io::ErrorKind::TimedOut =>
        {
            Ok(()) // idle peer timed out; drop the connection quietly
        }
        Err(e) => Err(e),
    }
}

struct HttpRequest {
    method: String,
    path: String,
    body: Vec<u8>,
}

enum ReadOutcome {
    Request(HttpRequest),
    Eof,
    TooLarge,
}

/// Minimal HTTP/1.1 request reader: request line, headers until a blank
/// line, then exactly `Content-Length` body bytes (one request per
/// connection; responses carry `Connection: close`).
fn read_http_request(reader: &mut BufReader<std::net::TcpStream>) -> std::io::Result<ReadOutcome> {
    let mut request_line = String::new();
    if reader.read_line(&mut request_line)? == 0 {
        return Ok(ReadOutcome::Eof);
    }
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_ascii_uppercase();
    let path = parts.next().unwrap_or("/").to_string();

    let mut content_length = 0usize;
    loop {
        let mut header_line = String::new();
        if reader.read_line(&mut header_line)? == 0 {
            break;
        }
        let header = header_line.trim_end();
        if header.is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':') {
            if name.trim().eq_ignore_ascii_case("content-length") {
                content_length = value.trim().parse().unwrap_or(0);
            }
        }
    }

    if content_length > MAX_HTTP_BODY {
        return Ok(ReadOutcome::TooLarge);
    }
    let body = if content_length == 0 {
        Vec::new()
    } else {
        let mut buf = vec![0u8; content_length];
        reader.read_exact(&mut buf)?;
        buf
    };
    Ok(ReadOutcome::Request(HttpRequest { method, path, body }))
}

fn http_route(
    req: HttpRequest,
    in_tx: &tokio::sync::mpsc::UnboundedSender<ClientJsonRpcMessage>,
    out_rx: &mut tokio::sync::mpsc::UnboundedReceiver<ServerJsonRpcMessage>,
) -> (u16, String) {
    let path_only = req.path.split('?').next().unwrap_or("/");
    if path_only != "/mcp" {
        return (404, jsonrpc_error_body(-32601, "Not Found"));
    }
    match req.method.as_str() {
        "POST" => http_post(req.body, in_tx, out_rx),
        // Streamable HTTP GET (SSE stream) and DELETE (session teardown)
        // are not part of this stateless JSON shell; 405 is spec-allowed.
        "GET" | "DELETE" => (405, jsonrpc_error_body(-32601, "Method Not Allowed")),
        _ => (405, jsonrpc_error_body(-32601, "Method Not Allowed")),
    }
}

fn http_post(
    body: Vec<u8>,
    in_tx: &tokio::sync::mpsc::UnboundedSender<ClientJsonRpcMessage>,
    out_rx: &mut tokio::sync::mpsc::UnboundedReceiver<ServerJsonRpcMessage>,
) -> (u16, String) {
    let msg: ClientJsonRpcMessage = match serde_json::from_slice(&body) {
        Ok(m) => m,
        Err(_) => return (400, jsonrpc_error_body(-32700, "Parse error")),
    };
    // Requests carry an id and expect a matching response; notifications
    // (e.g. `notifications/initialized`) get 202 with no body.
    let expect_id: Option<rmcp::model::RequestId> = match &msg {
        ClientJsonRpcMessage::Request(r) => Some(r.id.clone()),
        _ => None,
    };
    if in_tx.send(msg).is_err() {
        return (
            502,
            jsonrpc_error_body(-32000, "Sidecar service is not running"),
        );
    }
    let Some(id) = expect_id else {
        return (202, String::new());
    };
    loop {
        match out_rx.blocking_recv() {
            Some(ServerJsonRpcMessage::Response(resp)) if resp.id == id => {
                let frame = serde_json::to_string(&ServerJsonRpcMessage::Response(resp))
                    .unwrap_or_default();
                return (200, frame);
            }
            Some(ServerJsonRpcMessage::Error(err)) if err.id.as_ref() == Some(&id) => {
                let frame =
                    serde_json::to_string(&ServerJsonRpcMessage::Error(err)).unwrap_or_default();
                return (200, frame);
            }
            // Defensive skip: this handler emits no server-initiated
            // requests or notifications, so anything else is stale.
            Some(_) => continue,
            None => {
                return (
                    502,
                    jsonrpc_error_body(-32000, "Sidecar service closed the session"),
                )
            }
        }
    }
}

fn jsonrpc_error_body(code: i64, message: &str) -> String {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": serde_json::Value::Null,
        "error": {"code": code, "message": message}
    })
    .to_string()
}

fn write_http_response(
    stream: &std::net::TcpStream,
    status: u16,
    body: &str,
) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        202 => "Accepted",
        400 => "Bad Request",
        404 => "Not Found",
        405 => "Method Not Allowed",
        413 => "Payload Too Large",
        502 => "Bad Gateway",
        _ => "OK",
    };
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let mut stream = stream;
    stream.write_all(head.as_bytes())?;
    stream.write_all(body.as_bytes())?;
    stream.flush()
}

// ── handler unit tests (R9: no transport attached) ───────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn test_home() -> PathBuf {
        tempfile::tempdir().unwrap().keep()
    }

    #[tokio::test]
    async fn memory_set_get_roundtrip() {
        let server = SidecarServer::new(test_home());
        let saved = server
            .memory_set(
                "unit-note".into(),
                "paw stores facts as markdown".into(),
                vec!["unit".into()],
            )
            .await
            .unwrap();
        assert!(!saved.is_error.unwrap_or(false));
        let loaded = server.memory_get("unit-note".into()).await.unwrap();
        let text = text_of(&loaded);
        assert!(text.contains("paw stores facts as markdown"), "{text}");
    }

    #[tokio::test]
    async fn memory_set_updates_existing() {
        let server = SidecarServer::new(test_home());
        server
            .memory_set("unit-note".into(), "first".into(), vec![])
            .await
            .unwrap();
        server
            .memory_set("unit-note".into(), "second".into(), vec![])
            .await
            .unwrap();
        let loaded = server.memory_get("unit-note".into()).await.unwrap();
        let text = text_of(&loaded);
        assert!(text.contains("second"), "{text}");
    }

    #[tokio::test]
    async fn memory_get_missing_is_error() {
        let server = SidecarServer::new(test_home());
        let err = server.memory_get("ghost".into()).await.unwrap_err();
        assert!(err.message.contains("no memory with id `ghost`"));
    }

    #[tokio::test]
    async fn memory_search_finds_written() {
        let server = SidecarServer::new(test_home());
        server
            .memory_set(
                "unit-search".into(),
                "the quick paw jumps over the lazy log".into(),
                vec![],
            )
            .await
            .unwrap();
        let found = server
            .memory_search("paw jumps".into(), None)
            .await
            .unwrap();
        let text = text_of(&found);
        assert!(text.contains("unit-search"), "{text}");
    }

    #[tokio::test]
    async fn evolve_status_reports_ratio() {
        let server = SidecarServer::new(test_home());
        let result = server.evolve_status().await.unwrap();
        let text = text_of(&result);
        assert!(text.contains("unverified ratio"), "{text}");
        assert!(text.contains("goals:"), "{text}");
    }

    #[tokio::test]
    async fn ask_returns_mock_reply() {
        let server = SidecarServer::new(test_home());
        let result = server.ask("hello sidecar".into(), None).await.unwrap();
        let text = text_of(&result);
        assert!(text.contains("mock-reply-0"), "{text}");
    }

    #[tokio::test]
    async fn ask_echo_provider_echoes_input() {
        let server = SidecarServer::new(test_home());
        let result = server
            .ask("parrot this".into(), Some("echo".into()))
            .await
            .unwrap();
        let text = text_of(&result);
        assert!(text.contains("parrot this"), "{text}");
    }

    #[tokio::test]
    async fn tool_list_covers_all_five() {
        let names: Vec<String> = sidecar_tools()
            .into_iter()
            .map(|t| t.name.to_string())
            .collect();
        for expected in [
            "ask",
            "memory_get",
            "memory_set",
            "memory_search",
            "evolve_status",
        ] {
            assert!(names.iter().any(|n| n == expected), "{names:?}");
        }
    }

    #[test]
    fn unknown_transport_is_rejected() {
        assert_eq!(run_server("carrier-pigeon"), 1);
    }

    fn text_of(result: &CallToolResult) -> String {
        result
            .content
            .iter()
            .filter_map(|c| c.as_text().map(|t| t.text.clone()))
            .collect::<Vec<_>>()
            .join("\n")
    }
}
