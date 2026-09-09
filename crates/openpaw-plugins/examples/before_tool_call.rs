//! Minimal example: register and trigger a `BeforeToolCall` hook.
//!
//! Run: `cargo run --example before_tool_call -p openpaw-plugins`

use openpaw_plugins::{HookChain, HookDecision, HookPoint, HookResult};

#[tokio::main]
async fn main() {
    let mut chain = HookChain::new();

    // A plugin hooks in before every tool call and redacts secrets.
    chain.register(
        HookPoint::BeforeToolCall,
        "redact_secrets",
        |ctx| async move {
            println!("[hook:before_tool_call] saw payload: {}", ctx.payload);
            let mut redacted = ctx.payload.clone();
            if let Some(obj) = redacted.as_object_mut() {
                if obj.remove("api_key").is_some() {
                    println!("[hook:before_tool_call] redacted `api_key`");
                }
            }
            HookResult {
                decision: HookDecision::Continue,
                modified_payload: Some(redacted),
            }
        },
    );

    let payload = serde_json::json!({
        "tool": "http_get",
        "url": "https://example.com",
        "api_key": "hunter2",
    });
    println!("[host] dispatching tool call: {payload}");

    match chain.run(HookPoint::BeforeToolCall, payload).await {
        HookDecision::Replace(value) => {
            println!("[host] final payload after chain: {value}");
            println!("[host] secret removed: {}", value.get("api_key").is_none());
        }
        other => println!("[host] chain returned {other:?}"),
    }
}
