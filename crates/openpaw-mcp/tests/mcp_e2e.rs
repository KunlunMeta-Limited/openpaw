//! MC-2 e2e: handshake, tool calls, resources, call timeout, circuit
//! breaker, and reconnection — fully in-process. A mock rmcp server runs
//! on one end of a duplex pair; `ServerSession::connect_over_transport`
//! takes the other, so no external process or network is involved.

use std::sync::Arc;
use std::time::Duration;

use openpaw_mcp::server::ServerSession;
use openpaw_mcp::{CircuitBreaker, CircuitState, McpError};
use rmcp::model::{
    CallToolResponse, CallToolResult, ContentBlock, JsonObject, ListResourcesResult,
    ListToolsResult, ReadResourceResponse, ReadResourceResult, Resource, ServerCapabilities,
    ServerConfig, Tool,
};
use rmcp::service::{RequestContext, RunningService};
use rmcp::transport::async_rw::AsyncRwTransport;
use rmcp::{ErrorData, RoleServer, ServerHandler, ServiceExt};

const T: Duration = Duration::from_secs(5);
const CALL_TIMEOUT: Duration = Duration::from_millis(400);

/// A controllable MCP server: `echo` answers, `fail` answers with a
/// tool-level error result, everything else is invalid. `call_delay`
/// stalls every call (timeout tests).
struct MockServer {
    call_delay: Option<Duration>,
}

impl ServerHandler for MockServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .build(),
        )
    }

    async fn list_tools(
        &self,
        _request: Option<rmcp::model::PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(vec![
            tool("echo", "echoes args.text"),
            tool("fail", "always answers is_error"),
        ]))
    }

    async fn call_tool(
        &self,
        request: rmcp::model::CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        if let Some(delay) = self.call_delay {
            tokio::time::sleep(delay).await;
        }
        let args = request.arguments.unwrap_or_default();
        let result = match request.name.as_ref() {
            "echo" => {
                let text = args
                    .get("text")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                    "echo: {text}"
                ))]))
            }
            "fail" => Ok(CallToolResult::error(vec![ContentBlock::text(
                "mock tool failure",
            )])),
            _ => Ok(CallToolResult::error(vec![ContentBlock::text(format!(
                "unknown tool {}",
                request.name
            ))])),
        };
        result.map(Into::into)
    }

    async fn list_resources(
        &self,
        _request: Option<rmcp::model::PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        let resource = Resource::new("mock://doc/1", "Mock Document").with_mime_type("text/plain");
        Ok(ListResourcesResult::with_all_items(vec![resource]))
    }

    async fn read_resource(
        &self,
        request: rmcp::model::ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, ErrorData> {
        if request.uri != "mock://doc/1" {
            return Err(ErrorData::invalid_params(
                format!("no resource {}", request.uri),
                None,
            ));
        }
        Ok(
            ReadResourceResult::new(vec![rmcp::model::ResourceContents::TextResourceContents {
                uri: "mock://doc/1".into(),
                mime_type: Some("text/plain".into()),
                text: "mock body".into(),
                meta: None,
            }])
            .into(),
        )
    }
}

fn tool(name: &str, description: &str) -> Tool {
    let schema: JsonObject = rmcp::serde_json::from_value(rmcp::serde_json::json!({
        "type": "object",
        "properties": { "text": { "type": "string" } }
    }))
    .expect("static schema is an object");
    Tool::new(name.to_string(), description.to_string(), Arc::new(schema))
}

/// Wire both handshakes concurrently and keep the actual server service.
/// D13: aborting an outer waiter does not prove the transport stopped. // openpaw-scrub-allow
/// Tests explicitly cancel and await the service before asserting a dead peer.
async fn mock_pair(
    call_delay: Option<Duration>,
) -> (ServerSession, RunningService<RoleServer, MockServer>) {
    let (client_io, server_io) = tokio::io::duplex(1 << 16);
    let (client_read, client_write) = tokio::io::split(client_io);
    let (server_read, server_write) = tokio::io::split(server_io);

    let server = MockServer { call_delay };
    let (server, session) = tokio::join!(
        server.serve(AsyncRwTransport::new_server(server_read, server_write)),
        ServerSession::connect_over_transport(
            "mock",
            AsyncRwTransport::new_client(client_read, client_write),
            T,
        )
    );
    (
        session.expect("in-process client handshake"),
        server.expect("in-process server handshake"),
    )
}

#[tokio::test]
async fn handshake_and_list_tools() {
    let (session, handle) = mock_pair(None).await;
    let tools = session.list_tools(T).await.unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t.name.as_ref()).collect();
    assert_eq!(names, vec!["echo", "fail"]);
    session.shutdown().await.unwrap();
    handle.cancel().await.expect("server service stopped");
}

#[tokio::test]
async fn echo_roundtrip() {
    let (session, handle) = mock_pair(None).await;
    let mut args = JsonObject::new();
    args.insert("text".into(), "hi".into());
    let result = session.call_tool("echo", args, T).await.unwrap();
    assert!(!result.is_error.unwrap_or(false));
    assert_eq!(result.content[0].as_text().unwrap().text, "echo: hi");
    session.shutdown().await.unwrap();
    handle.cancel().await.expect("server service stopped");
}

#[tokio::test]
async fn tool_error_result_is_ok_transport_never_fails() {
    let (session, handle) = mock_pair(None).await;
    let result = session
        .call_tool("fail", JsonObject::new(), T)
        .await
        .unwrap();
    assert!(result.is_error.unwrap_or(false));
    // Server answered; the breaker must see a success, not a failure.
    session.shutdown().await.unwrap();
    handle.cancel().await.expect("server service stopped");
}

#[tokio::test]
async fn resources_list_and_read() {
    let (session, handle) = mock_pair(None).await;
    let resources = session.list_resources(T).await.unwrap();
    assert_eq!(resources.len(), 1);
    let read = session.read_resource("mock://doc/1", T).await.unwrap();
    assert_eq!(read.contents.len(), 1);
    session.shutdown().await.unwrap();
    handle.cancel().await.expect("server service stopped");
}

#[tokio::test]
async fn slow_tool_hits_call_timeout() {
    let (session, handle) = mock_pair(Some(Duration::from_secs(30))).await;
    let err = session
        .call_tool("echo", JsonObject::new(), CALL_TIMEOUT)
        .await
        .unwrap_err();
    assert!(matches!(err, McpError::Timeout(_)), "got: {err:?}");
    assert!(err.is_retryable());
    session.shutdown().await.unwrap();
    handle.cancel().await.expect("server service stopped");
}

#[tokio::test]
async fn breaker_trips_on_transport_failure_then_reconnects() {
    let (session, handle) = mock_pair(None).await;
    let mut breaker = CircuitBreaker::new(3, Duration::from_millis(120));

    // Stop the actual service and await transport closure before the calls.
    handle.cancel().await.expect("server service stopped");

    for _ in 0..3 {
        assert!(breaker.can_attempt());
        let err = session.call_tool("echo", JsonObject::new(), T).await;
        assert!(err.is_err(), "dead server must fail calls");
        breaker.record_failure();
    }
    assert_eq!(breaker.state(), CircuitState::Open);
    assert!(!breaker.can_attempt(), "open rejects without a round trip");

    // Fresh cooldown; reconnect and probe.
    tokio::time::sleep(Duration::from_millis(140)).await;
    assert!(breaker.can_attempt(), "cooldown elapsed admits the probe");
    drop(session);
    let (session, handle) = mock_pair(None).await;
    let probe = session.call_tool("echo", JsonObject::new(), T).await;
    assert!(probe.is_ok(), "reconnected server answers the probe");
    breaker.record_success();
    assert_eq!(breaker.state(), CircuitState::Closed);

    session.shutdown().await.unwrap();
    handle.cancel().await.expect("server service stopped");
}

#[tokio::test]
async fn http_json_and_error_bodies_are_bounded_with_or_without_length() {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::time::Instant;
    for (status, limit, chunked) in [
        (200, 16 * 1024 * 1024, false),
        (400, 64 * 1024, false),
        (200, 16 * 1024 * 1024, true),
        (400, 64 * 1024, true),
    ] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/mcp", listener.local_addr().unwrap());
        let worker = std::thread::spawn(move || {
            listener.set_nonblocking(true).unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut socket = loop {
                match listener.accept() {
                    Ok((s, _)) => break s,
                    Err(e)
                        if e.kind() == std::io::ErrorKind::WouldBlock
                            && Instant::now() < deadline =>
                    {
                        std::thread::sleep(Duration::from_millis(2))
                    }
                    other => panic!("bounded HTTP fixture was not reached: {other:?}"),
                }
            };
            socket.set_nonblocking(false).unwrap();
            socket.set_read_timeout(Some(T)).unwrap();
            socket.set_write_timeout(Some(T)).unwrap();
            let mut buf = [0; 8192];
            assert!(socket.read(&mut buf).unwrap() > 0);
            if chunked {
                write!(socket, "HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n").unwrap();
                let chunk = vec![b' '; 64 * 1024];
                for _ in 0..=limit / chunk.len() {
                    if write!(socket, "{:x}\r\n", chunk.len())
                        .and_then(|_| socket.write_all(&chunk))
                        .and_then(|_| socket.write_all(b"\r\n"))
                        .is_err()
                    {
                        break;
                    }
                }
                let _ = socket.write_all(b"0\r\n\r\n");
            } else {
                write!(socket, "HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", limit + 1).unwrap();
            }
        });
        let spec = openpaw_mcp::ServerSpec::Http {
            url: url.parse().unwrap(),
            headers: Default::default(),
        };
        let started = Instant::now();
        let error = ServerSession::connect("bounded", &spec, T)
            .await
            .unwrap_err();
        worker.join().unwrap();
        assert!(
            started.elapsed() < T,
            "must reject actual size, not hang until request timeout"
        );
        assert!(
            error.to_string().contains("maximum size"),
            "status={status}, chunked={chunked}: {error}"
        );
    }
}

#[tokio::test]
async fn http_sse_is_streamed_without_a_total_body_limit() {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::time::Instant;
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    let worker = std::thread::spawn(move || {
        listener.set_nonblocking(true).unwrap();
        let deadline = Instant::now() + T;
        let mut requests = 0;
        while requests < 2 {
            let mut socket = match listener.accept() {
                Ok((s, _)) => s,
                Err(e)
                    if e.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < deadline =>
                {
                    std::thread::sleep(Duration::from_millis(2));
                    continue;
                }
                other => panic!("SSE fixture did not receive initialize/notification: {other:?}"),
            };
            socket.set_nonblocking(false).unwrap();
            socket.set_read_timeout(Some(T)).unwrap();
            socket.set_write_timeout(Some(T)).unwrap();
            let mut buf = [0; 8192];
            let n = socket.read(&mut buf).unwrap();
            assert!(n > 0);
            if requests == 0 {
                socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n").unwrap();
                // Each SSE comment is small; the full stream deliberately exceeds JSON's 16MiB cap.
                let comment = format!(":{}\n\n", "x".repeat(16 * 1024));
                for _ in 0..1025 {
                    write!(socket, "{:x}\r\n{comment}\r\n", comment.len()).unwrap();
                }
                let event = format!(
                    "data: {}\n\n",
                    rmcp::serde_json::json!({"jsonrpc":"2.0","id":0,"result":{"protocolVersion":rmcp::model::ProtocolVersion::LATEST,"capabilities":{},"serverInfo":{"name":"bounded-sse","version":"1"}}})
                );
                write!(socket, "{:x}\r\n{event}\r\n0\r\n\r\n", event.len()).unwrap();
            } else {
                socket
                    .write_all(
                        b"HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    )
                    .unwrap();
            }
            requests += 1;
        }
    });
    let spec = openpaw_mcp::ServerSpec::Http {
        url: url.parse().unwrap(),
        headers: Default::default(),
    };
    let session = ServerSession::connect("streaming", &spec, T).await.unwrap();
    session.shutdown().await.unwrap();
    worker.join().unwrap();
}
