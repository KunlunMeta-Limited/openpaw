//! 职责：为当前授权会话提供不可变产物摘录及范围明确的核验。
//! 输入输出：准确版本和读回执到有界内容、确定性条件与模型意见。
//! 依赖：原ToolCallBinding、Store、版本blob，不另建文件服务。
//! 禁止职责：不写业务文件、不批准权限、不把模型判断当客观验收。
use crate::{HostError, HostResult};
use openpaw_core::ToolSpec;
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum Input {
    List {},
    Read {
        version: String,
        #[serde(default)]
        offset: usize,
        #[serde(default = "default_limit")]
        limit: usize,
    },
    Check {
        version: String,
        inspected_call_id: String,
        #[serde(default)]
        contains: Vec<String>,
        #[serde(default)]
        excludes: Vec<String>,
        min_chars: Option<usize>,
        max_chars: Option<usize>,
        assessment: Option<Assessment>,
    },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Assessment {
    criterion: String,
    verdict: String,
    evidence_quote: String,
    note: String,
}

fn default_limit() -> usize {
    4000
}
fn invalid(message: &str) -> HostError {
    HostError::new("invalid_delivery_check", message)
}
fn parse(value: &Value) -> HostResult<Input> {
    if serde_json::to_vec(value).map_err(HostError::storage)?.len() > 8192 {
        return Err(invalid("产物核验输入超过8KiB"));
    }
    let input: Input = serde_json::from_value(value.clone())
        .map_err(|_| invalid("产物核验字段不符合list/read/check约定"))?;
    match &input {
        Input::List {} => (),
        Input::Read { version, limit, .. } => {
            if version.is_empty() || version.len() > 128 || !(1..=6000).contains(limit) {
                return Err(invalid("read需要准确version和1—6000字符的limit"));
            }
        }
        Input::Check {
            version,
            inspected_call_id,
            contains,
            excludes,
            min_chars,
            max_chars,
            assessment,
        } => {
            if version.is_empty()
                || version.len() > 128
                || inspected_call_id.is_empty()
                || inspected_call_id.len() > 256
                || contains.len() + excludes.len() > 12
                || contains
                    .iter()
                    .chain(excludes)
                    .any(|s| s.trim().is_empty() || s.chars().count() > 200)
                || min_chars
                    .zip(*max_chars)
                    .is_some_and(|(min, max)| min > max)
                || contains.is_empty()
                    && excludes.is_empty()
                    && min_chars.is_none()
                    && max_chars.is_none()
                    && assessment.is_none()
            {
                return Err(invalid("check须有明确有限条件、准确版本和read回执；字符数使用Unicode标量，不等于中文字数"));
            }
            if let Some(a) = assessment {
                if !matches!(a.verdict.as_str(), "pass" | "fail" | "unknown")
                    || [&a.criterion, &a.evidence_quote, &a.note]
                        .iter()
                        .any(|s| s.trim().is_empty() || s.chars().count() > 300)
                {
                    return Err(invalid("模型意见须含具体criterion、pass/fail/unknown、原文evidence_quote和note，各不超300字符"));
                }
            }
        }
    }
    Ok(input)
}

/// 功能：提供通用不可变产物读取和核验工具声明。
/// 参数：无。
/// 返回：原Host工具规范，不包含业务流程。
/// 错误：无。
/// 状态：无。
/// 并发与取消：纯构造，执行复用原工具取消链。
/// 权限：仅Office及可信子Run启用，不授予业务文件权限。
/// 关联：CHG-2026-0007第12.160节D170；REQ-OP-014/022。
pub(crate) fn spec() -> ToolSpec {
    serde_json::from_value(json!({"name":"inspect_delivery", "description":"读取真实已保存产物的不可变版本并记录有限核验。先list取得version，再read；check引用同Run准确read_call_id。contains/excludes及min_chars/max_chars由Host针对全文计算（Unicode标量数，含标点空白），assessment仅是模型对已读摘录的意见，不代表全面验收。只核对用户实际要求，不添加自定阈值；改写新版本须重新核验。", "input_schema":{
        "type":"object","properties":{
            "action":{"type":"string","enum":["list","read","check"]},
            "version":{"type":"string"},"offset":{"type":"integer","minimum":0},"limit":{"type":"integer","minimum":1,"maximum":6000},
            "inspected_call_id":{"type":"string"},"contains":{"type":"array","items":{"type":"string"},"maxItems":12},"excludes":{"type":"array","items":{"type":"string"},"maxItems":12},
            "min_chars":{"type":"integer","minimum":0},"max_chars":{"type":"integer","minimum":0},
            "assessment":{"type":"object","properties":{"criterion":{"type":"string"},"verdict":{"type":"string","enum":["pass","fail","unknown"]},"evidence_quote":{"type":"string"},"note":{"type":"string"}},"required":["criterion","verdict","evidence_quote","note"],"additionalProperties":false}
        },"required":["action"],"additionalProperties":false
    }})).expect("固定核验工具声明合法")
}

/// 功能：校验产物工具输入的有界结构。
/// 参数：value为最终工具输入。
/// 返回：合法返回空。
/// 错误：超限、未知字段或空核验条件拒绝。
/// 状态：无。
/// 并发与取消：纯函数。
/// 权限：不检查也不扩展文件权限，执行时再核scope。
/// 关联：CHG-2026-0007第12.160节D170。
pub(crate) fn validate(value: &Value) -> HostResult<()> {
    parse(value).map(|_| ())
}

/// 功能：在经授权的不可变文本上计算摘录或有限核验结果。
/// 参数：value为最终输入，text为原件，receipt为本Run同版本read事实。
/// 返回：有界结果；read包含实际覆盖范围，check不声明全局通过。
/// 错误：缺回执、越界、引用未读原文或不支持操作拒绝。
/// 状态：不写状态，由原Store持久保存返回的检查事实。
/// 并发与取消：纯计算，文本受原8MiB版本上限约束。
/// 权限：调用方先验证scope、version与SHA256，receipt不能由模型自填。
/// 关联：CHG-2026-0007第12.160节。
pub(crate) fn evaluate(value: &Value, text: &str, receipt: Option<&Value>) -> HostResult<Value> {
    let chars = text.chars().count();
    match parse(value)? {
        Input::Read { offset, limit, .. } => {
            if offset > chars {
                return Err(invalid("摘录offset超出真实字符数"));
            }
            let excerpt: String = text.chars().skip(offset).take(limit).collect();
            let end = offset + excerpt.chars().count();
            Ok(
                json!({"offset":offset,"end":end,"total_chars":chars,"full_text":offset==0 && end==chars,"text":excerpt}),
            )
        }
        Input::Check {
            contains,
            excludes,
            min_chars,
            max_chars,
            assessment,
            ..
        } => {
            let read = receipt.ok_or_else(|| invalid("须引用本Run同版本的实际read_call_id"))?;
            let start = read["offset"]
                .as_u64()
                .ok_or_else(|| invalid("读取回执损坏"))? as usize;
            let end = read["end"]
                .as_u64()
                .ok_or_else(|| invalid("读取回执损坏"))? as usize;
            let excerpt: String = text
                .chars()
                .skip(start)
                .take(end.saturating_sub(start))
                .collect();
            let mut checks = Vec::new();
            for needle in contains {
                checks.push(
                    json!({"kind":"contains","expected":needle,"passed":text.contains(&needle)}),
                );
            }
            for needle in excludes {
                checks.push(
                    json!({"kind":"excludes","expected":needle,"passed":!text.contains(&needle)}),
                );
            }
            if let Some(min) = min_chars {
                checks.push(
                    json!({"kind":"min_chars","expected":min,"actual":chars,"passed":chars>=min}),
                );
            }
            if let Some(max) = max_chars {
                checks.push(
                    json!({"kind":"max_chars","expected":max,"actual":chars,"passed":chars<=max}),
                );
            }
            let assessment = if let Some(a) = assessment {
                if !excerpt.contains(&a.evidence_quote) {
                    return Err(invalid("模型意见证据不在实际读取的摘录中"));
                }
                json!({"source":"model_judgement","criterion":a.criterion,"verdict":a.verdict,"evidence_quote":a.evidence_quote,"note":a.note,"read_offset":start,"read_end":end,"full_text_read":read["full_text"]})
            } else {
                Value::Null
            };
            let failed = checks.iter().filter(|c| c["passed"] == false).count();
            Ok(
                json!({"deterministic_checks":checks,"deterministic_failed":failed,"assessment":assessment,"acceptance":"scoped_checks_only","note":"仅覆盖列出的条件；模型意见不是客观证明，未检查项仍未知。字符数含标点和空白，不等于中文字数。"}),
            )
        }
        Input::List {} => Err(invalid("list由原Store读取授权目录")),
    }
}

/// 功能：将准确版本核验事实缩成原Office可显示的文字。
/// 参数：record为该version/hash最后一次有限检查，可缺失。
/// 返回：范围明确的短句，不伪报整体验收通过。
/// 错误：无，旧数据默认未核验。
/// 状态：无。
/// 并发与取消：纯函数。
/// 权限：调用方已按会话过滤记录。
/// 关联：CHG-2026-0007第12.160节D170。
pub(crate) fn summary(record: Option<&Value>) -> String {
    let Some(r) = record else {
        return "已保存；本版本未登记核验".into();
    };
    let count = r["deterministic_checks"].as_array().map_or(0, Vec::len);
    let failed = r["deterministic_failed"].as_u64().unwrap_or(0);
    let mut s = if count > 0 {
        format!("最近自动条件{count}项，{failed}项未通过")
    } else {
        "未登记自动条件".into()
    };
    if let Some(v) = r["assessment"]["verdict"].as_str() {
        s.push_str(&format!(
            "；模型局部意见{}",
            match v {
                "pass" => "通过",
                "fail" => "不通过",
                _ => "未知",
            }
        ));
    }
    s.push_str("；其余未核验");
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounded_unicode_read_and_explicit_checks_do_not_claim_acceptance() {
        let text = "标题\n甲乙丙；金额100元。";
        let read = evaluate(
            &json!({"action":"read","version":"v","limit":6}),
            text,
            None,
        )
        .unwrap();
        assert_eq!(read["text"], "标题\n甲乙丙");
        assert_eq!(read["full_text"], false);
        let check = json!({"action":"check","version":"v","inspected_call_id":"r","contains":["金额200元"],"excludes":["秘密"],"max_chars":100,"assessment":{"criterion":"标题相关","verdict":"pass","evidence_quote":"标题","note":"仅此摘录"}});
        let result = evaluate(&check, text, Some(&read)).unwrap();
        assert_eq!(result["deterministic_failed"], 1);
        assert_eq!(result["assessment"]["full_text_read"], false);
        assert_eq!(result["acceptance"], "scoped_checks_only");
        assert!(evaluate(&check, text, None).is_err());
        let mut forged = check.clone();
        forged["assessment"]["evidence_quote"] = json!("金额100元");
        assert!(evaluate(&forged, text, Some(&read)).is_err());
        assert!(summary(None).contains("未登记核验"));
    }
    #[test]
    fn malformed_or_unbounded_check_is_rejected() {
        for input in [
            json!({"action":"read","version":"v","limit":6001}),
            json!({"action":"check","version":"v","inspected_call_id":"x"}),
            json!({"action":"check","version":"v","inspected_call_id":"x","contains":[""]}),
            json!({"action":"check","version":"v","inspected_call_id":"x","min_chars":3,"max_chars":2}),
            json!({"action":"list","scope":"other"}),
        ] {
            assert!(validate(&input).is_err(), "{input}");
        }
    }
}
