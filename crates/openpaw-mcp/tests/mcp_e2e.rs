//! MC-2 e2e: handshake, tool calls, resources, call timeout, circuit
//! breaker, and reconnection — fully in-process. A mock rmcp server runs
//! on one end of a duplex pair; `ServerSession::connect_over_transport`
//! takes the other, so no external process or network is involved.
use openpaw_mcp::server::ServerSession;
use openpaw_mcp::{CircuitBreaker, CircuitState, McpError};
use rmcp::model::{
    Annotated, CallToolResult, Content, JsonObject, ListResourcesResult, ListToolsResult,
    RawResource, ReadResourceResult, ServerCapabilities, ServerInfo, Tool,
};
use rmcp::service::RequestContext;
use rmcp::transport::async_rw::AsyncRwTransport;
use rmcp::{ErrorData, RoleServer, ServerHandler, ServiceExt};
use std::sync::Arc;
use std::time::Duration;
const T: Duration = Duration::from_secs(5);
const CALL_TIMEOUT: Duration = Duration::from_millis(400);
/// A controllable MCP server: `echo` answers, `fail` answers with a
/// tool-level error result, everything else is invalid. `call_delay`
/// stalls every call (timeout tests).
struct MockServer {
    call_delay: Option<Duration>,
}
impl ServerHandler for MockServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(
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
    ) -> Result<CallToolResult, ErrorData> {
        if let Some(delay) = self.call_delay {
            tokio::time::sleep(delay).await;
        }
        let args = request.arguments.unwrap_or_default();
        match request.name.as_ref() {
            "echo" => {
                let text = args
                    .get("text")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                Ok(CallToolResult::success(vec![Content::text(format!(
                    "echo: {text}"
                ))]))
            }
            "fail" => Ok(CallToolResult::error(vec![Content::text(
                "mock tool failure",
            )])),
            _ => Ok(CallToolResult::error(vec![Content::text(format!(
                "unknown tool {}",
                request.name
            ))])),
        }
    }
    async fn list_resources(
        &self,
        _request: Option<rmcp::model::PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        let resource = Annotated::new(
            RawResource::new("mock://doc/1", "Mock Document").with_mime_type("text/plain"),
            None,
        );
        Ok(ListResourcesResult {
            resources: vec![resource],
            next_cursor: None,
            meta: None,
        })
    }
    async fn read_resource(
        &self,
        request: rmcp::model::ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResult, ErrorData> {
        if request.uri != "mock://doc/1" {
            return Err(ErrorData::invalid_params(
                format!("no resource {}", request.uri),
                None,
            ));
        }
        Ok(ReadResourceResult::new(vec![
            rmcp::model::ResourceContents::TextResourceContents {
                uri: "mock://doc/1".into(),
                mime_type: Some("text/plain".into()),
                text: "mock body".into(),
                meta: None,
            },
        ]))
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
/// Wire a mock server and a connected client session over one duplex
/// pair. The server task dies with the handle (abort), which is exactly
/// how the breaker test simulates a server crash.
async fn mock_pair(call_delay: Option<Duration>) -> (ServerSession, tokio::task::JoinHandle<()>) {
    let (client_io, server_io) = tokio::io::duplex(1 << 16);
    let (client_read, client_write) = tokio::io::split(client_io);
    let (server_read, server_write) = tokio::io::split(server_io);
    let server = MockServer { call_delay };
    let handle = tokio::spawn(async move {
        let transport = AsyncRwTransport::new_server(server_read, server_write);
        match server.serve(transport).await {
            Ok(running) => {
                let _ = running.waiting().await;
            }
            Err(e) => eprintln!("mock server failed to start: {e}"),
        }
    });
    let session = ServerSession::connect_over_transport(
        "mock",
        AsyncRwTransport::new_client(client_read, client_write),
        T,
    )
    .await
    .expect("in-process handshake");
    (session, handle)
}
#[tokio::test]
async fn handshake_and_list_tools() {
    let (session, handle) = mock_pair(None).await;
    let tools = session.list_tools(T).await.unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t.name.as_ref()).collect();
    assert_eq!(names, vec!["echo", "fail"]);
    session.shutdown().await.unwrap();
    handle.abort();
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
    handle.abort();
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
    handle.abort();
}
#[tokio::test]
async fn resources_list_and_read() {
    let (session, handle) = mock_pair(None).await;
    let resources = session.list_resources(T).await.unwrap();
    assert_eq!(resources.len(), 1);
    let read = session.read_resource("mock://doc/1", T).await.unwrap();
    assert_eq!(read.contents.len(), 1);
    session.shutdown().await.unwrap();
    handle.abort();
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
    handle.abort();
}
#[tokio::test]
async fn breaker_trips_on_transport_failure_then_reconnects() {
    let (session, handle) = mock_pair(None).await;
    let mut breaker = CircuitBreaker::new(3, Duration::from_millis(120));
    // Crash the server: every subsequent call is a transport failure.
    handle.abort();
    let _ = handle.await;
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
    handle.abort();
}
