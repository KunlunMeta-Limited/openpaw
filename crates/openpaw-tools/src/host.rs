//! ToolHost register-dispatch layer (E2 first cut of the rewrite).
//!
//! Twenty-one tools are registered with static specs; dispatch is a name
//! match to the per-tool `run`. Validation runs before dispatch; unknown
//! tools and bad args surface as error outcomes, never aborting a batch.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use openpaw_core::{
    Error, Result, ToolCallOutcome, ToolExecutor, ToolHost, ToolHostValidator, ToolSpec,
};

use crate::exec::{bash, clipboard, sleep};
use crate::fs::{edit, glob, grep, notebook_edit, read, write};
use crate::meta::{clarify, plan_mode, session_search, structured_output, think, todo};
use crate::web::{web_extract, web_fetch, web_search};

/// The builtin tool host: static registry + name dispatch over the landed
/// tool set.
pub struct ToolRegistry {
    workspace: PathBuf,
    specs: Vec<ToolSpec>,
    media: std::sync::RwLock<std::collections::HashMap<String, openpaw_core::InlineMedia>>,
}

impl ToolRegistry {
    /// Register the landed tool set for a workspace.
    pub fn new(workspace: impl Into<PathBuf>) -> Self {
        let mut specs = vec![
            read::spec(),
            write::spec(),
            edit::spec(),
            grep::spec(),
            glob::spec(),
            notebook_edit::spec(),
            think::spec(),
        ];
        specs.extend(todo::specs());
        specs.push(clarify::spec());
        specs.extend(plan_mode::specs());
        specs.push(structured_output::spec());
        specs.push(session_search::spec());
        specs.push(crate::meta::read_attachment::spec());
        specs.push(bash::spec());
        specs.push(clipboard::spec());
        specs.push(sleep::spec());
        specs.push(web_fetch::spec());
        specs.push(web_search::spec());
        specs.push(web_extract::spec());
        Self {
            workspace: workspace.into(),
            specs,
            media: Default::default(),
        }
    }

    /// Specs of the registered tools.
    pub fn tool_names(&self) -> Vec<String> {
        self.specs.iter().map(|s| s.name.clone()).collect()
    }
}

#[async_trait]
impl ToolExecutor for ToolRegistry {
    fn set_media_context(&self, media: Vec<openpaw_core::InlineMedia>) {
        *self
            .media
            .write()
            .unwrap_or_else(|error| error.into_inner()) = media
            .into_iter()
            .map(|item| (item.id.clone(), item))
            .collect();
    }
    fn remember_media(&self, media: Vec<openpaw_core::InlineMedia>) {
        let mut cache = self
            .media
            .write()
            .unwrap_or_else(|error| error.into_inner());
        for item in media {
            cache.entry(item.id.clone()).or_insert(item);
        }
    }
    async fn list_tools(&self) -> Result<Vec<ToolSpec>> {
        Ok(self.specs.clone())
    }

    async fn call(&self, name: &str, args: serde_json::Value) -> Result<ToolCallOutcome> {
        let ws = self.workspace.clone();
        match name {
            "read_attachment" => {
                let media = args
                    .get("attachmentID")
                    .and_then(|value| value.as_str())
                    .and_then(|id| {
                        self.media
                            .read()
                            .unwrap_or_else(|error| error.into_inner())
                            .get(id)
                            .cloned()
                    });
                match media {
                    Some(media) => {
                        media.bytes()?;
                        let mut outcome = ToolCallOutcome::ok(format!(
                            "Attachment {} ({})",
                            media.id, media.mime
                        ));
                        outcome.media.push(media);
                        Ok(outcome)
                    }
                    None => Ok(ToolCallOutcome::error(
                        "attachment not found in current visible session",
                    )),
                }
            }
            "read_file" => read::run(&ws, args).await,
            "write_file" => write::run(&ws, args).await,
            "edit_file" => edit::run(&ws, args).await,
            "notebook_edit" => notebook_edit::run(&ws, args).await,
            "grep" => grep::run(&ws, args).await,
            "glob" => glob::run(&ws, args).await,
            "bash" => bash::run(&ws, args).await,
            "clipboard" => clipboard::run(&ws, args).await,
            "sleep" => sleep::run(&ws, args).await,
            "web_fetch" => web_fetch::run(&ws, args).await,
            "web_search" => web_search::run(&ws, args).await,
            "web_extract" => web_extract::run(&ws, args).await,
            "think" => think::run(&ws, args).await,
            "todo_add" | "todo_update" | "todo_list" => todo::run(&ws, name, args).await,
            "clarify" => clarify::run(&ws, args).await,
            "enter_plan_mode" | "exit_plan_mode" => plan_mode::run(&ws, name).await,
            "structured_output" => structured_output::run(&ws, args).await,
            "session_search" => session_search::run(&ws, args).await,
            other => Err(Error::ToolHost(format!("unknown tool: {other}"))),
        }
    }
}

#[async_trait]
impl ToolHostValidator for ToolRegistry {}

/// Convenience constructor matching the engine's `Arc<dyn ToolHost>` seam.
pub fn arc_registry(workspace: impl Into<PathBuf>) -> Arc<dyn ToolHost> {
    Arc::new(ToolRegistry::new(workspace))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn registry_lists_and_dispatches() {
        let dir = tempfile::tempdir().unwrap();
        let host = ToolRegistry::new(dir.path());

        assert_eq!(host.tool_names().len(), 22);
        let listed = host.list_tools().await.unwrap();
        assert_eq!(listed.len(), 22);

        // Roundtrip through the host: write then read.
        host.call(
            "write_file",
            json!({"path": "hello.txt", "content": "hi\n"}),
        )
        .await
        .unwrap();
        let out = host
            .call("read_file", json!({"path": "hello.txt"}))
            .await
            .unwrap();
        assert!(out.content.contains("hi"));
        assert!(!out.is_error);
    }

    #[tokio::test]
    async fn unknown_tool_errors() {
        let dir = tempfile::tempdir().unwrap();
        let host = ToolRegistry::new(dir.path());
        let err = host.call("vault_http_fetch", json!({})).await.unwrap_err();
        assert!(err.to_string().contains("unknown tool"));
    }

    #[tokio::test]
    async fn bad_args_surface_as_error_outcomes() {
        let dir = tempfile::tempdir().unwrap();
        let host = ToolRegistry::new(dir.path());
        // Bad args are an error *outcome* (never an Err) so a single
        // malformed call cannot abort a tool batch.
        let out = host.call("read_file", json!({"nope": 1})).await.unwrap();
        assert!(out.is_error);
        assert!(out.content.contains("bad args"));
    }
}
