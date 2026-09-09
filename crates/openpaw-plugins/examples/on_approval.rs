//! Minimal example: register and trigger an `OnApproval` hook.
//!
//! Run: `cargo run --example on_approval -p openpaw-plugins`

use openpaw_plugins::{HookChain, HookDecision, HookPoint, HookResult};

#[tokio::main]
async fn main() {
    let mut chain = HookChain::new();

    // First plugin: log every approval request.
    chain.register(HookPoint::OnApproval, "approval_logger", |ctx| async move {
        let tool = ctx.payload["tool"].as_str().unwrap_or("<unknown>");
        println!("[hook:on_approval] approval requested for tool: {tool}");
        HookResult {
            decision: HookDecision::Continue,
            modified_payload: None,
        }
    });

    // Second plugin: short-circuit the chain for destructive tools.
    chain.register(
        HookPoint::OnApproval,
        "destructive_deny",
        |ctx| async move {
            let destructive = ctx.payload["destructive"].as_bool().unwrap_or(false);
            if destructive {
                println!("[hook:on_approval] denying destructive request");
                return HookResult {
                    decision: HookDecision::Skip,
                    modified_payload: None,
                };
            }
            HookResult {
                decision: HookDecision::Continue,
                modified_payload: None,
            }
        },
    );

    for (tool, destructive) in [("fs_write", false), ("rm_rf", true)] {
        let payload = serde_json::json!({ "tool": tool, "destructive": destructive });
        println!("\n[host] requesting approval: {payload}");
        let decision = chain.run(HookPoint::OnApproval, payload).await;
        match decision {
            HookDecision::Skip => println!("[host] approval DENIED for {tool}"),
            HookDecision::Continue => println!("[host] approval GRANTED for {tool}"),
            HookDecision::Replace(v) => println!("[host] approval payload rewritten: {v:?}"),
        }
    }
}
