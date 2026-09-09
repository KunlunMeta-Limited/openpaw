use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use openpaw_core::{Error, ToolCallOutcome, ToolSpec};

use crate::hooks::HookChain;

type BoxedHandler = Arc<
    dyn Fn(
            PathBuf,
            serde_json::Value,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = openpaw_core::Result<ToolCallOutcome>> + Send>,
        > + Send
        + Sync,
>;

struct PluginToolEntry {
    spec: ToolSpec,
    handler: BoxedHandler,
    plugin_name: String,
}

pub struct PluginContext {
    tools: RwLock<HashMap<String, PluginToolEntry>>,
    hooks: HookChain,
    loaded_plugins: RwLock<Vec<String>>,
}

impl PluginContext {
    pub fn new() -> Self {
        Self {
            tools: RwLock::new(HashMap::new()),
            hooks: HookChain::new(),
            loaded_plugins: RwLock::new(Vec::new()),
        }
    }

    pub fn register_tool<F, Fut>(
        &self,
        plugin_name: &str,
        spec: ToolSpec,
        handler: F,
    ) -> openpaw_core::Result<()>
    where
        F: Fn(PathBuf, serde_json::Value) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = openpaw_core::Result<ToolCallOutcome>> + Send + 'static,
    {
        let name = spec.name.clone();
        let entry = PluginToolEntry {
            spec,
            handler: Arc::new(move |ws, input| Box::pin(handler(ws, input))),
            plugin_name: plugin_name.to_string(),
        };
        let mut tools = self.tools.write().unwrap_or_else(|e| e.into_inner());
        if tools.contains_key(&name) {
            tracing::warn!(name=%name, plugin=%plugin_name, "overwriting plugin tool");
        }
        tools.insert(name, entry);
        Ok(())
    }

    pub fn unregister_plugin(&self, plugin_name: &str) -> openpaw_core::Result<usize> {
        // Acquire both locks before modifying to avoid inconsistent state
        // if the second lock acquisition fails.
        // Use consistent lock poisoning recovery (into_inner)
        // across all methods in this struct.
        let mut tools = self.tools.write().unwrap_or_else(|e| e.into_inner());
        let mut loaded = self
            .loaded_plugins
            .write()
            .unwrap_or_else(|e| e.into_inner());

        let before = tools.len();
        tools.retain(|_, entry| entry.plugin_name != plugin_name);
        let removed = before - tools.len();

        loaded.retain(|n| n != plugin_name);

        Ok(removed)
    }

    pub fn list_plugin_tools(&self) -> Vec<ToolSpec> {
        let tools = self.tools.read().unwrap_or_else(|e| e.into_inner());
        let mut specs: Vec<ToolSpec> = tools.values().map(|e| e.spec.clone()).collect();
        specs.sort_by(|a, b| a.name.cmp(&b.name));
        specs
    }

    pub async fn call_tool(
        &self,
        name: &str,
        workspace: PathBuf,
        input: serde_json::Value,
    ) -> openpaw_core::Result<ToolCallOutcome> {
        let handler_and_ws = {
            let tools = self.tools.read().unwrap_or_else(|e| e.into_inner());
            tools
                .get(name)
                .map(|entry| (Arc::clone(&entry.handler), workspace))
        };

        if let Some((handler, ws)) = handler_and_ws {
            handler(ws, input).await
        } else {
            Err(Error::ToolHost(format!("plugin tool not found: {name}")))
        }
    }

    pub fn hook_chain(&self) -> &HookChain {
        &self.hooks
    }

    pub fn loaded_plugins(&self) -> Vec<String> {
        self.loaded_plugins
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn mark_loaded(&self, plugin_name: &str) -> openpaw_core::Result<()> {
        let mut loaded = self
            .loaded_plugins
            .write()
            .unwrap_or_else(|e| e.into_inner());
        if !loaded.contains(&plugin_name.to_string()) {
            loaded.push(plugin_name.to_string());
        }
        Ok(())
    }
}

impl Default for PluginContext {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openpaw_core::ToolSpec;

    #[test]
    fn test_register_and_list() {
        let ctx = PluginContext::new();
        let spec = ToolSpec {
            name: "test_tool".into(),
            description: "Test".into(),
            input_schema: serde_json::json!({}),
            ..Default::default()
        };
        ctx.register_tool("test-plugin", spec, |_ws, _input| async move {
            Ok(ToolCallOutcome::ok("ok"))
        })
        .unwrap();

        let tools = ctx.list_plugin_tools();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "test_tool");
    }

    #[test]
    fn test_unregister_plugin() {
        let ctx = PluginContext::new();
        let spec1 = ToolSpec {
            name: "a".into(),
            description: "A".into(),
            input_schema: serde_json::json!({}),
            ..Default::default()
        };
        let spec2 = ToolSpec {
            name: "b".into(),
            description: "B".into(),
            input_schema: serde_json::json!({}),
            ..Default::default()
        };
        ctx.register_tool("p1", spec1, |_ws, _input| async move {
            Ok(ToolCallOutcome::ok("a"))
        })
        .unwrap();
        ctx.register_tool("p2", spec2, |_ws, _input| async move {
            Ok(ToolCallOutcome::ok("b"))
        })
        .unwrap();

        let removed = ctx.unregister_plugin("p1").unwrap();
        assert_eq!(removed, 1);
        assert_eq!(ctx.list_plugin_tools().len(), 1);
    }

    #[tokio::test]
    async fn test_call_tool() {
        let ctx = PluginContext::new();
        let spec = ToolSpec {
            name: "echo".into(),
            description: "Echo".into(),
            input_schema: serde_json::json!({}),
            ..Default::default()
        };
        ctx.register_tool("echo-plugin", spec, |_ws, input| async move {
            let msg = input["msg"].as_str().unwrap_or("");
            Ok(ToolCallOutcome::ok(msg))
        })
        .unwrap();

        let result = ctx
            .call_tool(
                "echo",
                PathBuf::from("/tmp"),
                serde_json::json!({"msg": "hello"}),
            )
            .await
            .unwrap();
        assert_eq!(result.content, "hello");
    }

    #[tokio::test]
    async fn test_call_missing_tool() {
        let ctx = PluginContext::new();
        let result = ctx
            .call_tool("missing", PathBuf::from("/tmp"), serde_json::json!({}))
            .await;
        assert!(result.is_err());
        // Should be ToolHost error, not Provider error
        assert!(matches!(result.unwrap_err(), Error::ToolHost(_)));
    }

    #[test]
    fn test_unregister_removes_from_loaded_plugins() {
        let ctx = PluginContext::new();
        let spec = ToolSpec {
            name: "my_tool".into(),
            description: "Test".into(),
            input_schema: serde_json::json!({}),
            ..Default::default()
        };
        ctx.register_tool("my-plugin", spec, |_ws, _input| async move {
            Ok(ToolCallOutcome::ok("ok"))
        })
        .unwrap();
        ctx.mark_loaded("my-plugin").unwrap();

        // Verify loaded
        assert!(ctx.loaded_plugins().contains(&"my-plugin".to_string()));
        assert_eq!(ctx.list_plugin_tools().len(), 1);

        // Unregister
        let removed = ctx.unregister_plugin("my-plugin").unwrap();
        assert_eq!(removed, 1);

        // Both tools and loaded_plugins should be cleaned up atomically
        assert!(!ctx.loaded_plugins().contains(&"my-plugin".to_string()));
        assert_eq!(ctx.list_plugin_tools().len(), 0);
    }

    #[test]
    fn test_unregister_nonexistent_plugin() {
        let ctx = PluginContext::new();
        let removed = ctx.unregister_plugin("nonexistent").unwrap();
        assert_eq!(removed, 0);
    }
}
