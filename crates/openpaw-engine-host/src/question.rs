//! 职责：将显式许可的clarify桥接至Office原选择菜单与真实用户回答。
//! 输入输出：最终工具绑定到同Run的原工具结果；控制元数据留原host_events。
//! 依赖：原Store单写CAS、Office连接池/恢复通知及Run取消。
//! 禁止职责：不执行文件操作、不替代审批、不重放问题或模型请求。
use crate::{ClientScope, Host, HostError, HostResult};
use openpaw_core::{ToolCallBinding, ToolCallOutcome};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    question: String,
    options: Option<Vec<String>>,
    context: Option<String>,
}
fn input(value: &Value) -> HostResult<Input> {
    let input: Input = serde_json::from_value(value.clone())
        .map_err(|_| HostError::new("invalid_input", "clarify仅接受question/options/context"))?;
    let text = |s: &str, limit| !s.trim().is_empty() && s.encode_utf16().count() <= limit;
    if !text(&input.question, 120)
        || input
            .context
            .as_ref()
            .is_some_and(|s| s.encode_utf16().count() > 1800)
        || input.options.as_ref().is_some_and(|v| {
            !(2..=8).contains(&v.len())
                || v.iter().any(|s| !text(s, 120))
                || v.iter()
                    .map(|s| s.trim())
                    .collect::<std::collections::BTreeSet<_>>()
                    .len()
                    != v.len()
        })
    {
        return Err(HostError::new(
            "invalid_input",
            "问题限120单元，上下文限1800单元，选项须为2—8个互异非空短文本",
        ));
    }
    // null不是缺省，避免模型误以为已提供合法选项。
    if value.get("options").is_some_and(Value::is_null)
        || value.get("context").is_some_and(Value::is_null)
    {
        return Err(HostError::new(
            "invalid_input",
            "可选字段缺省时应省略，不能为null",
        ));
    }
    Ok(input)
}
/// 功能：按Host问答契约校验最终输入，不产生提问。
/// 参数：value为原工具最终输入。
/// 返回：合法时成功。
/// 错误：未知字段、空问题、超长或非法选项。
/// 状态：无。
/// 并发与取消：纯校验，无等待。
/// 权限：不接受模型指定作用域、期限或授权。
/// 关联：REQ-OP-013；DES-OP-008；CHG-2026-0007第6.1节。
pub(crate) fn validate(value: &Value) -> HostResult<()> {
    input(value).map(|_| ())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// 用途：原host_events中的版本化问题控制事实。
/// 字段：原Run/call/输入摘要、可信展示绑定、固定期限、状态及回答摘要。
/// 来源去向：Store单写序列到同Run等待器与恢复处理。
/// 约束：不保存完整答案；不是工具授权或第二份任务事实。
/// 稳定性：Host私有v1元数据。
/// 关联：REQ-OP-013/016；DES-OP-008/009；CHG-2026-0007第7.1节。
pub(crate) struct State {
    /// 用途：version，由可信Host生命周期提供。
    /// 字段：内部问题控制字段，具体类型如下。
    /// 来源去向：原Store或Office协调器与当前工具调用。
    /// 约束：模型不得覆盖，不构成执行授权。
    /// 稳定性：Host私有r37实现。
    /// 关联：REQ-OP-013；DES-OP-008；CHG-2026-0007第9.16节。
    pub(crate) version: u8,
    /// 用途：question_id，由可信Host生命周期提供。
    /// 字段：内部问题控制字段，具体类型如下。
    /// 来源去向：原Store或Office协调器与当前工具调用。
    /// 约束：模型不得覆盖，不构成执行授权。
    /// 稳定性：Host私有r37实现。
    /// 关联：REQ-OP-013；DES-OP-008；CHG-2026-0007第9.16节。
    pub(crate) question_id: String,
    /// 用途：run_id，由可信Host生命周期提供。
    /// 字段：内部问题控制字段，具体类型如下。
    /// 来源去向：原Store或Office协调器与当前工具调用。
    /// 约束：模型不得覆盖，不构成执行授权。
    /// 稳定性：Host私有r37实现。
    /// 关联：REQ-OP-013；DES-OP-008；CHG-2026-0007第9.16节。
    pub(crate) run_id: String,
    /// 用途：call_id，由可信Host生命周期提供。
    /// 字段：内部问题控制字段，具体类型如下。
    /// 来源去向：原Store或Office协调器与当前工具调用。
    /// 约束：模型不得覆盖，不构成执行授权。
    /// 稳定性：Host私有r37实现。
    /// 关联：REQ-OP-013；DES-OP-008；CHG-2026-0007第9.16节。
    pub(crate) call_id: String,
    /// 用途：input_digest，由可信Host生命周期提供。
    /// 字段：内部问题控制字段，具体类型如下。
    /// 来源去向：原Store或Office协调器与当前工具调用。
    /// 约束：模型不得覆盖，不构成执行授权。
    /// 稳定性：Host私有r37实现。
    /// 关联：REQ-OP-013；DES-OP-008；CHG-2026-0007第9.16节。
    pub(crate) input_digest: String,
    /// 用途：scope，由可信Host生命周期提供。
    /// 字段：内部问题控制字段，具体类型如下。
    /// 来源去向：原Store或Office协调器与当前工具调用。
    /// 约束：模型不得覆盖，不构成执行授权。
    /// 稳定性：Host私有r37实现。
    /// 关联：REQ-OP-013；DES-OP-008；CHG-2026-0007第9.16节。
    pub(crate) scope: ClientScope,
    /// 用途：role_id，由可信Host生命周期提供。
    /// 字段：内部问题控制字段，具体类型如下。
    /// 来源去向：原Store或Office协调器与当前工具调用。
    /// 约束：模型不得覆盖，不构成执行授权。
    /// 稳定性：Host私有r37实现。
    /// 关联：REQ-OP-013；DES-OP-008；CHG-2026-0007第9.16节。
    pub(crate) role_id: String,
    /// 用途：created_at_ms，由可信Host生命周期提供。
    /// 字段：内部问题控制字段，具体类型如下。
    /// 来源去向：原Store或Office协调器与当前工具调用。
    /// 约束：模型不得覆盖，不构成执行授权。
    /// 稳定性：Host私有r37实现。
    /// 关联：REQ-OP-013；DES-OP-008；CHG-2026-0007第9.16节。
    pub(crate) created_at_ms: i64,
    /// 用途：expires_at_ms，由可信Host生命周期提供。
    /// 字段：内部问题控制字段，具体类型如下。
    /// 来源去向：原Store或Office协调器与当前工具调用。
    /// 约束：模型不得覆盖，不构成执行授权。
    /// 稳定性：Host私有r37实现。
    /// 关联：REQ-OP-013；DES-OP-008；CHG-2026-0007第9.16节。
    pub(crate) expires_at_ms: i64,
    /// 用途：state，由可信Host生命周期提供。
    /// 字段：内部问题控制字段，具体类型如下。
    /// 来源去向：原Store或Office协调器与当前工具调用。
    /// 约束：模型不得覆盖，不构成执行授权。
    /// 稳定性：Host私有r37实现。
    /// 关联：REQ-OP-013；DES-OP-008；CHG-2026-0007第9.16节。
    pub(crate) state: String,
    /// 用途：display_id，由可信Host生命周期提供。
    /// 字段：内部问题控制字段，具体类型如下。
    /// 来源去向：原Store或Office协调器与当前工具调用。
    /// 约束：模型不得覆盖，不构成执行授权。
    /// 稳定性：Host私有r37实现。
    /// 关联：REQ-OP-013；DES-OP-008；CHG-2026-0007第9.16节。
    pub(crate) display_id: Option<String>,
    /// 用途：answer_digest，由可信Host生命周期提供。
    /// 字段：内部问题控制字段，具体类型如下。
    /// 来源去向：原Store或Office协调器与当前工具调用。
    /// 约束：模型不得覆盖，不构成执行授权。
    /// 稳定性：Host私有r37实现。
    /// 关联：REQ-OP-013；DES-OP-008；CHG-2026-0007第9.16节。
    pub(crate) answer_digest: Option<String>,
}
/// 用途：复用Office协调器的原问答传输资源。
/// 字段：连接池、恢复广播、已监听地址及同源关闭token。
/// 来源去向：运行中的协调器到当前clarify调用。
/// 约束：不能更换请求目的地址、凭据或延长问题期限。
/// 稳定性：Host私有句柄。
/// 关联：REQ-OP-013；DES-OP-008；CHG-2026-0007第9.16节。
pub(crate) struct Transport {
    /// 用途：client，由可信Host生命周期提供。
    /// 字段：内部问题控制字段，具体类型如下。
    /// 来源去向：原Store或Office协调器与当前工具调用。
    /// 约束：模型不得覆盖，不构成执行授权。
    /// 稳定性：Host私有r37实现。
    /// 关联：REQ-OP-013；DES-OP-008；CHG-2026-0007第9.16节。
    pub(crate) client: Arc<crate::office::client::Client>,
    /// 用途：refresh，由可信Host生命周期提供。
    /// 字段：内部问题控制字段，具体类型如下。
    /// 来源去向：原Store或Office协调器与当前工具调用。
    /// 约束：模型不得覆盖，不构成执行授权。
    /// 稳定性：Host私有r37实现。
    /// 关联：REQ-OP-013；DES-OP-008；CHG-2026-0007第9.16节。
    pub(crate) refresh: tokio::sync::broadcast::Sender<ClientScope>,
    /// 用途：address，由可信Host生命周期提供。
    /// 字段：内部问题控制字段，具体类型如下。
    /// 来源去向：原Store或Office协调器与当前工具调用。
    /// 约束：模型不得覆盖，不构成执行授权。
    /// 稳定性：Host私有r37实现。
    /// 关联：REQ-OP-013；DES-OP-008；CHG-2026-0007第9.16节。
    pub(crate) address: std::net::SocketAddr,
    /// 用途：stop，由可信Host生命周期提供。
    /// 字段：内部问题控制字段，具体类型如下。
    /// 来源去向：原Store或Office协调器与当前工具调用。
    /// 约束：模型不得覆盖，不构成执行授权。
    /// 稳定性：Host私有r37实现。
    /// 关联：REQ-OP-013；DES-OP-008；CHG-2026-0007第9.16节。
    pub(crate) stop: CancellationToken,
}
/// 功能：为控制事实生成不可逆SHA256摘要，正文仍由原账本保存。
/// 参数：value为需要绑定的最终输入或真实答案。
/// 返回：十六进制摘要。
/// 错误：JSON序列化错误。
/// 状态：不写入。
/// 并发与取消：纯计算。
/// 权限：摘要不是授权凭据。
/// 关联：REQ-OP-013；DES-OP-008；CHG-2026-0007第7.1节。
pub(crate) fn digest(value: &Value) -> HostResult<String> {
    let bytes = serde_json::to_vec(value).map_err(HostError::storage)?;
    Ok(ring::digest::digest(&ring::digest::SHA256, &bytes)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}
/// 功能：在原资源锁及写审批之前等待真实用户答案并返回原工具结果。
/// 参数：host/run_deadline为可信装配，binding为最终工具绑定，cancel为调用取消。
/// 返回：answered/declined/expired或明确展示错误，原AgentLoop继续同一Run。
/// 错误：取消、绑定/期限冲突或存储失败；不伪造答案。
/// 状态：问题控制事实经单写CAS终结，完整答案只进入原JSONL工具结果。
/// 并发与取消：不持根锁、事务或模型流；取消先drop请求并等待Store清理。
/// 权限：显式clarify许可和真实父子Office绑定，不授予任何执行权限。
/// 关联：REQ-OP-013/016；DES-OP-008/009；CHG-2026-0007第9.16节。
pub(crate) async fn ask(
    host: &Host,
    run_deadline: Option<tokio::time::Instant>,
    binding: ToolCallBinding,
    cancel: CancellationToken,
) -> HostResult<ToolCallOutcome> {
    let input = input(&binding.input)?;
    if cancel.is_cancelled() {
        return Err(HostError::new("cancelled", "提问已取消"));
    }
    let transport = host.question_transport().await?;
    let duration = run_deadline
        .map(|d| {
            d.saturating_duration_since(tokio::time::Instant::now())
                .min(Duration::from_secs(300))
        })
        .unwrap_or(Duration::from_secs(300));
    let deadline = tokio::time::Instant::now() + duration;
    let expires = chrono::Utc::now().timestamp_millis() + duration.as_millis() as i64;
    let b = binding.clone();
    let c = cancel.clone();
    let state = host
        .inner
        .store
        .call_priority(move |db| db.question_start(&b, expires, &c))
        .await?;
    let mut refresh = transport.refresh.subscribe();
    let mut options = input
        .options
        .clone()
        .unwrap_or_else(|| vec!["按明确标注的假设继续".into(), "暂不补充".into()]);
    for option in &mut options {
        *option = option.trim().into();
    }
    let decision_options: Vec<_> = options
        .iter()
        .enumerate()
        .map(|(i, s)| json!({"id":format!("option-{i}"),"label":s}))
        .collect();
    let role = crate::agents::role(&state.role_id);
    let mut decision = json!({"id":"pending","actorId":role.map(|r|r.actor).unwrap_or("office-lead"),
        "kind":"choice","title":input.question.trim(),"description":input.context.unwrap_or_default(),
        "allowCustom":true,"options":decision_options});
    let mut display = None;
    let mut retries = 0;
    let mut openings = 0;
    let response: HostResult<Value> = loop {
        let result = async {
            if cancel.is_cancelled() || transport.stop.is_cancelled() {
                return Err(HostError::new("cancelled", "提问已取消"));
            }
            if openings >= 32 {
                return Err(HostError::new(
                    "office_unavailable",
                    "问题展示恢复次数过多；请停止本轮后重新发起",
                ));
            }
            let scope = state.scope.clone();
            let config = host
                .inner
                .config
                .office
                .as_ref()
                .ok_or_else(|| HostError::new("office_unavailable", "Office未启用"))?;
            // 原同步屏障确认父对话存在，HTTP等待期间不占用根资源锁。
            crate::office::prepare_question(host, config, &scope, &transport).await?;
            let ms = (state.expires_at_ms - chrono::Utc::now().timestamp_millis()).min(300000);
            if ms < 1000 {
                return Err(HostError::new("decision_expired", "未在期限内收到用户回答"));
            }
            let next_display = uuid::Uuid::new_v4().simple().to_string();
            let (id, d, c) = (
                state.question_id.clone(),
                next_display.clone(),
                cancel.clone(),
            );
            host.inner
                .store
                .call_priority(move |db| db.question_display(&id, &d, &c))
                .await?;
            display = Some(next_display.clone());
            openings += 1;
            decision["id"] = json!(next_display);
            let request = json!({"type":"decision.request","teamId":scope.slot_id,"conversationId":scope.conversation_id,
                "timeoutMs":ms,"decision":decision});
            let response = transport.client.post(request);
            tokio::pin!(response);
            loop {
                tokio::select! {biased;
                    _=cancel.cancelled()=>return Err(HostError::new("cancelled","提问已取消")),
                    _=transport.stop.cancelled()=>return Err(HostError::new("cancelled","Office正在关闭")),
                    _=tokio::time::sleep_until(deadline)=>return Err(HostError::new("decision_expired","未在期限内收到用户回答")),
                    changed=refresh.recv()=>match changed {
                        Ok(s) if s != scope => continue,
                        Err(tokio::sync::broadcast::error::RecvError::Closed)=>return Err(HostError::new("office_unavailable","Office恢复通知已关闭")),
                        _=>return Err(HostError::new("question_refresh","原会话恢复展示")),
                    },
                    result=&mut response=>return result.map(|(_,v)|v),
                }
            }
        };
        // 包含同步和连接准备的所有阶段都受同一个原问题期限约束。
        let result = tokio::select! {biased;
            _=cancel.cancelled()=>Err(HostError::new("cancelled","提问已取消")),
            _=transport.stop.cancelled()=>Err(HostError::new("cancelled","Office正在关闭")),
            _=tokio::time::sleep_until(deadline)=>Err(HostError::new("decision_expired","未在期限内收到用户回答")),
            result=result=>result,
        }; // future在重开前释放，Office据HTTP断开撤销旧display。
        match result {
            Err(ref e) if e.kind == "question_refresh" => continue,
            Err(ref e) if e.kind == "office_unavailable" && retries < 2 && openings < 32 => {
                retries += 1;
                tokio::select! {biased;
                    _=cancel.cancelled()=>break Err(HostError::new("cancelled","提问已取消")),
                    _=transport.stop.cancelled()=>break Err(HostError::new("cancelled","Office正在关闭")),
                    _=tokio::time::sleep_until(deadline)=>break Err(HostError::new("decision_expired","未在期限内收到用户回答")),
                    _=tokio::time::sleep(Duration::from_millis(250 * retries))=>{}
                }
            }
            value => break value,
        }
    };
    let outcome = match &response {
        Ok(answer) => {
            let declined = input.options.is_none() && answer["choiceId"] == "option-1";
            json!({"question_id":state.question_id,"status":if declined {"declined"} else {"answered"},
                "choice_id":answer["choiceId"],"answer":answer.get("customText").unwrap_or(&answer["label"]),
                "note":"仅为用户补充信息，不是执行授权；不要重复询问同题。"})
        }
        Err(e) => {
            json!({"question_id":state.question_id,"status":if e.kind=="cancelled" {"cancelled"} else if e.kind=="decision_expired" {"expired"} else {"display_failed"},
            "error":{"kind":e.kind,"message":e.message},"note":"未收到可靠答案，不自动选择，不重复提问；已有成果保留。"})
        }
    };
    let (id, d, c, final_state, hash) = (
        state.question_id,
        display,
        cancel.clone(),
        outcome["status"].as_str().unwrap().to_owned(),
        response.as_ref().ok().map(digest).transpose()?,
    );
    let settled = host
        .inner
        .store
        .call_priority(move |db| db.question_finish(&id, d.as_deref(), &final_state, hash, &c))
        .await?;
    if settled == "cancelled" {
        return Err(HostError::new("cancelled", "提问已取消，晚到回答未接受"));
    }
    if settled != outcome["status"].as_str().unwrap() {
        return Ok(ToolCallOutcome::error(json!({"question_id":outcome["question_id"],"status":settled,"note":"未收到期限内有效答案，不能采用晚到回答，不重复提问。"}).to_string()));
    }
    let mut result = ToolCallOutcome::ok(outcome.to_string());
    result.is_error = response.is_err();
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn input_rejects_scope_forgery_duplicates_and_limits() {
        for value in [
            json!({}),
            json!({"question":" "}),
            json!({"question":"x","teamId":"other"}),
            json!({"question":"x","options":["one"]}),
            json!({"question":"x","options":["a"," a"]}),
            json!({"question":"x","options":null}),
            json!({"question":"😀".repeat(61)}),
            json!({"question":"x","context":"a".repeat(1801)}),
        ] {
            assert!(validate(&value).is_err(), "{value}");
        }
        for value in [
            json!({"question":"x"}),
            json!({"question":"x","options":["a","b"]}),
            json!({"question":"😀".repeat(60)}),
        ] {
            validate(&value).unwrap();
        }
    }
}
