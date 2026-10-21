//! 功能：从真实会话构建员工四视图，只读分页与手动刷新。
//! 输入输出边界：已验证的 Office UI action 进入，detail-v1 文本返回。
//! 禁止承担：不执行模型、工具、审批或持久化，不把展示游标当作员工身份。
//! 关联：CHG-2026-0036、subagent-design A4。

use super::{MainOffice, Value};
use crate::runtime::SessionHandle;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

// 四段即使全部是需要六字节 JSON 转义的控制字符，也低于既有64KiB消息上限。
const PAGE_FIELDS: usize = 4;
const MAX_SNAPSHOTS: usize = 64;
const TTL: Duration = Duration::from_secs(60);
const VIEWS: [&str; 4] = ["任务", "消息", "工具", "状态"];

#[derive(Default)]
/// 功能：有界保存只读详情能力和不可变快照。
/// 字段语义：entries 按 UI 请求编号关联办公室、员工、视图与随机按钮。
/// 数据来源：MainOffice 已验证身份及 runtime 公开消息投影。
/// 数据去向：纯文本 detail-v1，不写会话或产物。
/// 约束：最多64项、闲置60秒，旧按钮不能跨关联使用。
/// 稳定性：Host 内部内存状态，重启失效。
/// 关联：CHG-2026-0036。
pub(super) struct Details {
    entries: HashMap<String, Cursor>,
}

struct Snapshot {
    team: String,
    employee: String,
    session: String,
    name: String,
    captured: String,
    views: [Vec<Value>; 4],
}

struct Cursor {
    snapshot: Arc<Snapshot>,
    view: usize,
    expires: tokio::time::Instant,
    options: HashMap<String, Operation>,
}

#[derive(Clone, Copy)]
enum Operation {
    View(usize),
    Page(usize),
    Refresh,
}

fn unavailable(description: &str) -> Value {
    serde_json::json!({"template":"detail-v1","title":"员工详情","description":description,"fields":[],"actions":[]})
}

fn fields(label: &str, text: String, output: &mut Vec<Value>) {
    // 按浏览器 UTF-16 长度拆分；片段顺序和原文完全保留，不截断内容。
    let mut current = String::new();
    let mut length = 0;
    let mut number = 1;
    for character in text.chars() {
        if length + character.len_utf16() > 2000 {
            output.push(serde_json::json!({"label":format!("{label} · {number}"),"value":current}));
            current = String::new();
            length = 0;
            number += 1;
        }
        current.push(character);
        length += character.len_utf16();
    }
    output.push(serde_json::json!({"label":format!("{label} · {number}"),"value":current}));
}

impl MainOffice {
    /// 功能：回收已过期的只读详情快照。
    /// 参数：无。
    /// 返回：无。
    /// 错误：锁中毒时恢复原有缓存并继续回收。
    /// 状态：仅删除过期内存关联，不影响历史或执行。
    /// 并发与取消：既有 UI 轮询周期调用，持短同步锁；关闭 Host 即释放全部。
    /// 权限：不读取其他办公室，不发出任何业务请求。
    /// 关联：CHG-2026-0036。
    pub(super) fn expire_details(&self) {
        let now = tokio::time::Instant::now();
        self.details
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .entries
            .retain(|_, entry| entry.expires > now);
    }

    fn detail_session(&self, team: &str, employee: &str) -> Option<Arc<SessionHandle>> {
        let record = self.record(team).ok()?;
        if !record.snapshot["employees"]
            .as_array()?
            .iter()
            .any(|item| item["id"] == employee)
        {
            return None;
        }
        self.sessions
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .values()
            .filter_map(std::sync::Weak::upgrade)
            .find(|handle| {
                handle.workspace == record.workspace
                    && (handle.id == employee
                        || handle.child.as_ref().is_some_and(|child| {
                            Some(child.parent_id.as_str()) == record.session.as_deref()
                                && child.workspace_id == employee
                        }))
            })
    }

    fn detail_snapshot(&self, team: &str, employee: &str) -> Result<Arc<Snapshot>, Value> {
        let record = self
            .record(team)
            .map_err(|_| unavailable("办公室不存在，未启动任何工作。"))?;
        let handle = self
            .detail_session(team, employee)
            .ok_or_else(|| unavailable("该员工尚无已加载的持久会话记录；未生成或重放工作。"))?;
        let name = record.snapshot["employees"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|item| item["id"] == employee)
            .and_then(|item| item["name"].as_str())
            .unwrap_or("Agent")
            .to_string();
        let history = handle.wire_messages();
        let outcomes = handle.turn_outcomes();
        let mut views: [Vec<Value>; 4] = Default::default();
        let mut task_message = "尚无关联任务";
        for message in &history {
            let id = message["info"]["id"].as_str().unwrap_or("unknown");
            let role = message["info"]["role"].as_str().unwrap_or("unknown");
            let mut texts = Vec::new();
            for part in message["parts"].as_array().into_iter().flatten() {
                if part["type"] == "tool" {
                    fields("工具记录", format!("messageID={id}\ncallID={}\n工具={}\n状态={}\n输入：\n{}\n结果：\n{}\n错误：\n{}\n执行结论={}\n时间={}",
                        part["callID"].as_str().unwrap_or("unknown"), part["tool"].as_str().unwrap_or("unknown"),
                        part["state"]["status"].as_str().unwrap_or("unknown"), part["state"]["input"],
                        part["state"]["output"].as_str().unwrap_or(""),part["state"]["error"].as_str().unwrap_or(""),
                        part["state"]["disposition"],message["info"]["time"]), &mut views[2]);
                } else if let Some(text) = part["text"].as_str() {
                    texts.push(text.to_string());
                } else {
                    texts.push(serde_json::to_string_pretty(part).unwrap_or_default());
                }
            }
            if !texts.is_empty() {
                let text = texts.join("\n");
                fields(
                    "消息记录",
                    format!(
                        "messageID={id}\n角色={role}\n时间={}\n执行={}\n{text}",
                        message["info"]["time"], message["info"]["execution"]
                    ),
                    &mut views[1],
                );
                if role == "user" {
                    task_message = id;
                    let outcome = outcomes.iter().find(|turn| turn.user_message_id == id);
                    fields(
                        "任务记录",
                        format!(
                            "messageID={id}\n回合={}\n任务：\n{text}",
                            serde_json::to_string(&outcome).unwrap_or_default()
                        ),
                        &mut views[0],
                    );
                } else if role == "assistant" {
                    fields("任务输出",format!("任务messageID={task_message}\n输出messageID={id}\n执行={}\n输出：\n{text}",message["info"]["execution"]),&mut views[0]);
                }
            }
        }
        let latest = outcomes.last();
        fields(
            "状态 / 更新时间",
            format!(
                "状态={}\n更新时间={}\n当前活动={}\n最近回合={}",
                if handle.is_active() {
                    "运行中"
                } else {
                    "空闲（不会自动继续）"
                },
                handle.updated_at.load(std::sync::atomic::Ordering::Acquire),
                serde_json::to_string(&handle.activity_snapshot()).unwrap_or_default(),
                serde_json::to_string(&latest).unwrap_or_default()
            ),
            &mut views[3],
        );
        for view in &mut views {
            if view.is_empty() {
                fields("记录", "暂无已保存记录。".into(), view);
            }
        }
        Ok(Arc::new(Snapshot {
            team: team.into(),
            employee: employee.into(),
            session: handle.id.clone(),
            name,
            captured: chrono::Utc::now().to_rfc3339(),
            views,
        }))
    }

    /// 功能：处理员工查看及纯只读详情按钮。
    /// 参数：team 为办公室编号，action 为当前 ui.next 请求。
    /// 返回：符合现有容量限制的 detail-v1 内容。
    /// 错误：身份、过期或按钮无效时返回明确说明。
    /// 状态：只更新有界内存游标，不改变会话或执行状态。
    /// 并发与取消：短同步锁，不等待模型、工具或 I/O。
    /// 权限：绑定服务端办公室、员工和随机按钮，不能选择任意会话。
    /// 关联：CHG-2026-0036、subagent-design A4。
    pub(super) fn inspect(&self, team: &str, action: &Value) -> Value {
        let Some(id) = action["id"].as_str() else {
            return unavailable("无效请求编号。");
        };
        let mut details = self
            .details
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let now = tokio::time::Instant::now();
        details.entries.retain(|_, entry| entry.expires > now);
        let (snapshot, view, page) = if action["action"] == "dialog.action" {
            let previous = action["interactionId"].as_str().unwrap_or("");
            let option = action["optionId"].as_str().unwrap_or("");
            let Some(cursor) = details.entries.get(previous) else {
                return unavailable("详情已关闭、过期或被替换，请重新点击员工；未执行任何工作。");
            };
            if cursor.snapshot.team != team {
                return unavailable("分页办公室不匹配，拒绝访问其他办公室的员工记录。");
            }
            let Some(operation) = cursor.options.get(option).copied() else {
                return unavailable("无效或跨员工的按钮，未读取其他员工记录。");
            };
            if self
                .detail_session(team, &cursor.snapshot.employee)
                .is_none_or(|handle| handle.id != cursor.snapshot.session)
            {
                return unavailable("员工会话绑定已失效，请重新点击员工。");
            }
            let cursor = details.entries.remove(previous).expect("validated cursor");
            match operation {
                Operation::View(view) => (cursor.snapshot, view, 0),
                Operation::Page(page) => (cursor.snapshot, cursor.view, page),
                Operation::Refresh => match self.detail_snapshot(team, &cursor.snapshot.employee) {
                    Ok(snapshot) => (snapshot, cursor.view, 0),
                    Err(error) => return error,
                },
            }
        } else {
            let Ok(record) = self.record(team) else {
                return unavailable("办公室不存在，未启动任何工作。");
            };
            let employee = record.snapshot["employees"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|employee| {
                    if action["action"] == "actor.inspect" {
                        action["actorId"].as_str().is_some_and(|actor| {
                            !actor.is_empty() && employee["sceneActorId"] == actor
                        })
                    } else {
                        employee["id"] == action["employeeId"]
                    }
                });
            let Some(employee) = employee.and_then(|employee| employee["id"].as_str()) else {
                return unavailable("该人物尚未绑定实际 Agent，没有可查看的工作记录。");
            };
            match self.detail_snapshot(team, employee) {
                Ok(snapshot) => (snapshot, 0, 0),
                Err(error) => return error,
            }
        };
        // 新关联可能来自重放 ID；只能替换同办公室的关联，不能污染其他办公室。
        if details
            .entries
            .get(id)
            .is_some_and(|entry| entry.snapshot.team != team)
        {
            return unavailable("请求编号已属于其他办公室，拒绝覆盖。");
        }
        while details.entries.len() >= MAX_SNAPSHOTS {
            let Some(oldest) = details
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.expires)
                .map(|(id, _)| id.clone())
            else {
                break;
            };
            details.entries.remove(&oldest);
        }
        let pages = snapshot.views[view].len().div_ceil(PAGE_FIELDS);
        let mut page_fields = vec![
            serde_json::json!({"label":"会话编号","value":snapshot.session}),
            serde_json::json!({"label":"员工编号","value":snapshot.employee}),
        ];
        fields("员工姓名", snapshot.name.clone(), &mut page_fields);
        page_fields.extend(
            snapshot.views[view]
                .iter()
                .skip(page * PAGE_FIELDS)
                .take(PAGE_FIELDS)
                .cloned(),
        );
        let mut cursor = Cursor {
            snapshot: snapshot.clone(),
            view,
            expires: now + TTL,
            options: HashMap::new(),
        };
        let mut actions = Vec::new();
        let mut button = |label: &str, operation| {
            let token = format!("detail-{}", uuid::Uuid::new_v4().simple());
            cursor.options.insert(token.clone(), operation);
            actions.push(serde_json::json!({"id":token,"label":label}));
        };
        for (index, label) in VIEWS.into_iter().enumerate() {
            button(label, Operation::View(index));
        }
        if page > 0 {
            button("上一页", Operation::Page(page - 1));
        }
        if page + 1 < pages {
            button("下一页", Operation::Page(page + 1));
        }
        button("刷新", Operation::Refresh);
        let result = serde_json::json!({"template":"detail-v1","title":format!("员工只读详情 · {}",VIEWS[view]),
            "description":format!("快照时间：{} · 第 {}/{} 页。仅查看不会执行工具；刷新读取新记录。",snapshot.captured,page+1,pages),
            "fields":page_fields,"actions":actions});
        details.entries.insert(id.into(), cursor);
        result
    }
}
