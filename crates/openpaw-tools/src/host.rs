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
/// 用途：原工具注册和分派表
/// 字段：工作区、原spec、注入环境与可等待观察器
/// 来源去向：引擎到已有工具主体
/// 约束：不复制工具实现，不保存永久批准
/// 稳定性：新增协议无关接口
/// 关联：需求/设计 DES-OP-004；REQ-OP-005。
/// Stability: unstable.
pub struct ToolRegistry {
    workspace: PathBuf,
    environment: Arc<dyn crate::exec::environment::Environment>,
    observer: Option<Arc<dyn openpaw_core::ToolExecutionObserver>>,
    specs: Vec<ToolSpec>,
}

impl ToolRegistry {
    /// Register the landed tool set for a workspace.
    /// 功能：注册现有工具并使用默认本地环境
    /// 参数：workspace为调用方已授权根目录
    /// 返回：原工具注册表
    /// 错误：无；路径在调用时验证
    /// 状态：只注册声明，不创建工作区文件
    /// 并发与取消：注册后可共享，运行上下文不共享
    /// 权限：不进行人工批准；上下文由既有引擎传入
    /// 关联：需求/设计 DES-OP-004；REQ-OP-005。
    /// Stability: unstable.
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
        specs.push(bash::spec());
        specs.push(clipboard::spec());
        specs.push(sleep::spec());
        specs.push(web_fetch::spec());
        specs.push(web_search::spec());
        specs.push(web_extract::spec());
        // 原写工具已有分类/破坏标记；注册时落实批准要求，不能仅展示metadata。
        for spec in &mut specs {
            if spec.is_destructive == Some(true)
                || matches!(
                    spec.category,
                    Some(
                        openpaw_core::ToolCategory::Write | openpaw_core::ToolCategory::Destructive
                    )
                )
            {
                spec.requires_approval = true;
            }
        }
        Self {
            workspace: workspace.into(),
            environment: Arc::new(crate::exec::environment::LocalEnvironment),
            observer: None,
            specs,
        }
    }

    /// 功能：注入原命令Environment，避免注册表固定本地执行
    /// 参数：environment为调用方选定后端
    /// 返回：当前注册表
    /// 错误：无
    /// 状态：只更新后端引用
    /// 并发与取消：共享线程安全环境，不共享Run授权
    /// 权限：受限策略失败不能退回本地
    /// 关联：需求/设计 DES-OP-004；REQ-OP-005。
    /// Stability: unstable.
    pub fn with_environment(
        mut self,
        environment: Arc<dyn crate::exec::environment::Environment>,
    ) -> Self {
        self.environment = environment;
        self
    }
    /// 功能：注入真正执行前的可靠观察器
    /// 参数：observer为宿主存储接口
    /// 返回：当前注册表
    /// 错误：无
    /// 状态：只保存观察引用
    /// 并发与取消：每次执行等待观察写入完成；失败不执行
    /// 权限：不提供授权，不能改写输入
    /// 关联：需求/设计 DES-OP-004；REQ-OP-005。
    /// Stability: unstable.
    pub fn with_execution_observer(
        mut self,
        observer: Arc<dyn openpaw_core::ToolExecutionObserver>,
    ) -> Self {
        self.observer = Some(observer);
        self
    }
    /// Specs of the registered tools.
    pub fn tool_names(&self) -> Vec<String> {
        self.specs.iter().map(|s| s.name.clone()).collect()
    }
}

#[async_trait]
impl ToolExecutor for ToolRegistry {
    async fn list_tools(&self) -> Result<Vec<ToolSpec>> {
        Ok(self.specs.clone())
    }

    async fn call(&self, name: &str, args: serde_json::Value) -> Result<ToolCallOutcome> {
        let ws = self.workspace.clone();
        match name {
            "read_file" => read::run(&ws, args).await,
            "write_file" => write::run(&ws, args).await,
            "edit_file" => edit::run(&ws, args).await,
            "notebook_edit" => notebook_edit::run(&ws, args).await,
            "grep" => grep::run(&ws, args).await,
            "glob" => glob::run(&ws, args).await,
            "bash" => bash::run_with_env(&ws, args, &*self.environment).await,
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
    async fn call_cancellable(
        &self,
        name: &str,
        args: serde_json::Value,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<ToolCallOutcome> {
        let context = openpaw_core::ToolCallContext::new("legacy", "legacy", name, &args, cancel);
        self.call_with_context(name, args, context).await
    }
    async fn call_with_context(
        &self,
        name: &str,
        args: serde_json::Value,
        mut context: openpaw_core::ToolCallContext,
    ) -> Result<ToolCallOutcome> {
        self.validate_input(name, &args)
            .await
            .map_err(Error::ToolHost)?;
        if let Some(spec) = self.specs.iter().find(|s| s.name == name) {
            if spec.requires_approval && !context.is_approved_for(name, &args) {
                return Err(Error::ToolHost(
                    "this invocation requires approval for its exact input".into(),
                ));
            }
        }
        if name == "bash" {
            if let Err(outcome) = bash::check_call(&args, &*self.environment, Some(&context)) {
                return Ok(outcome);
            }
        }
        if let Some(observer) = &self.observer {
            context = context.with_observer(Arc::clone(observer));
        }
        if !matches!(name, "todo_add" | "todo_update") {
            context.execution_started(name, &args).await?;
        }
        match name {
            "bash" => {
                bash::run_with_context(
                    &self.workspace,
                    args,
                    &*self.environment,
                    Some(&context),
                    context.cancel(),
                )
                .await
            }
            "todo_add" | "todo_update" | "todo_list" => {
                todo::run_cancellable(
                    &self.workspace,
                    name,
                    args,
                    context.cancel(),
                    Some(&context),
                )
                .await
            }
            "sleep" => {
                tokio::select! {biased;_=context.cancel().cancelled_owned()=>Err(Error::Cancelled),result=self.call(name,args)=>result}
            }
            _ => self.call(name, args).await,
        }
    }
}

#[async_trait]
impl ToolHostValidator for ToolRegistry {
    async fn validate_input(
        &self,
        name: &str,
        input: &serde_json::Value,
    ) -> std::result::Result<(), String> {
        let spec = self
            .specs
            .iter()
            .find(|s| s.name == name)
            .ok_or_else(|| format!("unknown tool: {name}"))?;
        validate_schema(&spec.input_schema, input)?;
        if matches!(
            name,
            "read_file" | "write_file" | "edit_file" | "notebook_edit" | "grep" | "glob"
        ) {
            let path = input.get("path").and_then(|v| v.as_str()).unwrap_or(".");
            crate::safety::resolve(&self.workspace, path).map_err(|e| e.to_string())?;
        }
        Ok(())
    }
}
// 仅校验原内置工具Schema使用的关键字；不是另一个通用JSON Schema实现。
fn validate_schema(
    schema: &serde_json::Value,
    input: &serde_json::Value,
) -> std::result::Result<(), String> {
    if let Some(kind) = schema.get("type").and_then(|v| v.as_str()) {
        let valid = match kind {
            "object" => input.is_object(),
            "string" => input.is_string(),
            "integer" => input.as_i64().is_some() || input.as_u64().is_some(),
            "boolean" => input.is_boolean(),
            "array" => input.is_array(),
            _ => false,
        };
        if !valid {
            return Err(format!("expected {kind}"));
        }
    }
    if let Some(options) = schema.get("enum").and_then(|v| v.as_array()) {
        if !options.contains(input) {
            return Err("value is not in enum".into());
        }
    }
    for key in ["minimum", "maximum"] {
        if let (Some(bound), Some(value)) =
            (schema.get(key).and_then(|v| v.as_f64()), input.as_f64())
        {
            if (key == "minimum" && value < bound) || (key == "maximum" && value > bound) {
                return Err(format!("value violates {key}"));
            }
        }
    }
    if let Some(required) = schema.get("required").and_then(|v| v.as_array()) {
        for key in required.iter().filter_map(|v| v.as_str()) {
            if input.get(key).is_none() {
                return Err(format!("missing required field {key}"));
            }
        }
    }
    if let Some(properties) = schema.get("properties").and_then(|v| v.as_object()) {
        for (key, schema) in properties {
            if let Some(value) = input.get(key) {
                validate_schema(schema, value).map_err(|e| format!("{key}: {e}"))?;
            }
        }
    }
    if let (Some(items), Some(values)) = (schema.get("items"), input.as_array()) {
        for value in values {
            validate_schema(items, value)?;
        }
    }
    Ok(())
}

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

        assert_eq!(host.tool_names().len(), 21);
        let listed = host.list_tools().await.unwrap();
        assert_eq!(listed.len(), 21);

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
