//! 功能：提供有界工具模型视图及授权历史原件分页能力。
//! 输入输出边界：入口提供当前会话读取器；Engine不依赖存储和传输。
//! 禁止承担：不访问任意文件、不创建权限或重跑原工具。
//! 关联：CHG-2026-0051。
use async_trait::async_trait;
use openpaw_core::{
    ContentBlock, Message, Result, ToolCallOutcome, ToolCategory, ToolConcurrency, ToolExecutor,
    ToolHost, ToolHostValidator, ToolSpec,
};
use serde_json::{json, Value};
use std::sync::Arc;

fn source_identity(messages: &[Message]) -> (Vec<String>, String) {
    let ids = messages
        .iter()
        .enumerate()
        .map(|(index, m)| m.uuid.clone().unwrap_or_else(|| format!("legacy/{index}")))
        .collect();
    let mut hash = 0xcbf29ce484222325u64;
    for byte in serde_json::to_vec(messages).unwrap_or_default() {
        hash = (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3);
    }
    (ids, format!("fnv1a64:{hash:016x}"))
}

/// 功能：校验检查点是否覆盖当前可见原件前缀及实际模型。
/// 参数：checkpoint为持久候选，raw为可见完整历史，owner/provider/model为可信身份。
/// 返回：适用时返回被覆盖消息数。
/// 错误：不适用返回None，原件不改变。
/// 状态：只读。
/// 并发与取消：同步独占快照。
/// 权限：校验不授予读取或执行许可。
/// 关联：Host加载、回退及模型切换。
pub fn checkpoint_prefix(
    checkpoint: &openpaw_core::ModelContextCheckpoint,
    raw: &[Message],
    owner: &str,
    provider: &str,
    model: &str,
) -> Option<usize> {
    let count = checkpoint.source_ids.len();
    if checkpoint.version != 1
        || checkpoint.owner != owner
        || checkpoint.provider != provider
        || checkpoint.model != model
        || count == 0
        || count > raw.len()
        || checkpoint.messages.is_empty()
    {
        return None;
    }
    let (ids, digest) = source_identity(&raw[..count]);
    (ids == checkpoint.source_ids && digest == checkpoint.source_digest).then_some(count)
}

/// 功能：从适用检查点与可见后缀恢复有界模型上下文。
/// 参数：session为目标会话，raw为完整可见原件，checkpoints按旧到新排序，provider/model为实际选择。
/// 返回：无。
/// 错误：不适用时安全从原件重建。
/// 状态：仅替换模型视图和可选检查点。
/// 并发与取消：同步，调用方独占会话。
/// 权限：分支或隐藏原件不能通过检查点进入上下文。
/// 关联：Host与CLI恢复。
pub fn restore_context(
    session: &mut openpaw_core::Session,
    raw: &[Message],
    checkpoints: &[openpaw_core::ModelContextCheckpoint],
    provider: &str,
    model: &str,
) {
    let applicable = checkpoints
        .iter()
        .rev()
        .find_map(|c| checkpoint_prefix(c, raw, &session.meta.id, provider, model).map(|n| (c, n)));
    session.messages = if let Some((c, n)) = applicable {
        c.messages.iter().chain(&raw[n..]).cloned().collect()
    } else {
        raw.to_vec()
    };
    session.model_context = applicable.map(|(c, _)| c.clone());
    project_history(&mut session.messages, &session.meta.id, usize::MAX);
}

/// 功能：在稳定消息身份映射后封装本次可发布检查点。
/// 参数：session为成功回合模型视图，raw为同一事务提交后的完整可见原件。
/// 返回：有整理状态时返回版本事件，无整理时返回None。
/// 错误：无；调用方必须与原件在同一事务提交。
/// 状态：更新候选来源边界，不自行发布。
/// 并发与取消：同步；错误取消回合不得调用。
/// 权限：不把摘要当作真实用户或工具消息。
/// 关联：Store finish与CLI消息追加。
pub fn seal_context(
    session: &mut openpaw_core::Session,
    raw: &[Message],
) -> Option<openpaw_core::SessionEvent> {
    let checkpoint = session.model_context.as_mut()?;
    let (ids, digest) = source_identity(raw);
    checkpoint.source_ids = ids;
    checkpoint.source_digest = digest;
    checkpoint.messages = session.messages.clone();
    let mut value = serde_json::to_value(checkpoint).ok()?;
    value["type"] = json!("model_context_checkpoint_v1");
    Some(openpaw_core::SessionEvent::Unknown(Box::new(value)))
}

/// 功能：把模型视图与摘要内消息引用映射到入口最终稳定身份。
/// 参数：session为模型副本，ids为原生成身份到提交身份的映射。
/// 返回：无。
/// 错误：无；缺少映射保持原样。
/// 状态：不修改真实原件或执行记录。
/// 并发与取消：提交前同步独占。
/// 权限：只变更定位键，不新增可见内容。
/// 关联：Host稳定ID与检查点封装。
pub fn remap_context(
    session: &mut openpaw_core::Session,
    ids: &std::collections::HashMap<String, Option<String>>,
) {
    for message in &mut session.messages {
        if let Some(id) = message.uuid.as_ref().and_then(|id| ids.get(id)) {
            message.uuid = id.clone();
        }
        for block in &mut message.content {
            let text = match block {
                ContentBlock::Text { text } => text,
                ContentBlock::ToolResult { content, .. } => content,
                _ => continue,
            };
            for (old, new) in ids {
                if let Some(new) = new {
                    *text = text.replace(&format!("message/{old}"), &format!("message/{new}"));
                }
            }
        }
    }
}

/// 功能：入口校验可见性后返回的完整原始文本与成功状态。
/// Stability: experimental；读取权限仍由入口负责。
pub type OriginalReader =
    Arc<dyn Fn(&str) -> std::result::Result<(String, bool), String> + Send + Sync>;

/// 功能：构造稳定且不会因模型call_id重复而跨回合冲突的记录引用。
/// 参数：owner为会话身份，turn/response/call为可靠事实的定位键。
/// 返回：版本化不透明引用。
/// 错误：无。
/// 状态：无。
/// 并发与取消：同步无共享状态。
/// 权限：引用不是授权，读取时仍须核对可见性。
/// 关联：ExecutionRecorder、read_context_record。
pub fn record_reference(owner: &str, turn: &str, response: usize, call: &str) -> String {
    format!("ctx:v1:{}", json!([owner, turn, response, call]))
}

/// 功能：保存已验证版本和会话归属的原件定位键，不包含读取许可。
/// Stability: experimental；随版本化原件引用演进。
pub struct OriginalRecordKey {
    /// 功能：可靠执行回合或消息、Goal命名空间。
    pub turn: String,
    /// 功能：该回合内的响应序号。
    pub response: u64,
    /// 功能：响应内的工具调用或正文定位键。
    pub call: String,
}

/// 功能：严格解析稳定原件引用并核对会话归属。
/// 参数：reference为不透明引用，owner为可信入口会话身份。
/// 返回：已验证结构的定位键。
/// 错误：结构、版本或归属不匹配明确失败。
/// 状态：只读。
/// 并发与取消：同步。
/// 权限：解析不代替可见性检查。
/// 关联：Host与默认CLI，CLI无需新增JSON依赖。
pub fn parse_reference(
    reference: &str,
    owner: &str,
) -> std::result::Result<OriginalRecordKey, String> {
    let (source_owner, turn, response, call): (String, String, u64, String) =
        serde_json::from_str(reference.strip_prefix("ctx:v1:").ok_or("无效原件引用")?)
            .map_err(|_| "无效原件引用")?;
    if source_owner != owner {
        return Err("原件不属于当前会话".into());
    }
    Ok(OriginalRecordKey {
        turn,
        response,
        call,
    })
}

/// 功能：在入口提供的可见消息内查找稳定原件。
/// 参数：key为已校验归属的定位键，visible为授权可见原件。
/// 返回：精确文本及错误状态。
/// 错误：引用不匹配或正文已隐藏时失败。
/// 状态：只读。
/// 并发与取消：同步快照读取。
/// 权限：只读取visible，不访问文件或其他会话。
/// 关联：Host与CLI原件工具。
pub fn message_original(
    key: &OriginalRecordKey,
    visible: &[Message],
) -> std::result::Result<(String, bool), String> {
    let id = key.turn.strip_prefix("message/").ok_or("无效消息引用")?;
    let message = visible
        .iter()
        .find(|m| m.uuid.as_deref() == Some(id))
        .or_else(|| {
            id.strip_prefix("legacy/")
                .and_then(|s| s.parse::<usize>().ok())
                .and_then(|index| visible.get(index))
                .filter(|m| m.uuid.is_none())
        })
        .ok_or("原件不存在或已隐藏")?;
    if key.call == "__text" {
        return Ok((message.text(), false));
    }
    message
        .content
        .iter()
        .find_map(|block| match block {
            ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
                ..
            } if key.call == *tool_use_id => Some((content.clone(), *is_error)),
            _ => None,
        })
        .ok_or_else(|| "原件不存在或已隐藏".into())
}

fn prefix(text: &str, chars: usize, bytes: usize) -> String {
    let mut end = 0;
    for (index, c) in text.char_indices().take(chars) {
        if index + c.len_utf8() > bytes {
            break;
        }
        end = index + c.len_utf8();
    }
    text[..end].to_owned()
}

fn bounded_content(mut value: Value, text: &str, chars: usize, bytes: usize) -> String {
    let mut low = 0;
    let mut high = text.chars().count().min(chars);
    value["content"] = json!("");
    let mut best = value.to_string();
    while low <= high {
        let middle = low + (high - low) / 2;
        value["content"] = json!(prefix(text, middle, bytes));
        let candidate = value.to_string();
        if candidate.chars().count() <= chars && candidate.len() <= bytes {
            best = candidate;
            low = middle + 1;
        } else if middle == 0 {
            break;
        } else {
            high = middle - 1;
        }
    }
    best
}

/// 功能：只为模型裁剪工具副本，保留原件引用与准确大小。
/// 参数：blocks为模型副本，owner/turn/response标识已可靠保存的调用，window为实际窗口。
/// 返回：无。
/// 错误：无；读取原件由入口独立校验。
/// 状态：不修改真实generated和执行事实。
/// 并发与取消：同步；必须在可靠记录成功后调用。
/// 权限：只生成当前会话引用，不授权其他会话。
/// 关联：AgentLoop工具批次续轮。
pub fn project_results(
    blocks: &mut [ContentBlock],
    owner: &str,
    turn: &str,
    response: usize,
    window: usize,
) {
    let chars = (window / 8).clamp(128, 6000);
    let bytes = (window / 2).clamp(1024, 24 * 1024);
    for block in blocks {
        if let ContentBlock::ToolResult {
            tool_use_id,
            content,
            is_error,
            name,
            protected,
            ..
        } = block
        {
            if *protected
                || matches!(
                    name.as_deref(),
                    Some("read_context_record" | "goal_read" | "goal_update")
                )
                || (content.chars().count() <= chars && content.len() <= bytes)
            {
                continue;
            }
            // 已投影的检查点再次缩小时保留原件定位与总量，不把预览当成新原件。
            // 引用只供模型定位；实际读取仍须由入口验证归属及可见性。
            let previous = serde_json::from_str::<Value>(content).ok().filter(|v| {
                v["kind"] == "tool_result_preview"
                    && v["reference"]
                        .as_str()
                        .is_some_and(|r| parse_reference(r, owner).is_ok())
            });
            let (metadata, text) = if let Some(previous) = previous {
                let text = previous["content"].as_str().unwrap_or("").to_owned();
                (previous, text)
            } else {
                (
                    json!({"kind":"tool_result_preview","reference":record_reference(owner,turn,response,tool_use_id),
                "total_chars":content.chars().count(),"total_bytes":content.len(),
                "is_error":is_error,"truncated":true,"read":"read_context_record(reference,start=0,limit<=4096)"}),
                    content.clone(),
                )
            };
            *content = bounded_content(metadata, &text, chars, bytes);
        }
    }
}

/// 功能：按稳定消息身份为旧完整历史建立有界模型副本。
/// 参数：messages为模型消息，owner为授权会话，window为当前实际窗口。
/// 返回：无。
/// 错误：无；无UUID旧消息使用当前可见前缀位置建立稳定模型定位键。
/// 状态：只更新副本，不重写transcript。
/// 并发与取消：同步，调用方独占模型视图。
/// 权限：读取时仍须验证消息当前可见。
/// 关联：旧会话加载及检查点恢复。
pub fn project_history(messages: &mut [Message], owner: &str, window: usize) {
    for (index, message) in messages.iter_mut().enumerate() {
        if message.uuid.is_none() {
            message.uuid = Some(format!("legacy/{index}"));
        }
        if let Some(id) = &message.uuid {
            project_results(
                &mut message.content,
                owner,
                &format!("message/{id}"),
                0,
                window,
            );
        }
    }
}

struct ContextToolHost {
    inner: Arc<dyn ToolHost>,
    reader: OriginalReader,
    page_chars: usize,
    page_bytes: usize,
}

/// 功能：估算完整工具声明的输入成本。
/// 参数：tools为实际发送的声明。
/// 返回：包含名字、描述、schema及协议余量的本地估算。
/// 错误：无；序列化失败采用空声明估算。
/// 状态：只读。
/// 并发与取消：同步。
/// 权限：不输出声明正文。
/// 关联：每次请求预算。
pub(crate) fn tools_tokens(tools: &[ToolSpec]) -> usize {
    crate::compaction::estimate_tokens(&serde_json::to_string(tools).unwrap_or_default())
        + tools.len() * 32
}

/// 功能：预留输出及估算误差后计算输入额度。
/// 参数：window为实际窗口，output为本次最大输出。
/// 返回：可用输入估算额度。
/// 错误：无；固定预留装不下时返回零。
/// 状态：无。
/// 并发与取消：同步。
/// 权限：不改变模型或额度。
/// 关联：自动整理和最终预检。
pub(crate) fn input_limit(window: usize, output: u32) -> usize {
    window
        .saturating_sub(output as usize)
        .saturating_sub((window / 20).max(1024))
}

/// 功能：在发送前检查完整请求的预算。
/// 参数：request为实际请求，window为实际模型窗口。
/// 返回：输入可容纳时为空。
/// 错误：固定输入或保留尾部超限时返回ContextTooLong。
/// 状态：不裁剪请求。
/// 并发与取消：同步，不发送模型请求。
/// 权限：错误不携带正文。
/// 关联：所有业务和最终答复请求。
pub(crate) fn validate_request(
    request: &openpaw_core::CompletionRequest,
    window: usize,
) -> Result<()> {
    let estimate = crate::compaction::estimate_messages_tokens(
        request.system.as_deref().unwrap_or(""),
        &request.messages,
        tools_tokens(&request.tools),
    );
    if estimate > input_limit(window, request.max_tokens) {
        return Err(openpaw_core::Error::context_too_long(
            "固定输入或必要近期内容无法放入模型窗口（已预留输出和估算余量）",
        ));
    }
    Ok(())
}

/// 功能：把只读原件工具接到现有权限和工具执行链。
/// 参数：inner为原工具宿主，reader为当前会话授权读取器，window为实际模型窗口。
/// 返回：包装后的共享宿主。
/// 错误：无；读取失败通过工具错误返回。
/// 状态：不打开writer或读取原件。
/// 并发与取消：读取同步有界；原工具取消语义不变。
/// 权限：原工具继续由inner验证，新工具只能读取reader允许的历史。
/// 关联：Host会话组装、CLI共享接线。
pub fn wrap_tools(
    inner: Arc<dyn ToolHost>,
    reader: OriginalReader,
    window: usize,
) -> Arc<dyn ToolHost> {
    Arc::new(ContextToolHost {
        inner,
        reader,
        page_chars: (window / 8).clamp(128, 4096),
        page_bytes: (window / 2).clamp(1024, 16 * 1024),
    })
}

fn range(args: &Value) -> std::result::Result<(&str, usize, usize), String> {
    let object = args.as_object().ok_or("参数必须是对象")?;
    if object
        .keys()
        .any(|k| !matches!(k.as_str(), "reference" | "start" | "limit"))
    {
        return Err("未知原件读取参数".into());
    }
    let reference = args["reference"]
        .as_str()
        .filter(|v| v.len() <= 4096)
        .ok_or("缺少有效reference")?;
    let number = |key: &str, default: u64| {
        args.get(key)
            .map_or(Ok(default), |v| v.as_u64().ok_or("范围必须为非负整数"))
    };
    let start = usize::try_from(number("start", 0)?).map_err(|_| "范围过大")?;
    let limit = number("limit", 4096)?;
    if limit == 0 || limit > 4096 {
        return Err("limit必须在1到4096之间".into());
    }
    Ok((reference, start, limit as usize))
}

#[async_trait]
impl ToolExecutor for ContextToolHost {
    async fn list_tools(&self) -> Result<Vec<ToolSpec>> {
        let mut tools = self.inner.list_tools().await?;
        let mut spec=ToolSpec::new("read_context_record","分页读取当前会话实际保存的工具结果或Goal观察原件，不重跑原工具；start和limit按Unicode字符计数。");
        spec.input_schema = json!({"type":"object","required":["reference"],"properties":{"reference":{"type":"string"},"start":{"type":"integer","minimum":0},"limit":{"type":"integer","minimum":1,"maximum":4096}},"additionalProperties":false});
        spec.category = Some(ToolCategory::ReadOnly);
        spec.concurrency = Some(ToolConcurrency::ParallelSafe);
        spec.max_result_chars = Some(0);
        tools.push(spec);
        Ok(tools)
    }
    async fn call(&self, name: &str, args: Value) -> Result<ToolCallOutcome> {
        if name != "read_context_record" {
            return self.inner.call(name, args).await;
        }
        let page = (|| {
            let (reference, start, limit) = range(&args)?;
            let (text, is_error) = (self.reader)(reference)?;
            let total = text.chars().count();
            if start > total {
                return Err("start超过原件范围".into());
            }
            let tail: String = text
                .chars()
                .skip(start)
                .take(limit.min(self.page_chars))
                .collect();
            let bounded = bounded_content(
                json!({"kind":"page","reference":reference,"start":start,"end":start,"total_chars":total,"total_bytes":text.len(),"is_error":is_error,"has_more":true,"next_start":start}),
                &tail,
                usize::MAX,
                self.page_bytes.saturating_sub(128),
            );
            let mut page: Value = serde_json::from_str(&bounded).map_err(|_| "原件分页编码失败")?;
            let end = start + page["content"].as_str().unwrap_or("").chars().count();
            if end == start && start < total {
                return Err("原件引用元数据无法放入本次读取额度".into());
            }
            page["end"] = json!(end);
            page["next_start"] = json!(end);
            page["has_more"] = json!(end < total);
            Ok::<_, String>(page)
        })();
        Ok(match page {
            Ok(value) => ToolCallOutcome::ok(value.to_string()),
            Err(error) => ToolCallOutcome::error(error),
        })
    }
    fn set_media_context(&self, media: Vec<openpaw_core::InlineMedia>) {
        self.inner.set_media_context(media);
    }
    fn remember_media(&self, media: Vec<openpaw_core::InlineMedia>) {
        self.inner.remember_media(media);
    }
}
#[async_trait]
impl ToolHostValidator for ContextToolHost {
    async fn validate_input(&self, name: &str, args: &Value) -> std::result::Result<(), String> {
        if name == "read_context_record" {
            range(args).map(|_| ())
        } else {
            self.inner.validate_input(name, args).await
        }
    }
    fn should_cascade_on_error(&self, name: &str, outcome: &ToolCallOutcome) -> bool {
        self.inner.should_cascade_on_error(name, outcome)
    }
}
