//! Minimal example: register and trigger an `AfterReflect` hook.
//!
//! Run: `cargo run --example after_reflect -p openpaw-plugins`

use openpaw_plugins::{HookChain, HookDecision, HookPoint, HookResult};

#[tokio::main]
async fn main() {
    let mut chain = HookChain::new();

    // A plugin tags every reflection output with an audit marker before it
    // is persisted.
    chain.register(HookPoint::AfterReflect, "audit_tagger", |ctx| async move {
        let summary = ctx.payload["summary"].as_str().unwrap_or("<empty>");
        println!("[hook:after_reflect] reviewing reflection: {summary}");
        let mut tagged = ctx.payload.clone();
        if let Some(obj) = tagged.as_object_mut() {
            let _ = obj.insert("audited".into(), serde_json::json!(true));
        }
        HookResult {
            decision: HookDecision::Continue,
            modified_payload: Some(tagged),
        }
    });

    let reflection = serde_json::json!({
        "summary": "prefer streaming reads for large files",
        "confidence": 0.82,
    });
    println!("[host] reflection produced: {reflection}");

    match chain.run(HookPoint::AfterReflect, reflection).await {
        HookDecision::Replace(value) => {
            println!("[host] persisted reflection: {value}");
            println!(
                "[host] audit tag present: {}",
                value["audited"].as_bool() == Some(true)
            );
        }
        other => println!("[host] chain returned {other:?}"),
    }
}
