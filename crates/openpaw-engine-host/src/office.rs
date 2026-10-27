//! 功能：把 AgentLoop 的工具和状态事件投影为 AI Office 动作。
//! 输入输出边界：输入为引擎事件和回合生命周期，输出为 `/actions` Gateway
//! 队列；Office 请求失败只记录日志，不反向改变引擎回合结果。
//! 设计约束：首个工具由 1 号协调角色交接，后续工具由上一工具角色交接；角色状态按
//! session owner 计数，避免一个 session 结束时错误地把另一个 session 的角色置为空闲。
//! 关联：`API-OFFICE-ACTION-001`、`REQ-B-OFFICE-003`、`CHG-2026-0003`。

use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use openpaw_engine::{AgentEvent, ClassifierStatus};
use reqwest::{Client, Url};
use serde_json::Value;
use tokio::sync::mpsc;

mod detail;

const MAX_SUBAGENT_OUTPUT_CHARS: usize = 20_000;
const SUBAGENT_OUTPUT_CHUNK_CHARS: usize = 96;

#[derive(Clone, PartialEq, serde::Serialize, serde::Deserialize)]
/// 功能：保存一个实际工作目录的办公室身份、聊天绑定与完整业务快照。
/// 字段/变体语义：绑定字段同时存在或同时缺省；child_tasks 仅记录当前子执行。
/// 数据来源：runtime 验证后的工作目录和实际执行事实。
/// 数据去向：同一 OPENPAW_HOME 下的 offices/registry.json。
/// 约束：teamId、会话绑定及递增 seq 不由浏览器覆盖。
/// 稳定性：Host 内部版本；不改变已有 Session 格式。
/// 关联：CHG-2026-0033、T1-12、T1-13。
pub(crate) struct MainOfficeRecord {
    /// runtime 验证并规范化的实际工作目录。
    pub(crate) workspace: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// 办公室首次有效提交所绑定的 main 对话身份。
    pub(crate) conversation: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// 由 runtime 创建的独立主会话身份。
    pub(crate) session: Option<String>,
    #[serde(default = "default_conversation_title")]
    /// main 会话标题；旧绑定保留其原来下发的名称。
    title: String,
    /// 已提交的完整业务快照和递增版本。
    pub(crate) snapshot: Value,
    #[serde(default)]
    child_tasks: std::collections::BTreeMap<String, String>,
    // 尚未创建会话的本地容量，不持久化也不投影为已存在员工。
    #[serde(skip)]
    child_reservations: usize,
    // 人物工作状态是活动展示，不作为恢复后仍在执行的依据。
    #[serde(skip)]
    presentations: std::collections::BTreeMap<String, (String, String)>,
}

fn default_conversation_title() -> String {
    "主对话".into()
}

fn projection_text(text: &str, limit: usize) -> String {
    let mut used = 0;
    text.chars()
        .take_while(|character| {
            used += character.len_utf16();
            used <= limit
        })
        .collect()
}

fn apply_goal_projection(snapshot: &mut Value, goal: &Value) {
    let tasks: Vec<_> = goal["tasks"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|task| task["current"] != false)
        .cloned()
        .collect();
    let phase = goal["phase"].as_str().unwrap_or("draft");
    let ended = phase == "ended";
    let count = tasks.len();
    let accepted = tasks
        .iter()
        .filter(|task| task["status"] == "accepted")
        .count();
    let progress = if goal["confirmed_version"].as_u64().unwrap_or(0) == 0 || count == 0 {
        -1
    } else {
        (accepted * 100 / count) as i64
    };
    snapshot["goal"] = serde_json::json!({"id":goal["id"],"phase":phase,"planVersion":goal["plan_version"],"acceptedTaskCount":accepted,"taskCount":count,"progress":progress,"reason":goal["reason"]});
    snapshot["tasks"] = Value::Array(tasks.iter().map(|task| {
        let criteria = task["acceptance_criteria"].as_array().into_iter().flatten().filter_map(Value::as_str).collect::<Vec<_>>().join("；");
        let dependencies = task["dependencies"].as_array().into_iter().flatten().filter_map(Value::as_str).collect::<Vec<_>>().join("、");
        let waiting = task["status"] == "pending" && task["dependencies"].as_array().into_iter().flatten().any(|dep|tasks.iter().any(|task|task["id"] == *dep && task["status"] != "accepted"));
        let reason = if waiting {format!("等待依赖：{dependencies}")} else {task["block_reason"].as_str().unwrap_or("").into()};
        let summary = format!("阻塞原因：{reason}\n预期结果：{}\n验收条件：{criteria}\n依赖：{}\n实际产出：{}\n验收依据：{}",task["expected_result"].as_str().unwrap_or(""),if dependencies.is_empty() {"无"} else {&dependencies},task["result"].as_str().unwrap_or("尚未产出"),if task["status"] == "accepted" {"已根据真实工具观察逐项核对，完整依据见聊天"} else {"未验证，不计为通过"});
        let source = task["manual"]["sources"].as_array().into_iter().flatten().map(|source| format!("用户人工暂停（来源任务 {}）：{}",source["task_id"].as_str().unwrap_or(""),source["user_excerpt"].as_str().unwrap_or(""))).collect::<Vec<_>>().join("；");
        let summary = if source.is_empty() {summary} else {format!("{source}\n{summary}")};
        let summary = if summary.encode_utf16().count() > 1000 { format!("{}…完整条件与依赖见聊天计划。",projection_text(&summary,960)) } else { summary };
        let status = if ended { "failed" } else if waiting { "blocked" } else { match task["status"].as_str() { Some("accepted")=>"completed",Some("running")=>"running",Some("produced"|"interrupted"|"blocked"|"manual_blocked")=>"blocked",_=>"queued" } };
        serde_json::json!({"id":format!("{}-{}",goal["id"].as_str().unwrap_or(""),task["id"].as_str().unwrap_or("")),"goalId":goal["id"],"assigneeId":task.get("child_workspace_id").unwrap_or(&goal["session_id"]),"title":task["title"],"status":status,"progress":if task["status"] == "accepted" {100} else {0},"updatedAt":chrono::Utc::now().to_rfc3339(),"summary":summary})
    }).collect());
    let status = match phase {
        "completed" => "completed",
        "ended" => "failed",
        "active" => "running",
        "paused" | "blocked" => "blocked",
        _ => "queued",
    };
    let steps: Vec<_> = tasks.iter().take(3).map(|task| serde_json::json!({"id":format!("{}-{}",goal["id"].as_str().unwrap_or(""),task["id"].as_str().unwrap_or("")),"title":projection_text(task["title"].as_str().unwrap_or(""),160),"progress":if task["status"] == "accepted" {100} else {0}})).collect();
    snapshot["whiteboard"] = serde_json::json!({"template":"task-board-v1","title":projection_text(goal["objective"].as_str().unwrap_or(""),160),"status":status,"tagline":projection_text(goal["reason"].as_str().unwrap_or(""),40),"description":format!("已验收 {accepted}/{count} 项；白板展示前{}项，完整任务与验收条件见任务列表和聊天计划",steps.len()),"note":if phase == "draft" {"计划待确认；每项验收完成后显示100%"} else {"任务全部通过仍需独立整体验收"},"link":{"label":"","url":""},"progressMode":"reported","steps":steps});
}

// 人物外观的名字属于展示名册；子会话职责和任务名称保留在原执行事实中。
fn roster_display_snapshot(mut snapshot: Value) -> Value {
    if let Some(conversations) = snapshot
        .get_mut("conversations")
        .and_then(Value::as_array_mut)
    {
        for conversation in conversations {
            *conversation = roster_display_snapshot(conversation.take());
        }
    }
    if let Some(employees) = snapshot.get_mut("employees").and_then(Value::as_array_mut) {
        for employee in employees {
            let name = match employee["sceneActorId"].as_str() {
                Some("office-lead") => "王明",
                Some("code-agent") => "李研",
                Some("file-agent") => "周理",
                Some("app-agent") => "陈书",
                Some("review-agent") => "刘市",
                Some("data-agent") => "赵审",
                _ => continue,
            };
            employee["name"] = Value::from(name);
        }
    }
    snapshot
}

// 原单绑定字段继续可读；额外会话拥有独立快照、成员名额及活动展示。
#[derive(Clone, PartialEq, serde::Serialize, serde::Deserialize)]
struct MainOfficeEntry {
    #[serde(flatten)]
    primary: MainOfficeRecord,
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    conversations: std::collections::BTreeMap<String, MainOfficeRecord>,
}

impl std::ops::Deref for MainOfficeEntry {
    type Target = MainOfficeRecord;
    fn deref(&self) -> &Self::Target {
        &self.primary
    }
}

impl std::ops::DerefMut for MainOfficeEntry {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.primary
    }
}

impl MainOfficeEntry {
    fn all(&self) -> impl Iterator<Item = &MainOfficeRecord> {
        std::iter::once(&self.primary).chain(self.conversations.values())
    }

    fn all_mut(&mut self) -> impl Iterator<Item = &mut MainOfficeRecord> {
        std::iter::once(&mut self.primary).chain(self.conversations.values_mut())
    }

    fn snapshot(&self, conversation_wire: bool) -> Value {
        if !conversation_wire {
            return roster_display_snapshot(self.primary.snapshot.clone());
        }
        let conversations = self
            .all()
            .filter_map(|record| {
                let mut value = serde_json::json!({"id":record.conversation.as_ref()?,"title":record.title,
                "employees":record.snapshot["employees"],"tasks":record.snapshot["tasks"],
                "events":record.snapshot["events"],"whiteboard":record.snapshot["whiteboard"]});
                if let Some(goal) = record.snapshot.get("goal") { value["goal"] = goal.clone(); }
                Some(value)
            })
            .collect::<Vec<_>>();
        // 每个会话的版本独立递增；总和保证任一会话更新都会推进办公室版本。
        let revision = self
            .all()
            .map(|record| record.snapshot["seq"].as_u64().unwrap_or(1))
            .sum::<u64>();
        roster_display_snapshot(
            serde_json::json!({"schemaVersion":"1.0","seq":revision,"workspace":self.primary.snapshot["workspace"],
            "system":self.primary.snapshot["system"],"conversations":conversations}),
        )
    }
}

/// 功能：管理 main 的持久办公室目录、七消息与有界展示投影。
/// 字段/变体语义：records 保存快照；会话历史仍归 runtime/Store。
/// 数据来源：Host 配置、工作目录与真实 Agent 生命周期。
/// 数据去向：同根持久卷及固定 main gateway。
/// 约束：展示失败不改变 AgentLoop 结果，不重放历史动作。
/// 稳定性：Host 内部实现，原 main 协议保持不变。
/// 关联：CHG-2026-0033、T1-01、T1-10。
pub(crate) struct MainOffice {
    /// 串行化首次绑定与聊天准入，不覆盖实际 runtime 执行锁。
    pub(crate) admission: tokio::sync::Mutex<()>,
    // 完整目录同步与 UI 创建回包互斥，避免新 ID 被提前初始化后遭到重复拒绝。
    directory_sync: tokio::sync::Mutex<()>,
    records: Mutex<Result<std::collections::BTreeMap<String, MainOfficeEntry>, String>>,
    path: PathBuf,
    /// 仅服务端配置的固定 main gateway 地址。
    pub(crate) url: Option<Url>,
    key: String,
    /// 复用连接且禁止重定向的客户端；仅用于服务端 Gateway 消息。
    pub(crate) client: Client,
    started: std::sync::atomic::AtomicBool,
    // 由原版 gateway 的能力响应协商；仅控制出站协议形状。
    conversation_wire: std::sync::atomic::AtomicBool,
    // 原版 main 的原生详情模式；只改变出站副本，不改变 Goal 执行事实。
    native_goal_details: std::sync::atomic::AtomicBool,
    projection_tx: mpsc::Sender<MainProjection>,
    projection_rx: Mutex<Option<mpsc::Receiver<MainProjection>>>,
    sessions: Mutex<HashMap<String, std::sync::Weak<crate::runtime::SessionHandle>>>,
    details: Mutex<detail::Details>,
    runtime: Mutex<std::sync::Weak<crate::runtime::AppState>>,
}

struct MainProjection {
    team: String,
    conversation: Option<String>,
    snapshot: Value,
    commands: Vec<Value>,
    expires: tokio::time::Instant,
    cancel: tokio_util::sync::CancellationToken,
    allow_cancelled: bool,
}

/// 功能：保留一个办公室新员工名额，防止并发创建超出协议容量。
/// 字段/变体语义：office 与 parent 定位唯一已绑定办公室；不包含工具或模型许可。
/// 数据来源：MainOffice::reserve_child 的原子本地检查。
/// 数据去向：登记成功后转换为持久员工；丢弃时释放临时计数。
/// 约束：仅适用于新身份，既有 task_id 续办不申请。
/// 稳定性：Host 内部类型，不改变 Office 或 Session 线协议。
/// 关联：子会话创建与容量准入。
pub(crate) struct MainChildReservation {
    office: Arc<MainOffice>,
    parent: String,
}

impl MainChildReservation {
    /// 功能：在子回合执行前可靠登记真实员工及稳定人物绑定。
    /// 参数：child 为真实子会话 ID，employee 为其兼容展示空间身份，name 为已校验展示名。
    /// 返回：事实及快照都已提交时成功。
    /// 错误：持久化错误明确返回子身份；事实已落盘时仍保留身份以供恢复。
    /// 状态：新增员工但不伪造执行状态、任务或交接；预留随后由 Drop 释放。
    /// 并发与取消：共享办公室短锁，不访问网络；登记完成前不接收子回合。
    /// 权限：仅 runtime 的真实子创建路径调用，不授予任何执行权限。
    /// 关联：主会话事实提交。
    pub(crate) fn commit(self, child: &str, employee: &str, name: &str) -> Result<(), String> {
        self.office
            .register_child(&self.parent, employee, name)
            .map_err(|error| format!("子会话 {child} 未启动：{error}"))
    }
}

impl Drop for MainChildReservation {
    fn drop(&mut self) {
        let mut guard = self
            .office
            .records
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Ok(records) = guard.as_mut() {
            if let Some(record) = records
                .values_mut()
                .flat_map(MainOfficeEntry::all_mut)
                .find(|record| record.session.as_deref() == Some(&self.parent))
            {
                record.child_reservations = record.child_reservations.saturating_sub(1);
            }
        }
    }
}

impl MainOffice {
    /// 功能：加载办公室目录或为配置的工作目录创建稳定身份。
    /// 参数：home、workspace 和可选受控 gateway 地址。
    /// 返回：共享目录，加载失败保留错误并拒绝聊天。
    /// 错误：错误延迟由目录/绑定操作返回，不清除已有状态。
    /// 状态：只在无历史记录时创建初始快照。
    /// 并发与取消：短同步锁保护原子记录；聊天准入另行串行。
    /// 权限：只读取服务端地址与密钥，不接受浏览器上游配置。
    /// 关联：CHG-2026-0033、T1-13。
    pub(crate) fn new(home: &Path, workspace: &Path, url: Option<String>) -> Arc<Self> {
        let path = home.join("offices").join("registry.json");
        let records = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|_| "办公室记录损坏".into()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(std::collections::BTreeMap::new())
            }
            Err(_) => Err("办公室记录读取失败".into()),
        };
        let (projection_tx, projection_rx) = mpsc::channel(32);
        let this = Arc::new(Self {
            admission: tokio::sync::Mutex::new(()),
            directory_sync: tokio::sync::Mutex::new(()),
            records: Mutex::new(records),
            path,
            url: url
                .and_then(|value| Url::parse(&value).ok())
                .filter(|url| matches!(url.scheme(), "http" | "https") && url.host_str().is_some()),
            key: std::env::var("OPENPAW_MAIN_API_KEY").unwrap_or_default(),
            client: Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .expect("HTTP client"),
            started: std::sync::atomic::AtomicBool::new(false),
            conversation_wire: std::sync::atomic::AtomicBool::new(false),
            native_goal_details: std::sync::atomic::AtomicBool::new(false),
            projection_tx,
            projection_rx: Mutex::new(Some(projection_rx)),
            sessions: Mutex::new(HashMap::new()),
            details: Mutex::new(detail::Details::default()),
            runtime: Mutex::new(std::sync::Weak::new()),
        });
        if let Err(error) = this.register(workspace) {
            *this.records.lock().unwrap_or_else(|e| e.into_inner()) = Err(error);
        }
        if let Err(error) = this.recover(home) {
            *this.records.lock().unwrap_or_else(|e| e.into_inner()) = Err(error);
        }
        this
    }

    /// 功能：关联 runtime 会话，事实写入继续由 runtime 管理。
    /// 参数：handle 为原 SessionHandle。
    /// 返回：无。
    /// 错误：无。
    /// 状态：仅保存弱引用，不创建新历史存储。
    /// 并发与取消：短锁，不跨 await。
    /// 权限：仅内部调用，办公室限制不影响其他 OpenWork 会话。
    /// 关联：T1-12。
    pub(crate) fn attach_session(&self, handle: &Arc<crate::runtime::SessionHandle>) {
        self.sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(handle.id.clone(), Arc::downgrade(handle));
    }

    /// 功能：关联只读员工冷加载所需的原运行时。
    /// 参数：runtime 为当前 Host 的共享状态。
    /// 返回：无。
    /// 错误：锁中毒时恢复弱引用槽。
    /// 状态：只保存弱引用，不创建会话或执行。
    /// 并发与取消：短同步锁，不形成循环所有权。
    /// 权限：只允许后续已验证员工使用原会话加载入口。
    /// 关联：CHG-2026-0040、ADR-003。
    pub(crate) fn bind_runtime(&self, runtime: &Arc<crate::runtime::AppState>) {
        *self.runtime.lock().unwrap_or_else(|e| e.into_inner()) = Arc::downgrade(runtime);
    }

    fn recover(&self, home: &Path) -> Result<(), String> {
        let mut guard = self.records.lock().unwrap_or_else(|e| e.into_inner());
        let records = guard.as_mut().map_err(|error| error.clone())?;
        let mut next = records.clone();
        for (team, entry) in &mut next {
            if entry.conversations.iter().any(|(id, record)| {
                record.conversation.as_deref() != Some(id)
                    || record.workspace != entry.primary.workspace
            }) {
                return Err("办公室对话目录结构损坏".into());
            }
            for record in entry.all_mut() {
                if record.snapshot["workspace"]["id"] != *team
                    || record.conversation.is_some() != record.session.is_some()
                    || !Self::valid_facts(&record.snapshot)
                {
                    return Err("办公室记录结构损坏".into());
                }
                let Some(session) = &record.session else {
                    continue;
                };
                let previous = record.snapshot.clone();
                let mut revision = record.snapshot["seq"].as_u64().ok_or("办公室版本损坏")?;
                if let Some(fact) = crate::runtime::latest_main_fact(home, session)? {
                    if !Self::valid_facts(&fact) {
                        return Err("办公室执行事实损坏".into());
                    }
                    revision = revision.max(fact["seq"].as_u64().ok_or("办公室事实版本损坏")?);
                    record.snapshot["employees"] = fact["employees"].clone();
                    record.snapshot["tasks"] = fact["tasks"].clone();
                }
                let employees = record.snapshot["employees"]
                    .as_array_mut()
                    .ok_or("办公室人员记录损坏")?;
                for (_id, child) in
                    crate::runtime::restored_office_children(home, session, &record.workspace)
                {
                    if employees.len() < 200
                        && !employees
                            .iter()
                            .any(|employee| employee["id"] == child.workspace_id)
                    {
                        // 已存在的人物绑定不变；恢复缺失列表项也不借用其他员工人物。
                        employees.push(serde_json::json!({"id":child.workspace_id,"name":child.name,"role":"子 Agent","sceneActorId":"","online":false}));
                    }
                }
                for legacy in crate::delegation::legacy_records(&record.workspace, session) {
                    if employees.len() < 200
                        && !employees
                            .iter()
                            .any(|employee| employee["id"] == legacy["id"])
                    {
                        employees.push(serde_json::json!({"id":legacy["id"],"name":legacy["name"],"role":"历史子 Agent（完整历史未保存）","sceneActorId":"","online":false}));
                    }
                }
                record.child_tasks.clear();
                record.snapshot["events"] = serde_json::json!([]);
                for employee in record.snapshot["employees"]
                    .as_array_mut()
                    .ok_or("办公室人员记录损坏")?
                {
                    employee["online"] = Value::Bool(false);
                }
                for task in record.snapshot["tasks"]
                    .as_array_mut()
                    .ok_or("办公室任务记录损坏")?
                {
                    if matches!(
                        task["status"].as_str(),
                        Some("running" | "queued" | "blocked")
                    ) {
                        task["status"] = Value::from("failed");
                        task["summary"] = Value::from("重启中断，未自动继续执行");
                        task["updatedAt"] = Value::from(chrono::Utc::now().to_rfc3339());
                    }
                }
                if let Some(goal) = crate::runtime::latest_goal(home, session)? {
                    apply_goal_projection(&mut record.snapshot, &goal);
                }
                if previous != record.snapshot || revision > previous["seq"].as_u64().unwrap_or(0) {
                    if revision >= 9_007_199_254_740_991 {
                        return Err("办公室快照版本已耗尽".into());
                    }
                    record.snapshot["seq"] = Value::from(revision + 1);
                }
            }
        }
        if *records != next {
            self.save(&next)?;
            *records = next;
        }
        Ok(())
    }

    fn valid_facts(fact: &Value) -> bool {
        if let Some(conversations) = fact["conversations"].as_array() {
            return conversations.len() <= 100 && conversations.iter().all(|conversation| {
                Self::valid_facts(&serde_json::json!({"seq":fact["seq"],"employees":conversation["employees"],"tasks":conversation["tasks"]}))
            });
        }
        let Some(employees) = fact["employees"].as_array() else {
            return false;
        };
        let Some(tasks) = fact["tasks"].as_array() else {
            return false;
        };
        fact["seq"]
            .as_u64()
            .is_some_and(|seq| (1..=9_007_199_254_740_991).contains(&seq))
            && employees.len() <= 200
            && tasks.len() <= 1000
            && employees.iter().all(|employee| {
                employee.is_object()
                    && employee["id"].is_string()
                    && employee["sceneActorId"].is_string()
                    && employee["name"].is_string()
                    && employee["online"].is_boolean()
            })
            && tasks.iter().all(|task| {
                task.is_object()
                    && task["id"].is_string()
                    && task["assigneeId"].as_str().is_some_and(|owner| {
                        owner.is_empty() || employees.iter().any(|employee| employee["id"] == owner)
                    })
                    && matches!(
                        task["status"].as_str(),
                        Some("queued" | "running" | "blocked" | "completed" | "failed")
                    )
            })
    }

    fn save(
        &self,
        records: &std::collections::BTreeMap<String, MainOfficeEntry>,
    ) -> Result<(), String> {
        use std::io::Write;
        for entry in records.values() {
            if entry
                .all()
                .map(|record| record.snapshot["seq"].as_u64().unwrap_or(1))
                .sum::<u64>()
                > 9_007_199_254_740_991
            {
                return Err("办公室快照版本已耗尽".into());
            }
        }
        let parent = self.path.parent().ok_or("办公室存储路径无效")?;
        std::fs::create_dir_all(parent).map_err(|_| "办公室目录创建失败")?;
        let temporary = parent.join(format!("{}.tmp", uuid::Uuid::new_v4().simple()));
        let result = (|| {
            let mut file = std::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&temporary)
                .map_err(|_| "办公室记录创建失败")?;
            let bytes = serde_json::to_vec(records).map_err(|_| "办公室记录编码失败")?;
            file.write_all(&bytes)
                .and_then(|_| file.sync_all())
                .map_err(|_| "办公室记录写入失败")?;
            std::fs::rename(&temporary, &self.path).map_err(|_| "办公室记录提交失败")?;
            #[cfg(unix)]
            std::fs::File::open(parent)
                .and_then(|f| f.sync_all())
                .map_err(|_| "办公室目录提交失败")?;
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(temporary);
        }
        result
    }

    /// 功能：在可信工作根目录下创建真实子目录并登记办公室。
    /// 参数：root 为服务端配置的工作根目录；name 为单层工作空间名称。
    /// 返回：符合当前 main 协议的办公室快照。
    /// 错误：名称非法400、重名409、容量422、目录或登记故障500。
    /// 状态：仅建立目录和办公室，不创建会话、不启动模型或工具。
    /// 并发与取消：admission 串行创建；登记失败只回收本次新建空目录。
    /// 权限：调用方完成认证，输入不能选择根目录或复用已有目录。
    /// 关联：API-HOST-OFFICE-WORKSPACE-001、CHG-2026-0047。
    pub(crate) async fn create_workspace(
        &self,
        root: &Path,
        name: &str,
    ) -> Result<Value, (u16, &'static str)> {
        let name = name.trim();
        let stem = name
            .split('.')
            .next()
            .unwrap_or_default()
            .trim_end()
            .to_ascii_uppercase();
        let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            || (stem.starts_with("COM") || stem.starts_with("LPT"))
                && stem.len() == 4
                && matches!(stem.as_bytes()[3], b'1'..=b'9');
        if name.is_empty()
            || name.encode_utf16().count() > 40
            || name.ends_with('.')
            || reserved
            || name
                .chars()
                .any(|c| c.is_control() || "/\\:*?\"<>|".contains(c))
        {
            return Err((400, "名称需要1–40个字符，不能包含路径、非法字符或设备名"));
        }
        let _admission = self.admission.lock().await;
        let offices = self.directory().map_err(|_| (500, "办公室目录无法读取"))?;
        if offices.iter().any(|office| {
            office["workspace"]["name"]
                .as_str()
                .is_some_and(|existing| existing.to_lowercase() == name.to_lowercase())
        }) {
            return Err((409, "这个工作空间名称已存在，请换一个名称"));
        }
        if offices.len() >= 100 {
            return Err((422, "最多支持100个办公室"));
        }
        let root = std::fs::canonicalize(root).map_err(|_| (500, "工作空间根目录不可用"))?;
        let path = root.join(name);
        std::fs::create_dir(&path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::AlreadyExists {
                (409, "同名目录已存在，未覆盖任何文件")
            } else {
                (500, "工作空间目录创建失败")
            }
        })?;
        if let Err(error) = self.register(&path) {
            let _ = std::fs::remove_dir(&path);
            return Err(if error == "办公室数量已达上限" {
                (422, "办公室数量已达上限")
            } else {
                (500, "办公室登记失败，未创建聊天")
            });
        }
        self.directory()
            .ok()
            .and_then(|offices| {
                offices
                    .into_iter()
                    .find(|office| office["workspace"]["name"] == name)
            })
            .map(|snapshot| self.wire_snapshot(&snapshot))
            .ok_or((500, "办公室登记状态无法读取"))
    }

    /// 功能：为实际工作目录登记稳定办公室。
    /// 参数：workspace 为 runtime 已验证的规范路径。
    /// 返回：无；已有目录保持原身份。
    /// 错误：磁盘错误或目录不存在。
    /// 状态：只新增目录映射，不创建聊天或回合。
    /// 并发与取消：短锁与原子提交，无网络。
    /// 权限：仅由服务端 runtime 调用。
    /// 关联：办公室对应工作目录。
    pub(crate) fn register(&self, workspace: &Path) -> Result<(), String> {
        let workspace = std::fs::canonicalize(workspace).map_err(|_| "办公室工作目录不存在")?;
        let mut guard = self.records.lock().unwrap_or_else(|e| e.into_inner());
        let records = guard.as_mut().map_err(|e| e.clone())?;
        if records.values().any(|record| record.workspace == workspace) {
            return Ok(());
        }
        if records.len() >= 100 {
            return Err("办公室数量已达上限".into());
        }
        let team = format!("office-{}", uuid::Uuid::new_v4().simple());
        let name = workspace
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .chars()
            .take(80)
            .collect::<String>();
        let snapshot = serde_json::json!({"schemaVersion":"1.0","seq":1,
            "workspace":{"id":team,"name":if name.is_empty(){"工作空间"}else{&name}},
            "employees":[],"tasks":[],"events":[],
            "system":{"cpuPercent":-1,"memoryPercent":-1,"networkKbps":-1},
            "whiteboard":{"template":"task-board-v1","title":"","status":"queued","tagline":"","description":"","note":"","link":{"label":"","url":""},"steps":[]}});
        let mut next = records.clone();
        next.insert(
            team,
            MainOfficeEntry {
                primary: MainOfficeRecord {
                    workspace,
                    conversation: None,
                    session: None,
                    title: default_conversation_title(),
                    snapshot,
                    child_tasks: std::collections::BTreeMap::new(),
                    child_reservations: 0,
                    presentations: std::collections::BTreeMap::new(),
                },
                conversations: Default::default(),
            },
        );
        self.save(&next)?;
        *records = next;
        Ok(())
    }

    fn directory_bytes(
        &self,
        records: &std::collections::BTreeMap<String, MainOfficeEntry>,
    ) -> Result<usize, String> {
        let wire = self
            .conversation_wire
            .load(std::sync::atomic::Ordering::Acquire);
        records
            .values()
            .map(|entry| {
                serde_json::to_vec(&serde_json::json!({"type":"offices.initialize",
                "offices":[self.display_snapshot(entry.snapshot(wire))]}))
                .map(|bytes| bytes.len())
                .map_err(|_| "办公室目录编码失败".to_string())
            })
            .collect::<Result<Vec<_>, _>>()
            .map(|sizes| sizes.into_iter().max().unwrap_or(0))
    }

    /// 功能：在创建子会话前原子预留已绑定办公室的成员容量。
    /// 参数：parent 为可信父会话身份。
    /// 返回：main 绑定父返回预留 guard；原 feature/OpenWork 独立父返回 None。
    /// 错误：记录不可读、成员已达200或单办公室无法保留64KiB发布预算时明确拒绝。
    /// 状态：仅增加暂存计数，既有员工包含主员工，不改变模型/进程额度。
    /// 并发与取消：所有创建使用原 records 锁；失败、取消和 future 丢弃释放 guard。
    /// 权限：不查询远端 Office，不因网络故障放开或拒绝名额。
    /// 关联：子会话创建与容量准入。
    pub(crate) fn reserve_child(
        self: &Arc<Self>,
        parent: &str,
    ) -> Result<Option<MainChildReservation>, String> {
        if self.url.is_none() {
            return Ok(None);
        }
        let mut guard = self.records.lock().unwrap_or_else(|e| e.into_inner());
        let records = guard.as_mut().map_err(|error| error.clone())?;
        let directory_bytes = self.directory_bytes(records)?;
        let pending = records
            .values()
            .flat_map(MainOfficeEntry::all)
            .map(|record| record.child_reservations)
            .sum::<usize>();
        let Some(record) = records
            .values_mut()
            .flat_map(MainOfficeEntry::all_mut)
            .find(|record| record.session.as_deref() == Some(parent))
        else {
            return Ok(None);
        };
        let employees = record.snapshot["employees"]
            .as_array()
            .ok_or("办公室人员记录损坏")?;
        let leader = usize::from(!employees.iter().any(|employee| employee["id"] == parent));
        if employees.len() + leader + record.child_reservations >= 200 {
            return Err("办公室成员容量已达200人（包含主员工），未创建或执行新子 Agent".into());
        }
        // 目录逐办公室发布；预留单帧新成员/任务及在途子回合的状态开销。
        // 不裁剪身份、清除业务事实或放大 main 的64KiB合同。
        if directory_bytes + (pending + 1) * 1000 + 4096 > 65536 {
            return Err("办公室快照发布容量不足（64 KiB），未创建或执行新子 Agent".into());
        }
        record.child_reservations += 1;
        Ok(Some(MainChildReservation {
            office: self.clone(),
            parent: parent.into(),
        }))
    }

    fn register_child(&self, parent: &str, child: &str, name: &str) -> Result<(), String> {
        let mut guard = self.records.lock().unwrap_or_else(|e| e.into_inner());
        let records = guard.as_mut().map_err(|error| error.clone())?;
        let mut next = records.clone();
        let (team, record) = next
            .iter_mut()
            .flat_map(|(team, entry)| entry.all_mut().map(move |record| (team, record)))
            .find(|(_, record)| record.session.as_deref() == Some(parent))
            .ok_or("办公室绑定已失效")?;
        let revision = record.snapshot["seq"]
            .as_u64()
            .filter(|seq| *seq < 9_007_199_254_740_991)
            .ok_or("办公室快照版本已耗尽")?
            + 1;
        let employees = record.snapshot["employees"]
            .as_array_mut()
            .ok_or("办公室人员记录损坏")?;
        if !employees.iter().any(|employee| employee["id"] == parent) {
            employees.push(serde_json::json!({"id":parent,"sceneActorId":"office-lead","name":"王明","role":"主 Agent","online":false}));
        }
        if employees.len() >= 200 || record.child_reservations == 0 {
            return Err("办公室成员预留失效".into());
        }
        let actor = [
            "code-agent",
            "file-agent",
            "app-agent",
            "review-agent",
            "data-agent",
        ]
        .into_iter()
        .find(|actor| {
            !employees
                .iter()
                .any(|employee| employee["sceneActorId"] == *actor)
        })
        .unwrap_or("");
        employees.push(serde_json::json!({"id":child,"sceneActorId":actor,"name":name,"role":"子 Agent","online":false}));
        record.snapshot["seq"] = Value::from(revision);
        record.snapshot["events"] = serde_json::json!([]);
        let handle = self
            .sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(parent)
            .and_then(std::sync::Weak::upgrade)
            .ok_or("父会话写入器不可用")?;
        crate::runtime::record_main_fact(
            &handle,
            serde_json::json!({"seq":revision,"employees":record.snapshot["employees"],"tasks":record.snapshot["tasks"]}),
        )?;
        let projection = MainProjection {
            team: team.clone(),
            conversation: record.conversation.clone(),
            snapshot: record.snapshot.clone(),
            commands: Vec::new(),
            expires: tokio::time::Instant::now() + Duration::from_secs(3),
            cancel: Default::default(),
            allow_cancelled: true,
        };
        let result = self.save(&next);
        // 已提交的会话事实不可因为办公室快照保存失败而从本机视图消失。
        *records = next;
        drop(guard);
        if result.is_ok() {
            let _ = self.projection_tx.try_send(projection);
        }
        result
    }

    /// 功能：读取当前完整办公室快照目录。
    /// 参数：无。
    /// 返回：按稳定 teamId 排列的快照。
    /// 错误：记录不可读时拒绝而非返回空目录。
    /// 状态：只读。
    /// 并发与取消：同步短锁，无网络。
    /// 权限：仅供 Host 内部受控出站使用。
    /// 关联：offices.initialize。
    pub(crate) fn directory(&self) -> Result<Vec<Value>, String> {
        self.records
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|records| {
                records
                    .values()
                    .map(|entry| {
                        self.display_snapshot(
                            entry.snapshot(
                                self.conversation_wire
                                    .load(std::sync::atomic::Ordering::Acquire),
                            ),
                        )
                    })
                    .collect()
            })
            .map_err(Clone::clone)
    }

    /// 功能：读取经过验证的办公室绑定。
    /// 参数：team 为浏览器提供的已登记办公室 ID。
    /// 返回：实际目录和稳定会话关联。
    /// 错误：未知身份或持久化错误。
    /// 状态：只读。
    /// 并发与取消：同步短锁，不跨 await。
    /// 权限：浏览器不能指定任意工作目录。
    /// 关联：T1-13。
    pub(crate) fn record(&self, team: &str) -> Result<MainOfficeRecord, String> {
        self.records
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map_err(Clone::clone)?
            .get(team)
            .map(|entry| entry.primary.clone())
            .ok_or_else(|| "办公室不存在".into())
    }

    /// 功能：在执行前提交首次 conversation 和 runtime Session 绑定。
    /// 参数：team、conversation、session 的稳定身份。
    /// 返回：已落盘后返回成功。
    /// 错误：拒绝覆盖已有绑定；磁盘失败不执行模型。
    /// 状态：原子更新目录记录。
    /// 并发与取消：由调用方持有准入锁；写入在短同步锁下完成。
    /// 权限：不接收浏览器指定的 Session。
    /// 关联：T1-13、T1-12。
    pub(crate) fn bind(&self, team: &str, conversation: &str, session: &str) -> Result<(), String> {
        self.bind_named(team, conversation, session, &default_conversation_title())
    }

    /// 功能：为办公室登记独立主会话及稳定标题。
    /// 参数：team、conversation、session 为已验证身份，title 为服务器生成标题。
    /// 返回：会话目录可靠提交后返回成功。
    /// 错误：重复身份、协议容量或磁盘失败时拒绝执行。
    /// 状态：保留旧绑定，额外会话具有独立业务快照。
    /// 并发与取消：调用方持准入锁，内部仅短锁与原子保存。
    /// 权限：不接受浏览器指定的实际 Session 或工作路径。
    /// 关联：CHG-2026-0045。
    pub(crate) fn bind_named(
        &self,
        team: &str,
        conversation: &str,
        session: &str,
        title: &str,
    ) -> Result<(), String> {
        let mut guard = self.records.lock().unwrap_or_else(|e| e.into_inner());
        let records = guard.as_mut().map_err(|e| e.clone())?;
        let mut next = records.clone();
        let entry = next.get_mut(team).ok_or("办公室不存在")?;
        if entry.all().any(|record| {
            record.conversation.as_deref() == Some(conversation)
                || record.session.as_deref() == Some(session)
        }) {
            return Err("对话已经绑定".into());
        }
        if entry
            .all()
            .filter(|record| record.conversation.is_some())
            .count()
            >= 100
        {
            return Err("办公室会话数量已达协议上限".into());
        }
        let record = if entry.primary.conversation.is_none() {
            &mut entry.primary
        } else {
            let mut record = entry.primary.clone();
            record.conversation = None;
            record.session = None;
            record.title = default_conversation_title();
            record.snapshot["seq"] = Value::from(1);
            for field in ["employees", "tasks", "events"] {
                record.snapshot[field] = serde_json::json!([]);
            }
            record
                .snapshot
                .as_object_mut()
                .expect("owned office snapshot")
                .remove("goal");
            record.snapshot["whiteboard"] = serde_json::json!({"template":"task-board-v1","title":"","status":"queued","tagline":"","description":"","note":"","link":{"label":"","url":""},"steps":[]});
            record.child_tasks.clear();
            record.child_reservations = 0;
            record.presentations.clear();
            entry
                .conversations
                .entry(conversation.into())
                .or_insert(record)
        };
        record.conversation = Some(conversation.into());
        record.session = Some(session.into());
        record.title = title.into();
        let seq = record.snapshot["seq"].as_u64().ok_or("办公室版本损坏")?;
        record.snapshot["seq"] = Value::from(
            seq.checked_add(1)
                .filter(|seq| *seq <= 9_007_199_254_740_991)
                .ok_or("办公室版本耗尽")?,
        );
        let bytes = self.directory_bytes(&next)?;
        if bytes + 4096 > 65536 {
            return Err("办公室快照发布容量不足（64 KiB），未启动新对话".into());
        }
        self.save(&next)?;
        *records = next;
        Ok(())
    }

    /// 功能：按办公室及对话读取真实绑定。
    /// 参数：team 和 conversation 为原版 main 请求身份。
    /// 返回：已保存会话或未发送草稿的 None。
    /// 错误：目录不可读、办公室不存在时明确失败。
    /// 状态：只读，不因历史查询创建会话。
    /// 并发与取消：同步短锁，不跨 await。
    /// 权限：不允许跨办公室查找同名对话。
    /// 关联：CHG-2026-0045。
    pub(crate) fn conversation_record(
        &self,
        team: &str,
        conversation: &str,
    ) -> Result<Option<MainOfficeRecord>, String> {
        let guard = self.records.lock().unwrap_or_else(|e| e.into_inner());
        let entry = guard
            .as_ref()
            .map_err(Clone::clone)?
            .get(team)
            .ok_or("办公室不存在")?;
        let record = entry
            .all()
            .find(|record| record.conversation.as_deref() == Some(conversation))
            .cloned();
        Ok(record)
    }

    /// 功能：为已领取的 UI 操作验证会话归属。
    /// 参数：team 为办公室，action 为原版 UI 请求。
    /// 返回：所属会话记录；旧协议保留原办公室绑定。
    /// 错误：会话缺失或不属于办公室时拒绝。
    /// 状态：只读。
    /// 并发与取消：短同步锁，不跨 await。
    /// 权限：不能通过人物外观或其他会话 ID 越权。
    /// 关联：CHG-2026-0045。
    pub(super) fn action_record(
        &self,
        team: &str,
        action: &Value,
    ) -> Result<MainOfficeRecord, String> {
        if !self
            .conversation_wire
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return self.record(team);
        }
        self.conversation_record(
            team,
            action["conversationId"].as_str().ok_or("缺少对话归属")?,
        )?
        .ok_or("对话不存在".into())
    }

    /// 功能：从实际员工身份定位其办公室会话。
    /// 参数：team 为办公室，employee 为已验证员工编号。
    /// 返回：持有该真实员工的会话记录。
    /// 错误：无；未知员工返回 None。
    /// 状态：只读，不建立或恢复执行。
    /// 并发与取消：短同步锁，不跨 await。
    /// 权限：仅由已核对 UI 作用域的只读详情调用。
    /// 关联：CHG-2026-0045。
    pub(super) fn employee_record(&self, team: &str, employee: &str) -> Option<MainOfficeRecord> {
        let guard = self.records.lock().unwrap_or_else(|e| e.into_inner());
        let entry = guard.as_ref().ok()?.get(team)?;
        let record = entry
            .all()
            .find(|record| {
                record.snapshot["employees"]
                    .as_array()
                    .is_some_and(|employees| employees.iter().any(|item| item["id"] == employee))
            })
            .cloned();
        record
    }

    /// 功能：从真实主会话定位其办公室会话归属。
    /// 参数：team 为已登记办公室，session 为可信主 Session。
    /// 返回：该办公室内的绑定记录，未知身份不回退。
    /// 错误：无；不可用时返回 None。
    /// 状态：只读。
    /// 并发与取消：短同步锁，不跨 await。
    /// 权限：仅供工具审批与内部投影使用。
    /// 关联：CHG-2026-0045。
    pub(crate) fn session_record(&self, team: &str, session: &str) -> Option<MainOfficeRecord> {
        let guard = self.records.lock().unwrap_or_else(|e| e.into_inner());
        let entry = guard.as_ref().ok()?.get(team)?;
        let record = entry
            .all()
            .find(|record| record.session.as_deref() == Some(session))
            .cloned();
        record
    }

    /// 功能：按已协商的原版 main 协议生成出站快照。
    /// 参数：snapshot 为 Host 原有内部快照。
    /// 返回：保留原人物名的旧协议或 conversations 快照；按能力适配原生Goal详情。
    /// 错误：无；未绑定时输出空会话目录。
    /// 状态：只读，不迁移持久记录。
    /// 并发与取消：同步短锁，不等待网络。
    /// 权限：会话身份仅来自办公室登记。
    /// 关联：CHG-2026-0044、CHG-2026-0046。
    pub(crate) fn wire_snapshot(&self, snapshot: &Value) -> Value {
        if !self
            .conversation_wire
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return self.display_snapshot(roster_display_snapshot(snapshot.clone()));
        }
        let guard = self.records.lock().unwrap_or_else(|e| e.into_inner());
        let snapshot = snapshot["workspace"]["id"]
            .as_str()
            .and_then(|team| guard.as_ref().ok()?.get(team))
            .map(|entry| entry.snapshot(true))
            .unwrap_or_else(|| roster_display_snapshot(snapshot.clone()));
        self.display_snapshot(snapshot)
    }

    fn display_snapshot(&self, mut snapshot: Value) -> Value {
        if !self
            .native_goal_details
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return snapshot;
        }
        fn adapt(snapshot: &mut Value) {
            if let Some(conversations) = snapshot
                .get_mut("conversations")
                .and_then(Value::as_array_mut)
            {
                for conversation in conversations {
                    adapt(conversation);
                }
            }
            let Some(goal) = snapshot
                .as_object_mut()
                .and_then(|fields| fields.remove("goal"))
            else {
                return;
            };
            let mut summary = format!(
                "已验收 {}/{} 项。白板为时间动画，Goal整体验收后统一100%。",
                goal["acceptedTaskCount"], goal["taskCount"]
            );
            let phase = match goal["phase"].as_str() {
                Some("active") => "执行中",
                Some("paused") => "已暂停",
                Some("blocked") => "等待条件",
                Some("completed") => "整体验收通过",
                Some("ended") => "已终止",
                _ => "计划待确认",
            };
            let mut detail = format!("# {}\n\n目标状态：{phase}；已验收 {}/{} 项。\n\n{}\n\n任务全部完成后仍需独立整体验收。\n\n", snapshot["whiteboard"]["title"].as_str().unwrap_or("目标"), goal["acceptedTaskCount"], goal["taskCount"], goal["reason"].as_str().unwrap_or(""));
            let tasks = snapshot["tasks"].as_array_mut().expect("owned Goal tasks");
            for (index, task) in tasks.iter_mut().enumerate() {
                task.as_object_mut()
                    .expect("owned Goal task")
                    .remove("goalId");
                let title = task["title"]
                    .as_str()
                    .unwrap_or("")
                    .replace(['\n', '\r'], " ");
                let progress = task["progress"].as_u64().unwrap_or(0);
                if index < 3 {
                    summary.push_str(&format!(
                        " {}. {}：{progress}%。",
                        index + 1,
                        projection_text(&title, 48)
                    ));
                }
                detail.push_str(&format!(
                    "## {}. {title} — {progress}%\n\n{}\n\n",
                    index + 1,
                    task["summary"].as_str().unwrap_or("")
                ));
            }
            detail.push_str("完整任务与验收依据见任务列表和聊天计划。");
            let board = snapshot["whiteboard"]
                .as_object_mut()
                .expect("owned Goal board");
            board.remove("progressMode");
            board.insert("summary".into(), projection_text(&summary, 300).into());
            board.insert("detail".into(), projection_text(&detail, 16000).into());
            // 保留原生三项进度动画；仅整个Goal完成后统一100，任务验收事实不变。
        }
        adapt(&mut snapshot);
        snapshot
    }

    fn wire_message(&self, mut message: Value) -> Value {
        if message["type"] == "offices.initialize" {
            if let Some(snapshots) = message["offices"].as_array_mut() {
                for snapshot in snapshots {
                    *snapshot = self.wire_snapshot(snapshot);
                }
            }
        } else if message["type"] == "office.snapshot" {
            message["snapshot"] = self.wire_snapshot(&message["snapshot"]);
        }
        if !self
            .conversation_wire
            .load(std::sync::atomic::Ordering::Acquire)
        {
            if let Some(message) = message.as_object_mut() {
                message.remove("conversationId");
            }
            return message;
        }
        if (message["type"] == "decision.request" || message.get("command").is_some())
            && message["conversationId"].is_null()
        {
            let guard = self.records.lock().unwrap_or_else(|e| e.into_inner());
            let conversation = message["teamId"].as_str().and_then(|team| {
                let entry = guard.as_ref().ok()?.get(team)?;
                // 旧单会话消息可以补全；多会话缺少归属时保留缺失，交协议校验拒绝。
                entry
                    .conversations
                    .is_empty()
                    .then(|| entry.conversation.clone())
                    .flatten()
            });
            if let Some(conversation) = conversation {
                message["conversationId"] = Value::from(conversation);
            }
        }
        message
    }

    /// 功能：向固定 main gateway 发送原协议消息并保留实际状态及完整 data。
    /// 参数：message 为原合同 JSON；timeout 为含等待菜单期限的 HTTP 预算。
    /// 返回：实际 HTTP 状态和未裁剪的 data；错误 envelope 仍保留业务分类。
    /// 错误：网络结果不明、超限、null、code 不匹配及非法 envelope 返回安全错误。
    /// 状态：不自动重试，不将 cached/ignored 当作已应用。
    /// 并发与取消：丢弃 future 即关闭该 HTTP 请求，用于撤销待决卡片。
    /// 权限：仅附服务端 Bearer，不接受浏览器上游 URL。
    /// 关联：main 七消息合同、T1-01。
    pub(crate) async fn message(
        &self,
        message: Value,
        timeout: Duration,
    ) -> Result<(u16, Value), String> {
        let message = self.wire_message(message);
        fn valid(value: &Value, depth: usize) -> bool {
            depth <= 64
                && match value {
                    Value::Null => false,
                    Value::Array(values) => values.iter().all(|v| valid(v, depth + 1)),
                    Value::Object(values) => values
                        .iter()
                        .all(|(key, v)| key != "__proto__" && valid(v, depth + 1)),
                    _ => true,
                }
        }
        let bytes = serde_json::to_vec(&message).map_err(|_| "main 消息编码失败")?;
        if bytes.len() > 65536 || !valid(&message, 0) {
            return Err("main 消息超限或包含非法值".into());
        }
        let url = self
            .url
            .as_ref()
            .ok_or("main gateway 未配置")?
            .join("/api/pawoffice/messages")
            .map_err(|_| "main 地址无效")?;
        let mut response = self
            .client
            .post(url)
            .bearer_auth(&self.key)
            .timeout(timeout)
            .json(&message)
            .send()
            .await
            .map_err(|_| "main 请求结果不明")?;
        let status = response.status().as_u16();
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| "main 响应中断")? {
            if bytes.len() + chunk.len() > 4_000_000 {
                return Err("main 响应超限".into());
            }
            bytes.extend_from_slice(&chunk);
        }
        let value: Value = serde_json::from_slice(&bytes).map_err(|_| "main 响应不是合法 JSON")?;
        if !valid(&value, 0)
            || value["code"].as_u64() != Some(u64::from(status))
            || !value["msg"].is_string()
            || !value["data"].is_object()
        {
            return Err("main 响应 envelope 不合法".into());
        }
        let data = value["data"].clone();
        if status == 200 && message["type"] == "office.capabilities" {
            fn has_conversations(value: &Value) -> bool {
                match value {
                    Value::Object(fields) => {
                        fields
                            .get("properties")
                            .and_then(Value::as_object)
                            .is_some_and(|properties| properties.contains_key("conversations"))
                            || fields.values().any(has_conversations)
                    }
                    Value::Array(values) => values.iter().any(has_conversations),
                    _ => false,
                }
            }
            self.conversation_wire.store(
                has_conversations(&data["requests"]),
                std::sync::atomic::Ordering::Release,
            );
            self.native_goal_details.store(
                data["ui"]["whiteboardSchema"]["properties"]["detail"].is_object()
                    && data["ui"]["whiteboardSchema"]["properties"]
                        .get("progressMode")
                        .is_none(),
                std::sync::atomic::Ordering::Release,
            );
        }
        if status >= 400
            && (!data["reasonCode"].is_string()
                || !data["details"].is_object()
                || !data["issues"].is_array())
        {
            return Err("main 错误响应不合法".into());
        }
        if status < 400 {
            let directory = || {
                data["offices"].as_array().is_some_and(|offices| {
                    offices.iter().all(|office| {
                        office["id"].is_string()
                            && office["name"].is_string()
                            && office["seq"].as_u64().is_some()
                            && office["runtimeReady"].is_boolean()
                            && office["taskCount"].as_u64().is_some()
                    })
                }) && data["actors"].is_array()
                    && data["supportedMessages"].is_array()
                    && data["sceneCommandProtocol"] == "2.0"
                    && data["ui"].is_object()
            };
            let matches_contract = match message["type"].as_str() {
                Some("office.capabilities") => {
                    status == 200
                        && data["scene"].is_object()
                        && data["ui"].is_object()
                        && data["requests"].is_object()
                        && data["responses"]["success"].is_object()
                        && data["responses"]["error"].is_object()
                }
                Some("offices.initialize") => matches!(status, 200 | 202) && directory(),
                Some("office.read") if message["teamId"].as_str().is_none_or(str::is_empty) => {
                    status == 200 && directory()
                }
                Some("office.read") => {
                    status == 200
                        && data["snapshot"]["workspace"]["id"] == message["teamId"]
                        && Self::valid_facts(&data["snapshot"])
                        && data["scene"].is_object()
                        && data["pendingDecisions"].is_array()
                }
                Some("office.snapshot") => {
                    data["teamId"] == message["teamId"]
                        && (data["seq"] == message["snapshot"]["seq"]
                            && (status == 202 && data["status"] == "cached"
                                || status == 200 && data["status"] == "applied")
                            || status == 200
                                && data["status"] == "ignored"
                                && data["seq"]
                                    .as_u64()
                                    .zip(message["snapshot"]["seq"].as_u64())
                                    .is_some_and(|(actual, sent)| actual >= sent)
                                && matches!(
                                    data["reasonCode"].as_str(),
                                    Some("STALE_SEQ" | "DUPLICATE_SEQ")
                                ))
                }
                Some("ui.next") => {
                    status == 200
                        && data["actions"].as_array().is_some_and(|actions| {
                            actions.len() <= 1
                                && actions.iter().all(|action| {
                                    action["id"].is_string()
                                        && action["teamId"].is_string()
                                        && action["action"].is_string()
                                })
                        })
                }
                Some("ui.respond") => {
                    status == 200
                        && data["interactionId"] == message["interactionId"]
                        && data["delivered"] == true
                }
                Some("decision.request") => {
                    status == 200
                        && data["teamId"] == message["teamId"]
                        && (message.get("conversationId").is_none()
                            || data["conversationId"] == message["conversationId"])
                        && data["decisionId"] == message["decision"]["id"]
                        && data["choiceId"].is_string()
                        && data["label"].is_string()
                }
                _ => status == 200 && data["forwarded"].as_u64().is_some(),
            };
            if !matches_contract {
                return Err("main 响应字段与原请求不匹配".into());
            }
        }
        Ok((status, data))
    }

    fn clear_transient_events(&self) -> Result<(), String> {
        let mut guard = self.records.lock().unwrap_or_else(|e| e.into_inner());
        let records = guard.as_mut().map_err(|error| error.clone())?;
        let mut next = records.clone();
        for record in next.values_mut().flat_map(MainOfficeEntry::all_mut) {
            if record.snapshot["events"]
                .as_array()
                .is_some_and(|events| !events.is_empty())
            {
                let seq = record.snapshot["seq"].as_u64().ok_or("办公室版本损坏")?;
                if seq >= 9_007_199_254_740_991 {
                    return Err("办公室版本已耗尽".into());
                }
                record.snapshot["events"] = serde_json::json!([]);
                record.snapshot["seq"] = Value::from(seq + 1);
            }
        }
        if next != *records {
            self.save(&next)?;
            *records = next;
        }
        Ok(())
    }

    async fn restore_presentations(&self) -> Result<(), String> {
        let current = {
            let guard = self.records.lock().unwrap_or_else(|e| e.into_inner());
            let records = guard.as_ref().map_err(Clone::clone)?;
            records
                .iter()
                .flat_map(|(team, entry)| entry.all().map(move |record| (team, record)))
                .map(|(team, record)| {
                    (
                        team.clone(),
                        record.conversation.clone(),
                        record.session.clone(),
                        record.snapshot.clone(),
                        record.presentations.clone(),
                    )
                })
                .collect::<Vec<_>>()
        };
        for (team, conversation, session, snapshot, actors) in current {
            let handle = session.as_ref().and_then(|session| {
                self.sessions
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .get(session)
                    .and_then(std::sync::Weak::upgrade)
            });
            let Some(handle) = handle.filter(|handle| handle.is_active()) else {
                continue;
            };
            let cancel = handle.main_cancel_token();
            let commands = actors
                .into_iter()
                .map(|(actor, (status, title))| Self::command(&team, &actor, &status, &title, 1))
                .collect();
            self.publish_commands(&MainProjection {
                team,
                conversation,
                snapshot,
                commands,
                cancel,
                allow_cancelled: false,
                expires: tokio::time::Instant::now() + Duration::from_secs(2),
            })
            .await?;
        }
        Ok(())
    }

    /// 功能：续看历史时请求重新应用当前人物状态。
    /// 参数：session 为已验证且已加载的主会话身份。
    /// 返回：无；展示队列满时由原周期同步恢复。
    /// 错误：无；未绑定或不在运行的会话忽略。
    /// 状态：只排队现有快照，不写业务事实、不重放动作或执行工具。
    /// 并发与取消：短同步锁和非阻塞队列，不阻塞聊天准入。
    /// 权限：不接收浏览器人物或路径，按真实 Session 定位。
    /// 关联：CHG-2026-0045、A6。
    pub(crate) fn refresh_presentation(&self, session: &str) {
        let handle = self
            .sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(session)
            .and_then(std::sync::Weak::upgrade);
        let Some(handle) = handle.filter(|handle| handle.is_active()) else {
            return;
        };
        let guard = self.records.lock().unwrap_or_else(|e| e.into_inner());
        let Ok(records) = guard.as_ref() else { return };
        let Some((team, record)) = records
            .iter()
            .flat_map(|(team, entry)| entry.all().map(move |record| (team, record)))
            .find(|(_, record)| record.session.as_deref() == Some(session))
        else {
            return;
        };
        let _ = self.projection_tx.try_send(MainProjection {
            team: team.clone(),
            conversation: record.conversation.clone(),
            snapshot: record.snapshot.clone(),
            commands: Vec::new(),
            expires: tokio::time::Instant::now() + Duration::from_secs(3),
            cancel: handle.main_cancel_token(),
            allow_cancelled: false,
        });
    }

    async fn publish_commands(&self, projection: &MainProjection) -> Result<(), String> {
        if projection.commands.is_empty() {
            return Ok(());
        }
        let work = async {
            // 展示版本由原页面和 Host 共同推进，不能把业务 seq 当作人物版本。
            let (_, scene) = self
                .message(
                    serde_json::json!({"type":"office.read","teamId":projection.team}),
                    Duration::from_secs(1),
                )
                .await?;
            let mut revisions = scene["scene"]["actors"]
                .as_array()
                .ok_or("main 人物状态不可读")?
                .iter()
                .filter_map(|actor| {
                    Some((
                        actor["id"].as_str()?.to_string(),
                        actor["presentation"]["sourceRevision"].as_u64()?,
                    ))
                })
                .collect::<HashMap<_, _>>();
            for original in &projection.commands {
                let mut command = original.clone();
                if command["type"] == "actor.presentation.set" {
                    let revision = revisions
                        .get_mut(command["actorId"].as_str().unwrap_or(""))
                        .ok_or("main 人物不存在")?;
                    *revision = revision
                        .checked_add(1)
                        .filter(|revision| *revision <= 9_007_199_254_740_991)
                        .ok_or("main 人物版本耗尽")?;
                    command["sourceRevision"] = Value::from(*revision);
                }
                self.message(
                    serde_json::json!({"teamId":projection.team,"conversationId":projection.conversation,"command":command}),
                    Duration::from_secs(1),
                )
                .await?;
            }
            Ok(())
        };
        tokio::select! { biased;
            _ = projection.cancel.cancelled(), if !projection.allow_cancelled => Ok(()),
            _ = tokio::time::sleep_until(projection.expires) => Ok(()),
            result = work => result,
        }
    }

    /// 功能：启动办公室当前状态同步及原 UI 操作领取，单个 Host 只启动一次。
    /// 参数：shutdown 为 Host 关闭令牌。
    /// 返回：无。
    /// 错误：仅安全日志，离线不改变模型回合结果。
    /// 状态：有限间隔重试目录、可选聊天地址与当前快照，不恢复执行或历史动画。
    /// 并发与取消：两个独立任务，UI 长轮询不阻塞快照；关闭时丢弃 HTTP future。
    /// 权限：创建仅使用服务端根目录；聊天地址仅来自可信环境，不附浏览器凭据。
    /// 关联：T1-01、T1-08、T1-10、CHG-2026-0047、CHG-2026-0048。
    pub(crate) fn start(self: &Arc<Self>, shutdown: tokio_util::sync::CancellationToken) {
        use std::sync::atomic::Ordering;
        if self.url.is_none() || self.started.swap(true, Ordering::AcqRel) {
            return;
        }
        let configured_chat = match std::env::var("OPENPAW_MAIN_CHAT_URL") {
            Err(std::env::VarError::NotPresent) => Ok(None),
            Err(_) => Err(()),
            Ok(value) if value.trim().is_empty() => Ok(None),
            Ok(value) => Url::parse(value.trim())
                .ok()
                .filter(|url| {
                    matches!(url.scheme(), "http" | "https")
                        && url.host_str().is_some()
                        && url.username().is_empty()
                        && url.password().is_none()
                        && url.fragment().is_none()
                })
                .map(Some)
                .ok_or(()),
        };
        if configured_chat.is_err() {
            tracing::warn!("main 聊天地址配置无效，拒绝目录初始化；不影响引擎执行");
        }
        let weak = Arc::downgrade(self);
        let stop = shutdown.clone();
        let mut receiver = self
            .projection_rx
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
            .expect("single main worker");
        tokio::spawn(async move {
            // 即使页面已经就绪，Host 首轮仍须登记本次配置；Gateway 丢失目录后再次登记。
            let mut chat_registered = false;
            let mut interval = tokio::time::interval(Duration::from_secs(5));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                let projection = tokio::select! { biased;
                    _ = stop.cancelled() => break,
                    _ = interval.tick() => None,
                    projection = receiver.recv() => match projection {Some(projection)=>Some(projection),None=>break},
                };
                let Some(office) = weak.upgrade() else {
                    break;
                };
                let sync = async {
                    if let Some(projection) = projection {
                        if tokio::time::Instant::now() > projection.expires {
                            return Ok(());
                        }
                        if projection.cancel.is_cancelled() && !projection.allow_cancelled {
                            return Ok(());
                        }
                        let request = office.message(serde_json::json!({"type":"office.snapshot","teamId":projection.team,"snapshot":projection.snapshot}),Duration::from_secs(2));
                        let (status, data) = tokio::select! { biased;
                            _ = projection.cancel.cancelled(), if !projection.allow_cancelled => return Ok(()),
                            _ = tokio::time::sleep_until(projection.expires) => return Ok(()),
                            result = request => result?,
                        };
                        if !matches!(status, 200 | 202)
                            || !matches!(
                                data["status"].as_str(),
                                Some("applied" | "cached" | "ignored")
                            )
                        {
                            return Err("main 快照未同步".into());
                        }
                        if status == 200
                            && matches!(data["status"].as_str(), Some("applied" | "ignored"))
                            && tokio::time::Instant::now() <= projection.expires
                        {
                            office.publish_commands(&projection).await?;
                            // 聚合快照会重算当前页面的全部人物，来源会话动作不足以恢复其他活动会话。
                            // 仅补当前状态；交接、拜访等瞬时动作仍只发送一次。
                            let _ = tokio::time::timeout(
                                Duration::from_secs(2),
                                office.restore_presentations(),
                            )
                            .await;
                        }
                        return Ok(());
                    }
                    let chat = configured_chat
                        .as_ref()
                        .map_err(|()| "main 聊天地址配置无效".to_string())?;
                    let (status, capabilities) = office
                        .message(
                            serde_json::json!({"type":"office.capabilities"}),
                            Duration::from_secs(5),
                        )
                        .await?;
                    if status != 200
                        || !capabilities["scene"].is_object()
                        || !capabilities["ui"].is_object()
                        || !capabilities["requests"].is_object()
                        || !capabilities["responses"].is_object()
                    {
                        return Err("main 能力合同不匹配".to_string());
                    }
                    // 离线恢复只发送当前状态。交接事件超过投影期限后以新版本清除，
                    // 避免 gateway 重启后把历史事件当成一次新的拜访。
                    let _directory_sync = office.directory_sync.lock().await;
                    office.clear_transient_events()?;
                    let snapshots = office.directory()?;
                    let (_, remote) = office
                        .message(
                            serde_json::json!({"type":"office.read"}),
                            Duration::from_secs(2),
                        )
                        .await?;
                    let ready = remote["offices"].as_array().is_some_and(|offices| {
                        snapshots.iter().all(|snapshot| {
                            offices.iter().any(|entry| {
                                entry["id"] == snapshot["workspace"]["id"]
                                    && entry["seq"] == snapshot["seq"]
                                    && entry["runtimeReady"] == true
                            })
                        })
                    });
                    let status = if ready && (chat.is_none() || chat_registered) {
                        200
                    } else {
                        // 原版Gateway和浏览器均增量合并目录；每个办公室独立满足单请求64KiB合同。
                        let mut status = 200;
                        for snapshot in snapshots {
                            let mut message = serde_json::json!({"type":"offices.initialize","offices":[snapshot]});
                            if let Some(url) = chat {
                                message["chat"] = serde_json::json!({"url":url.as_str()});
                            }
                            let result = office.message(message, Duration::from_secs(3)).await?.0;
                            if !matches!(result, 200 | 202) {
                                return Err("main 初始化被拒绝".into());
                            }
                            status = status.max(result);
                        }
                        status
                    };
                    if !matches!(status, 200 | 202) {
                        return Err("main 初始化被拒绝".into());
                    }
                    chat_registered = true;
                    if status == 200 {
                        // 完整快照会按任务重算人物状态。对仍真实活动的回合重新提交
                        // 当前思考/工具状态；只恢复状态，不重播交接或重新启动执行。
                        let _ = tokio::time::timeout(
                            Duration::from_secs(2),
                            office.restore_presentations(),
                        )
                        .await;
                    }
                    office
                        .message(
                            serde_json::json!({"type":"office.read"}),
                            Duration::from_secs(5),
                        )
                        .await?;
                    Ok::<_, String>(())
                };
                tokio::select! { _ = stop.cancelled() => break, result = sync => {
                    if result.is_err() { tracing::warn!("main 当前状态同步未完成，将按间隔重试"); }
                }}
            }
        });
        let weak = Arc::downgrade(self);
        tokio::spawn(async move {
            loop {
                let Some(office) = weak.upgrade() else {
                    break;
                };
                office.expire_details();
                let result = tokio::select! {
                    _ = shutdown.cancelled() => break,
                    result = office.message(serde_json::json!({"type":"ui.next","timeoutMs":1000}),Duration::from_secs(3)) => result,
                };
                if let Ok((200, data)) = result {
                    if let Some(actions) = data["actions"].as_array() {
                        for action in actions.iter().take(1) {
                            let Some(id) = action["id"].as_str() else {
                                continue;
                            };
                            let Some(team) = action["teamId"].as_str() else {
                                continue;
                            };
                            let _directory_sync = if action["action"] == "office.create" {
                                Some(office.directory_sync.lock().await)
                            } else {
                                None
                            };
                            let presentation = if action["action"] == "office.create" {
                                // 创建发生在办公室/会话绑定前，不能走详情的归属查找。
                                let runtime = office
                                    .runtime
                                    .lock()
                                    .unwrap_or_else(|e| e.into_inner())
                                    .upgrade();
                                let created = match (
                                    team,
                                    action["conversationId"].as_str(),
                                    action["name"].as_str(),
                                    runtime,
                                ) {
                                    ("", Some(""), Some(name), Some(state)) => {
                                        office.create_workspace(&state.config.workspace, name).await
                                    }
                                    ("", Some(""), Some(_), None) => {
                                        Err((500, "工作空间服务不可用"))
                                    }
                                    _ => Err((400, "创建工作空间的名称或归属格式错误")),
                                };
                                match created {
                                    Ok(snapshot) => {
                                        serde_json::json!({"target":"office","snapshot":snapshot})
                                    }
                                    Err((_, message)) => {
                                        serde_json::json!({"target":"result","success":false,"message":message})
                                    }
                                }
                            } else {
                                let matching = office.action_record(team, action).is_ok();
                                let content = match action["action"].as_str() {
                                    Some(
                                        "actor.inspect" | "employee.inspect" | "dialog.action",
                                    ) => {
                                        if matching {
                                            office.prepare_detail(team, action).await;
                                            office.inspect(team, action)
                                        } else {
                                            serde_json::json!({"template":"detail-v1","title":"归属不匹配","description":"办公室不匹配或对话不存在，请返回原对话查看员工。","fields":[],"actions":[]})
                                        }
                                    }
                                    _ => {
                                        serde_json::json!({"template":"detail-v1","title":"操作说明","description":"此操作尚未开放，未启动或修改任务。","fields":[],"actions":[]})
                                    }
                                };
                                if action["action"] == "conversation.delete" {
                                    serde_json::json!({"target":"result","success":false,"message":"本轮暂不支持删除主对话，历史已保留。"})
                                } else {
                                    serde_json::json!({"target":"dialog","content":content})
                                }
                            };
                            let response = serde_json::json!({"type":"ui.respond","interactionId":id,
                                "presentation":presentation});
                            tokio::select! { _ = shutdown.cancelled() => return,
                            _ = office.message(response,Duration::from_secs(5)) => {} }
                        }
                    }
                }
                tokio::select! { _ = shutdown.cancelled() => break, _ = tokio::time::sleep(Duration::from_millis(250)) => {} }
            }
        });
    }
}
const OFFICE_VISUAL_SYSTEM_PROMPT: &str = r#"你正在 AI Office attached 模式中工作。可将职责清晰、可独立执行的任务通过 delegate_task 交给真实子 Agent，简单问答无需委派。当用户明确要求多 Agent、子 Agent、多空间协作且存在多个独立职责时，必须先调用 delegate_task；失败时说明原因，不能静默退化为单 Agent。按任务实际依赖安排协作，不套用固定领域角色或阶段模板。name 由你根据本次职责选择，优先使用清晰简短的中文功能名称；必要的 API、SQL 等技术标识可以保留。name 只用于展示，新委派建立独立真实子会话，不按名称或相似职责合并历史。主 Agent 负责整合真实结果。每轮结束发送有效最终答复，严格遵循用户指定语言、格式和详略；允许纯文本、Markdown、JSON、XML等，不强制章节、文件清单或“无”占位。只有确实需要交付文件时，写入独立文件并调用 register_artifact 登记用户应查看的真实相对路径；shell或子Agent生成的成品也可登记，不登记检索脚本、临时数据和调试文件。子Agent过程文件保留在子空间。图示和图表只是可选展示能力，按用户需要选择合适格式，不因回答涉及某个主题就额外生成图表或强制使用Mermaid。可选图表文件.chart.json支持type为bar、bar3d、line、area、pie、donut、radar，以及title、categories、series[{name,data}]字段；数值必须真实、分类与系列等长，超过预览能力可交付其他通用格式。不得编造文件、工具执行或验证结果。"#;

impl MainOffice {
    fn update(
        &self,
        session: &str,
        change: impl FnOnce(&str, &mut MainOfficeRecord) -> Vec<Value>,
    ) {
        let projection = {
            let mut guard = self.records.lock().unwrap_or_else(|e| e.into_inner());
            let Ok(records) = guard.as_mut() else {
                return;
            };
            let mut next = records.clone();
            let Some((team, record)) = next
                .iter_mut()
                .flat_map(|(team, entry)| entry.all_mut().map(move |record| (team, record)))
                .find(|(_, record)| record.session.as_deref() == Some(session))
            else {
                return;
            };
            let seq = record.snapshot["seq"].as_u64().unwrap_or(1);
            if seq >= 9_007_199_254_740_991 {
                tracing::error!("main 快照版本已耗尽");
                return;
            }
            record.snapshot["seq"] = Value::from(seq + 1);
            record.snapshot["events"] = serde_json::json!([]);
            let mut commands = change(team, record);
            for command in &commands {
                if command["type"] == "actor.presentation.set" {
                    if let (Some(actor), Some(status), Some(title)) = (
                        command["actorId"].as_str(),
                        command["status"].as_str(),
                        command["title"].as_str(),
                    ) {
                        record
                            .presentations
                            .insert(actor.into(), (status.into(), title.into()));
                    }
                }
            }
            for (actor, (status, title)) in &record.presentations {
                if !commands.iter().any(|command| {
                    command["type"] == "actor.presentation.set" && command["actorId"] == *actor
                }) {
                    commands.push(Self::command(team, actor, status, title, seq + 1));
                }
            }
            let allow_cancelled = record.snapshot["events"]
                .as_array()
                .is_some_and(|events| events.is_empty())
                && commands.iter().all(|command| command["status"] == "idle");
            let handle = self
                .sessions
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(session)
                .and_then(std::sync::Weak::upgrade);
            let cancel = handle
                .as_ref()
                .map(|handle| handle.main_cancel_token())
                .unwrap_or_default();
            if cancel.is_cancelled() && !allow_cancelled {
                return;
            }
            let projection = MainProjection {
                team: team.clone(),
                conversation: record.conversation.clone(),
                snapshot: record.snapshot.clone(),
                commands,
                expires: tokio::time::Instant::now() + Duration::from_secs(3),
                cancel,
                allow_cancelled,
            };
            if let Some(handle) = handle {
                let fact = serde_json::json!({"seq":record.snapshot["seq"],"employees":record.snapshot["employees"],"tasks":record.snapshot["tasks"]});
                if crate::runtime::record_main_fact(&handle, fact).is_err() {
                    tracing::error!("main 会话事实持久化失败，未发布人物状态");
                    return;
                }
            }
            if self.save(&next).is_err() {
                *records = next;
                tracing::error!("main 快照持久化失败，会话事实已保留，未发送新版本");
                return;
            }
            *records = next;
            projection
        };
        let _ = self.projection_tx.try_send(projection);
    }

    fn command(team: &str, actor: &str, state: &str, title: &str, revision: u64) -> Value {
        serde_json::json!({"protocolVersion":"2.0","sceneId":team,
            "commandId":format!("cmd-{}",uuid::Uuid::new_v4().simple()),"type":"actor.presentation.set",
            "actorId":actor,"status":state,"title":title,"sourceRevision":revision})
    }

    fn begin(&self, session: &str) {
        self.update(session,|team,record| {
            let snapshot = &mut record.snapshot;
            let employees = snapshot["employees"].as_array_mut().expect("owned snapshot");
            if let Some(lead) = employees.iter_mut().find(|employee| employee["id"] == session) {
                lead["online"] = Value::Bool(true);
            } else {
                employees.push(serde_json::json!({"id":session,"sceneActorId":"office-lead","name":"王明","role":"主 Agent","online":true}));
            }
            vec![Self::command(team,"office-lead","thinking","处理当前请求",snapshot["seq"].as_u64().unwrap_or(1))]
        });
    }

    fn activity(&self, session: &str, owner: &str, event: &AgentEvent) {
        let state = match event {
            AgentEvent::ToolExecutionStart { .. } => "working",
            AgentEvent::ToolResult { .. } => "thinking",
            _ => return,
        };
        self.update(session, |team, record| {
            let actor = record.snapshot["employees"]
                .as_array()
                .and_then(|employees| employees.iter().find(|employee| employee["id"] == owner))
                .and_then(|employee| employee["sceneActorId"].as_str())
                .unwrap_or("");
            if actor.is_empty() {
                return Vec::new();
            }
            vec![Self::command(
                team,
                actor,
                state,
                if state == "working" {
                    "执行工具"
                } else {
                    "正在思考"
                },
                record.snapshot["seq"].as_u64().unwrap_or(1),
            )]
        });
    }

    fn finish(&self, session: &str) {
        self.update(session, |team, record| {
            let revision = record.snapshot["seq"].as_u64().unwrap_or(1);
            record.snapshot["employees"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|employee| {
                    let actor = employee["sceneActorId"].as_str()?;
                    (!actor.is_empty()).then(|| Self::command(team, actor, "idle", "", revision))
                })
                .collect()
        });
    }

    fn cancel(&self, session: &str) {
        self.update(session, |team, record| {
            record.child_tasks.clear();
            for task in record.snapshot["tasks"]
                .as_array_mut()
                .expect("owned tasks")
            {
                if task["status"] == "running" {
                    task["status"] = Value::from("failed");
                    task["summary"] = Value::from("执行已取消；已发生的工具副作用不自动回滚");
                    task["updatedAt"] = Value::from(chrono::Utc::now().to_rfc3339());
                }
            }
            record.snapshot["employees"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|employee| {
                    let actor = employee["sceneActorId"].as_str()?;
                    (!actor.is_empty()).then(|| {
                        Self::command(
                            team,
                            actor,
                            "idle",
                            "",
                            record.snapshot["seq"].as_u64().unwrap_or(1),
                        )
                    })
                })
                .collect()
        });
    }

    fn child_begin(&self, session: &str, child: &str, _name: &str) {
        self.update(session,|team,record| {
            let employees = record.snapshot["employees"].as_array_mut().expect("owned employees");
            if !employees.iter().any(|employee| employee["id"] == child) {
                // main 子身份必须在 runtime 执行前通过容量预留和可靠登记。
                return Vec::new();
            } else if let Some(employee) = employees.iter_mut().find(|employee| employee["id"] == child) {
                employee["online"] = Value::Bool(true);
            }
            let bound_task = record.snapshot["tasks"].as_array().into_iter().flatten().find(|task| task.get("goalId").is_some() && task["assigneeId"] == child).and_then(|task|task["id"].as_str()).map(str::to_owned);
            let task = bound_task.clone().unwrap_or_else(|| format!("exec-{}",uuid::Uuid::new_v4().simple()));
            record.child_tasks.insert(child.into(),task.clone());
            let tasks = record.snapshot["tasks"].as_array_mut().expect("owned tasks");
            if tasks.len() >= 1000 { tasks.retain(|task| task["status"] == "running"); }
            let now = chrono::Utc::now().to_rfc3339();
            if bound_task.is_none() {
                tasks.push(serde_json::json!({"id":task,"title":"执行子任务","assigneeId":child,"status":"running","progress":-1,"updatedAt":now,"summary":"真实子 Agent 执行"}));
            }
            record.snapshot["events"] = serde_json::json!([{"id":format!("assigned-{task}"),"kind":"task.assigned",
                "summary":"主 Agent 向子 Agent 交接任务","occurredAt":now,"taskId":task,"employeeId":child,"fromEmployeeId":session}]);
            record.snapshot["employees"].as_array().into_iter().flatten()
                .find(|employee|employee["id"] == child)
                .and_then(|employee|employee["sceneActorId"].as_str()).filter(|actor|!actor.is_empty())
                .map(|actor|vec![Self::command(team,actor,"thinking","处理子任务",record.snapshot["seq"].as_u64().unwrap_or(1))])
                .unwrap_or_default()
        });
    }

    fn child_end(&self, session: &str, child: &str, completed: bool) {
        self.update(session,|team,record| {
            let Some(task_id) = record.child_tasks.remove(child) else { return Vec::new(); };
            if let Some(task) = record.snapshot["tasks"].as_array_mut().expect("owned tasks").iter_mut().find(|task| task["id"] == task_id && task.get("goalId").is_none()) {
                task["status"] = Value::from(if completed {"completed"}else{"failed"});
                task["updatedAt"] = Value::from(chrono::Utc::now().to_rfc3339());
                task["summary"] = Value::from(if completed {"真实执行已返回结果"}else{"执行未成功完成"});
            }
            let Some(actor) = record.snapshot["employees"].as_array().expect("owned employees").iter()
                .find(|employee| employee["id"] == child).and_then(|employee| employee["sceneActorId"].as_str()) else { return Vec::new(); };
            if actor.is_empty() { return Vec::new(); }
            let mut commands = vec![Self::command(team,actor,"idle","",record.snapshot["seq"].as_u64().unwrap_or(1))];
            if completed {
                commands.push(serde_json::json!({"protocolVersion":"2.0","sceneId":team,"commandId":format!("return-{task_id}"),
                    "type":"activity.start","capability":"office.visit","participants":[{"entityId":actor,"role":"visitor"},{"entityId":"office-lead","role":"host"}],
                    "params":{"stops":[{"hostId":"office-lead","message":"子任务已完成，请查收结果。"}]}}));
            }
            commands
        });
    }
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
/// 功能：表示发送给 AI Office Gateway 的单个动作。
/// 字段/变体语义：`set_state` 更新角色状态；`desk_visit` 表示两个角色之间的工作交接。
/// 数据来源：`OfficeDispatcher` 根据 AgentEvent 和回合生命周期转换。
/// 数据去向：AI Office `/actions` 接口；序列化名称必须保持网关兼容。
/// 约束：角色编号使用 Office 名册编号；`task` 为空时不输出该字段。
/// 稳定性：跨项目公共动作契约，字段变化必须同步 Schema 和 CHG。
/// 关联：`API-OFFICE-ACTION-001`、`DAT-OFFICE-ACTION-001`。
pub enum OfficeAction {
    #[serde(rename = "set_state")]
    /// 更新一个 Office 角色的工作状态和可选任务描述。
    SetState {
        #[serde(rename = "workspaceId")]
        /// 当前会话的主空间编号；使用 session id 隔离并行对话的动画状态。
        workspace_id: String,
        #[serde(rename = "workspaceName")]
        /// 主空间显示名称。
        workspace_name: String,
        #[serde(rename = "sessionId")]
        /// 动作所属会话编号，供 Gateway 和前端按对话筛选空间。
        session_id: String,
        #[serde(rename = "parentWorkspaceId", skip_serializing_if = "Option::is_none")]
        /// 子 Agent 空间所属的父空间；主空间省略。
        parent_workspace_id: Option<String>,
        #[serde(rename = "rosterNo")]
        /// Office 名册中的角色编号，序列化字段名为 `rosterNo`。
        roster_no: u8,
        /// 角色状态，例如 `thinking`、`working` 或 `idle`。
        state: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        /// 当前任务的脱敏短描述；没有任务时省略字段。
        task: Option<String>,
    },
    #[serde(rename = "desk_visit")]
    /// 表示一个回合内的角色拜访动作。
    DeskVisit {
        #[serde(rename = "workspaceId")]
        /// 当前会话的主空间编号。
        workspace_id: String,
        #[serde(rename = "workspaceName")]
        /// 主空间显示名称。
        workspace_name: String,
        #[serde(rename = "sessionId")]
        /// 动作所属会话编号。
        session_id: String,
        #[serde(rename = "parentWorkspaceId", skip_serializing_if = "Option::is_none")]
        /// 子 Agent 空间所属的父空间；主空间省略。
        parent_workspace_id: Option<String>,
        /// 发起拜访的 Office 名册编号；首站为 1 号角色，后续为上一工具角色。
        visitor: u8,
        /// 被拜访的目标角色名册编号。
        host: u8,
        /// 触发拜访的工具名或简短消息，不承载凭据和完整用户输入。
        message: String,
    },
    #[serde(rename = "workspace_handoff")]
    /// 表示两个真实 Agent 空间之间的任务交接。
    WorkspaceHandoff {
        #[serde(rename = "fromWorkspaceId")]
        /// 交接发起空间的稳定编号。
        from_workspace_id: String,
        #[serde(rename = "toWorkspaceId")]
        /// 交接目标空间的稳定编号。
        to_workspace_id: String,
        #[serde(rename = "fromWorkspaceName")]
        /// 交接发起空间的显示名称。
        from_workspace_name: String,
        #[serde(rename = "toWorkspaceName")]
        /// 交接目标空间的显示名称。
        to_workspace_name: String,
        #[serde(rename = "sessionId")]
        /// 动作所属的父会话编号。
        session_id: String,
        /// going、returning 或 complete。
        phase: String,
        #[serde(rename = "rosterNo", skip_serializing_if = "Option::is_none")]
        /// 参与交接的可选 Office 名册编号。
        roster_no: Option<u8>,
        #[serde(skip_serializing_if = "Option::is_none")]
        /// 交接说明，不包含凭据或完整工具输入。
        message: Option<String>,
    },
    #[serde(rename = "register_artifact")]
    /// 把已验证的工作区文件登记到所属空间，主空间登记由显式交付工具触发。
    RegisterArtifact {
        #[serde(rename = "sessionId")]
        /// 动作所属的父会话编号。
        session_id: String,
        #[serde(rename = "workspaceId")]
        /// 动作所属空间的稳定编号。
        workspace_id: String,
        /// 相对于 OpenPaw 工作区的路径；不包含文件正文或工具命令。
        path: String,
    },
    #[serde(rename = "workspace_output")]
    /// 把子 Agent 可展示的模型正文增量或最终文本投影到所属空间。
    WorkspaceOutput {
        #[serde(rename = "sessionId")]
        /// 动作所属的父会话编号。
        session_id: String,
        #[serde(rename = "workspaceId")]
        /// 动作所属空间的稳定编号。
        workspace_id: String,
        #[serde(rename = "workspaceName")]
        /// 动作所属空间的显示名称。
        workspace_name: String,
        #[serde(rename = "parentWorkspaceId", skip_serializing_if = "Option::is_none")]
        /// 子空间所属的父空间编号；主空间省略。
        parent_workspace_id: Option<String>,
        /// `model_delta` 表示增量块，`model_final` 表示完整最终正文。
        kind: String,
        /// 只包含 assistant 可展示正文，不包含 reasoning 或工具输入输出。
        text: String,
    },
}
impl OfficeAction {
    fn session_id(&self) -> &str {
        match self {
            Self::SetState { session_id, .. }
            | Self::DeskVisit { session_id, .. }
            | Self::WorkspaceHandoff { session_id, .. }
            | Self::RegisterArtifact { session_id, .. }
            | Self::WorkspaceOutput { session_id, .. } => session_id,
        }
    }
}

#[derive(Debug, Clone)]
struct OfficeScope {
    workspace_id: String,
    workspace_name: String,
    session_id: String,
    parent_workspace_id: Option<String>,
}

impl OfficeScope {
    fn main(session_id: &str) -> Self {
        Self {
            workspace_id: session_id.to_string(),
            workspace_name: "我的空间".into(),
            session_id: session_id.to_string(),
            parent_workspace_id: None,
        }
    }
}

#[derive(Clone)]
/// 功能：维护 Office 动作发送队列和跨 session 的角色占用关系。
/// 数据来源：Host 回合开始、AgentEvent 工具事件和回合结束通知。
/// 数据去向：Gateway HTTP POST；角色占用只用于决定何时发送 `idle`。
/// 约束：发送采用有界非阻塞队列；网络超时或队列满不得阻塞 AgentLoop。
/// 并发与取消：队列由 Tokio worker 顺序消费；每个 owner 独立维护工作角色集合。
/// 权限：动作内容必须脱敏，不能绕过 Host 鉴权或写入敏感数据。
/// 稳定性：Office 投影 L3 边界，修改必须建立 CHG。
/// 关联：`API-OFFICE-ACTION-001`、`REQ-B-OFFICE-003`。
pub struct OfficeDispatcher {
    tx: mpsc::Sender<OfficeAction>,
    owners: Arc<Mutex<HashMap<u8, HashSet<String>>>>,
    scopes: Arc<Mutex<HashMap<String, OfficeScope>>>,
    removed_sessions: Arc<Mutex<HashSet<String>>>,
    enabled: bool,
    main: Arc<Mutex<std::sync::Weak<MainOffice>>>,
}

#[derive(Debug, Clone)]
/// 功能：记录一个活动工具在 Office 中对应的角色和短完成态。
/// 约束：不保存路径、扩展名、完整 URL、正文或命令；只保存限长的内容主题。
pub(crate) struct ActiveToolProjection {
    role: u8,
    completed_task: String,
    failed_task: String,
}

/// 子 Agent 单回合的办公室投影状态，仅由 engine-host 委派执行器持有。
pub(crate) struct SubagentOfficeTurn {
    working: HashSet<u8>,
    last_tool_role: Option<u8>,
    tool_failed: bool,
    artifact_count: usize,
    active_tools: HashMap<String, ActiveToolProjection>,
    workspace: PathBuf,
    pending_artifacts: HashMap<String, String>,
    model_output: String,
    model_pending: String,
    model_output_chars: usize,
}

impl OfficeDispatcher {
    /// 功能：创建只记录动作的测试分发器。
    /// 参数：无。
    /// 返回：分发器与动作接收端。
    /// 错误：无。
    /// 状态：初始化容量为 128 的空队列。
    /// 并发与取消：接收端关闭后停止记录，不启动网络任务。
    /// 权限：不执行网络请求或授权操作。
    /// 关联：Office 投影与委派回归测试。
    #[cfg(test)]
    pub(crate) fn recording() -> (Self, mpsc::Receiver<OfficeAction>) {
        let (tx, rx) = mpsc::channel(128);
        (
            Self {
                tx,
                owners: Arc::new(Mutex::new(HashMap::new())),
                scopes: Arc::new(Mutex::new(HashMap::new())),
                removed_sessions: Arc::new(Mutex::new(HashSet::new())),
                enabled: false,
                main: Arc::new(Mutex::new(std::sync::Weak::new())),
            },
            rx,
        )
    }

    /// 功能：创建 Office 动作分发器并启动可选的 Gateway 后台发送 worker。
    /// 参数：`url` 为 Gateway `/actions` 地址；传入 `None` 时保留队列但丢弃网络投影。
    /// 返回：可在线程间共享的分发器。
    /// 错误：不会因 worker 启动或 HTTP 请求失败返回错误；失败只记录警告并丢弃动作。
    /// 状态：初始没有角色占用；队列容量为 128 个动作。
    /// 并发与取消：`send` 使用非阻塞入队；每个 HTTP POST 最多等待 500 毫秒，不影响回合。
    /// 权限：URL 来自受控配置；动作请求不得携带 Basic Authentication 凭据。
    /// 关联：`REQ-B-OFFICE-008`、`CHG-2026-0003`。
    pub fn new(url: Option<String>) -> Self {
        let enabled = url.is_some();
        let (tx, mut rx) = mpsc::channel::<OfficeAction>(128);
        let removed_sessions = Arc::new(Mutex::new(HashSet::new()));
        if let Some(url) = url {
            let worker_removed_sessions = removed_sessions.clone();
            let worker = async move {
                let client = Client::new();
                while let Some(action) = rx.recv().await {
                    if worker_removed_sessions
                        .lock()
                        .unwrap_or_else(|error| error.into_inner())
                        .contains(action.session_id())
                    {
                        continue;
                    }
                    let result = tokio::time::timeout(
                        Duration::from_millis(500),
                        client.post(&url).json(&action).send(),
                    )
                    .await;
                    match result {
                        Ok(Ok(response)) if response.status().is_success() => {}
                        Ok(Ok(response)) => {
                            tracing::warn!(status = %response.status(), "办公室动作被网关拒绝")
                        }
                        Ok(Err(error)) => {
                            tracing::warn!(%error, "办公室动作请求失败，已忽略本次动作")
                        }
                        Err(_) => tracing::warn!("办公室动作请求超时（500 毫秒），已忽略本次动作"),
                    }
                }
            };
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                handle.spawn(worker);
            }
        } else if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move { while rx.recv().await.is_some() {} });
        }
        Self {
            tx,
            owners: Arc::new(Mutex::new(HashMap::new())),
            scopes: Arc::new(Mutex::new(HashMap::new())),
            removed_sessions,
            enabled,
            main: Arc::new(Mutex::new(std::sync::Weak::new())),
        }
    }

    /// 功能：查询显式 Office 开关。
    /// 参数：无。
    /// 返回：是否配置 Office URL。
    /// 错误：无。
    /// 状态：只读。
    /// 并发与取消：同步无锁。
    /// 权限：不按请求内容自动启用。
    /// 关联：Q8。
    pub(crate) fn is_enabled(&self) -> bool {
        self.enabled || self.main().is_some_and(|main| main.url.is_some())
    }

    /// 功能：关联 main 投影，不改变 feature 出站协议。
    /// 参数：main 为同根持久化目录。
    /// 返回：无。
    /// 错误：无。
    /// 状态：只保存弱引用，避免后台任务引用环。
    /// 并发与取消：同步短锁，关闭随 AppState 释放。
    /// 权限：绑定仅由 Host 初始化执行。
    /// 关联：CHG-2026-0033。
    pub(crate) fn attach_main(&self, main: &Arc<MainOffice>) {
        *self.main.lock().unwrap_or_else(|e| e.into_inner()) = Arc::downgrade(main);
    }

    /// 功能：投影 main 取消状态并使原令牌对应的迟到工作动作失效。
    /// 参数：session 为取消的父会话。
    /// 返回：无。
    /// 错误：投影失败不改变取消结果。
    /// 状态：保留已发生事实，清理活动子任务展示。
    /// 并发与取消：同步有界入队，不等待动画。
    /// 权限：不取消其他办公室，不回滚工具副作用。
    /// 关联：T1-09。
    pub(crate) fn cancel_main(&self, session: &str) {
        if let Some(main) = self.main() {
            main.cancel(session);
        }
    }

    /// 功能：把已提交 Goal 事实投影到当前聊天任务列表与白板。
    /// 参数：session 为所属主会话，goal 为完整事实快照。
    /// 返回：无。
    /// 错误：展示失败只记录原投影日志，不改变 Goal 或回合。
    /// 状态：只写派生 Office 快照。
    /// 并发与取消：先释放 Goal 锁再调用；沿用原有异步出站队列。
    /// 权限：不执行或验收任务。
    /// 关联：CHG-2026-0050。
    pub(crate) fn project_goal(&self, session: &str, goal: Value) {
        if let Some(main) = self.main() {
            main.update(session, |_, record| {
                apply_goal_projection(&mut record.snapshot, &goal);
                Vec::new()
            });
        }
    }

    fn main(&self) -> Option<Arc<MainOffice>> {
        self.main
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .upgrade()
    }

    /// 功能：取得 Office 模式的可视化指导。
    /// 参数：无。
    /// 返回：启用时返回指导文本。
    /// 错误：无。
    /// 状态：不修改模型消息或持久历史。
    /// 并发与取消：同步无锁。
    /// 权限：不改变独立 CLI 或关闭 Office 的 Host。
    /// 关联：Q8。
    pub(crate) fn system_prompt(&self) -> Option<String> {
        self.is_enabled()
            .then(|| OFFICE_VISUAL_SYSTEM_PROMPT.to_string())
    }

    /// 功能：登记主空间显式交付文件。
    /// 参数：会话身份和已校验工作区相对路径。
    /// 返回：无。
    /// 错误：投影失败不改变工具结果。
    /// 状态：写入既有有界动作队列。
    /// 并发与取消：同步尽力发送；不等待 Office 网络。
    /// 权限：路径校验由调用工具负责，不发布文件正文。
    /// 关联：register_artifact 与 Q8。
    pub(crate) fn register_artifact(&self, session_id: &str, path: String) {
        self.send(OfficeAction::RegisterArtifact {
            session_id: session_id.to_string(),
            workspace_id: session_id.to_string(),
            path,
        });
    }

    fn send(&self, action: OfficeAction) {
        if self
            .removed_sessions
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .contains(action.session_id())
        {
            return;
        }
        if let Err(error) = self.tx.try_send(action) {
            tracing::warn!(%error, "办公室动作队列已满或已关闭，已忽略本次动作");
        }
    }

    fn scope(&self, owner: &str) -> OfficeScope {
        self.scopes
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(owner)
            .cloned()
            .unwrap_or_else(|| OfficeScope::main(owner))
    }

    fn register_scope(&self, owner: &str, scope: OfficeScope) {
        self.scopes
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(owner.to_string(), scope);
    }

    fn send_state(&self, owner: &str, role: u8, state: &str, task: Option<String>) {
        let scope = self.scope(owner);
        self.send(OfficeAction::SetState {
            workspace_id: scope.workspace_id,
            workspace_name: scope.workspace_name,
            session_id: scope.session_id,
            parent_workspace_id: scope.parent_workspace_id,
            roster_no: role,
            state: state.into(),
            task,
        });
    }

    fn send_visit(&self, owner: &str, visitor: u8, host: u8, message: &str) {
        let scope = self.scope(owner);
        self.send(OfficeAction::DeskVisit {
            workspace_id: scope.workspace_id,
            workspace_name: scope.workspace_name,
            session_id: scope.session_id,
            parent_workspace_id: scope.parent_workspace_id,
            visitor,
            host,
            message: message.into(),
        });
    }

    fn set_working(
        &self,
        owner: &str,
        role: u8,
        state: &str,
        task: Option<String>,
        working: &mut HashSet<u8>,
    ) {
        self.owners
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .entry(role)
            .or_default()
            .insert(owner.to_string());
        self.send_state(owner, role, state, task);
        working.insert(role);
    }

    fn release(&self, owner: &str, role: u8) {
        let released = {
            let mut owners = self
                .owners
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let Some(role_owners) = owners.get_mut(&role) else {
                return;
            };
            let released = role_owners.remove(owner);
            let empty = role_owners.is_empty();
            if empty {
                owners.remove(&role);
            }
            released
        };
        if released {
            self.send_state(owner, role, "idle", None);
        }
    }

    /// 功能：清理指定会话持有的角色及 main 当前人物状态。
    /// 参数：owner 为原会话稳定身份。
    /// 返回：无。
    /// 错误：展示投影或持久化失败只记录安全日志，不改变回合终态。
    /// 状态：释放 feature 角色占用，main 人物回到 idle，不声明子任务成功。
    /// 并发与取消：同步短锁；main 事实由原 writer 串行提交，不等待网络。
    /// 权限：只清理原会话所属人员，不清理其他办公室。
    /// 关联：CHG-2026-0033、T1-09、T1-10。
    pub fn clear_owner(&self, owner: &str) {
        if let Some(main) = self.main() {
            main.finish(owner);
        }
        let released_roles = {
            let mut owners = self
                .owners
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let mut released_roles = Vec::new();
            let mut empty_roles = Vec::new();
            for (role, role_owners) in owners.iter_mut() {
                if role_owners.remove(owner) {
                    released_roles.push(*role);
                }
                if role_owners.is_empty() {
                    empty_roles.push(*role);
                }
            }
            for role in empty_roles {
                owners.remove(&role);
            }
            released_roles
        };
        for role in released_roles {
            self.send_state(owner, role, "idle", None);
        }
    }

    /// 功能：释放已删除会话的 Office 状态。
    /// 参数：会话或空间身份。
    /// 返回：无。
    /// 错误：无。
    /// 状态：清理 owner 与 scope 并阻止迟到动作。
    /// 并发与取消：同步内部锁；不等待网络。
    /// 权限：不删除用户文件或会话历史。
    /// 关联：runtime 删除会话。
    pub(crate) fn remove_scope(&self, owner: &str) {
        self.removed_sessions
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(owner.to_string());
        self.clear_owner(owner);
        self.scopes
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(owner);
    }

    /// 功能：开始一个 session 回合，清理该 owner 上一回合的角色并让 1 号角色进入思考状态。
    /// 参数：`owner` 为 session 所有者编号；`working` 为调用方保存的本回合角色集合。
    /// 返回：无；`working` 会被更新为当前占用角色。
    /// 错误：无；动作入队失败只记录警告。
    /// 状态：释放旧角色后发送 `set_state(thinking)`。
    /// 并发与取消：通过 owner 计数避免清理其他 session 的同一角色。
    /// 权限：不改变工具权限或审批状态。
    /// 关联：`REQ-B-OFFICE-003`、`EVT-HOST-SESSION-001`。
    pub fn turn_started(&self, owner: &str, working: &mut HashSet<u8>) {
        if let Some(main) = self.main() {
            main.begin(owner);
        }
        if !self
            .scopes
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .contains_key(owner)
        {
            self.register_scope(owner, OfficeScope::main(owner));
        }
        for role in working.drain() {
            self.release(owner, role);
        }
        self.set_working(owner, 1, "thinking", Some("处理当前请求".into()), working);
    }

    /// 功能：观察单个 AgentEvent，并按工具类别更新 Office 角色状态。
    /// 参数：`owner` 为 session 所有者；`event` 为引擎事件；`working` 为角色占用集合；
    /// `last_tool_role` 保存当前工具链末端；`tool_failed` 保存末端工具结果；
    /// `active_tools` 记录仍在执行的工具调用及其 Office 投影。
    /// 返回：无；未知工具不产生动作，审批等待只更新 6 号审核角色。
    /// 错误：无；投影发送失败由队列记录并忽略。
    /// 状态：工具角色变化时由上一角色发起交接；链尾在工具完成后保持工作，直到后续交接或回合结束。
    /// 并发与取消：调用顺序即事件顺序；取消由回合结束逻辑统一清理。
    /// 权限：只使用引擎提供的白名单参数生成限长摘要，不读取或发送正文和命令。
    /// 关联：`EVT-HOST-TOOL-001`、`REQ-B-OFFICE-003`。
    pub(crate) fn observe(
        &self,
        owner: &str,
        event: &AgentEvent,
        working: &mut HashSet<u8>,
        last_tool_role: &mut Option<u8>,
        tool_failed: &mut bool,
        active_tools: &mut HashMap<String, ActiveToolProjection>,
    ) {
        if let Some(main) = self.main() {
            main.activity(&self.scope(owner).session_id, owner, event);
        }
        match event {
            AgentEvent::ToolExecutionStart {
                tool_use_id,
                name,
                input,
            } => {
                if let Some(role) = tool_role(name) {
                    let task = tool_activity_summary(name, input);
                    let completed_task = tool_activity_completion(name, input);
                    active_tools.insert(
                        tool_use_id.clone(),
                        ActiveToolProjection {
                            role,
                            completed_task,
                            failed_task: format!("{task}；失败，正在调整"),
                        },
                    );
                    self.send_tool_working(
                        owner,
                        role,
                        &task,
                        working,
                        last_tool_role,
                        active_tools,
                    );
                }
            }
            AgentEvent::ToolResult {
                tool_use_id,
                is_error,
                ..
            } => {
                let projection = active_tools.remove(tool_use_id);
                if let Some(projection) = projection {
                    let role = projection.role;
                    let role_still_active = active_tools.values().any(|active| active.role == role);
                    if !role_still_active {
                        if Some(role) == *last_tool_role {
                            *tool_failed = *is_error;
                            self.set_working(
                                owner,
                                role,
                                "working",
                                Some(if *is_error {
                                    projection.failed_task
                                } else {
                                    projection.completed_task
                                }),
                                working,
                            );
                        } else if working.remove(&role) {
                            self.release(owner, role);
                        }
                    }
                }
            }
            AgentEvent::ClassifierStatus {
                status: ClassifierStatus::SuggestConfirm { .. },
                ..
            } => {
                self.set_working(owner, 6, "working", Some("等待审批".into()), working);
            }
            _ => {}
        }
    }

    fn send_tool_working(
        &self,
        owner: &str,
        role: u8,
        task: &str,
        working: &mut HashSet<u8>,
        last_tool_role: &mut Option<u8>,
        active_tools: &HashMap<String, ActiveToolProjection>,
    ) {
        let previous = last_tool_role.unwrap_or(1);
        if previous != role {
            self.send_visit(owner, previous, role, handoff_message(role));
        }
        self.set_working(owner, role, "working", Some(task.to_string()), working);
        *last_tool_role = Some(role);

        if previous != 1
            && previous != role
            && !active_tools.values().any(|active| active.role == previous)
            && working.remove(&previous)
        {
            self.release(owner, previous);
        }
    }

    /// 功能：结束一个 session 回合并释放本 owner 持有的全部 Office 角色。
    /// 参数：`owner` 为 session 所有者；`working` 为本回合角色集合；最后角色用于成果交付；
    /// `completed` 指示回合是否成功完成。
    /// 返回：无；成功的工具回合先由最后角色向 1 号角色交付，再释放全部角色。
    /// 错误：无；Office 网络失败不影响已返回的引擎结果。
    /// 状态：逐一释放旧角色；只有最后一个 owner 释放角色时才发送 `idle`。
    /// 并发与取消：按 owner 释放，不能把其他 session 仍占用的角色置为空闲。
    /// 权限：整理消息不得包含敏感凭据或完整对话正文。
    /// 关联：`REQ-B-OFFICE-003`、`REQ-B-OFFICE-008`。
    pub fn turn_ended(
        &self,
        owner: &str,
        working: &mut HashSet<u8>,
        last_tool_role: Option<u8>,
        artifact_count: usize,
        completed: bool,
    ) {
        if completed {
            if artifact_count > 0 {
                let previous = last_tool_role.unwrap_or(1);
                if previous != 5 {
                    self.send_visit(owner, previous, 5, "请整理产物");
                }
                self.set_working(
                    owner,
                    5,
                    "working",
                    Some(format!("整理 {artifact_count} 项产物")),
                    working,
                );
                self.send_visit(owner, 5, 1, "产物已整理，请查收");
            } else if let Some(role) = last_tool_role.filter(|role| *role != 1) {
                self.send_visit(owner, role, 1, "任务已完成，请查收");
            }
        }
        for role in working.drain() {
            self.release(owner, role);
        }
    }

    /// 功能：建立子空间并投影任务交接。
    /// 参数：父会话、子空间身份及工作区。
    /// 返回：本次子任务的投影状态。
    /// 错误：队列失败不影响子任务。
    /// 状态：登记 scope 并开始交接动画。
    /// 并发与取消：同步入队；任务取消由 guard 清理。
    /// 权限：只投影身份，不发送任务正文或凭据。
    /// 关联：Q3、Office 子空间。
    pub(crate) fn subagent_started(
        &self,
        parent_session_id: &str,
        workspace_id: &str,
        workspace_name: &str,
        workspace: &Path,
    ) -> SubagentOfficeTurn {
        if let Some(main) = self.main() {
            main.child_begin(parent_session_id, workspace_id, workspace_name);
        }
        self.register_scope(
            workspace_id,
            OfficeScope {
                workspace_id: workspace_id.to_string(),
                workspace_name: workspace_name.to_string(),
                session_id: parent_session_id.to_string(),
                parent_workspace_id: Some(parent_session_id.to_string()),
            },
        );
        self.send(OfficeAction::WorkspaceHandoff {
            from_workspace_id: parent_session_id.to_string(),
            to_workspace_id: workspace_id.to_string(),
            from_workspace_name: "我的空间".into(),
            to_workspace_name: workspace_name.to_string(),
            session_id: parent_session_id.to_string(),
            phase: "going".into(),
            roster_no: Some(5),
            message: Some("正在分配子任务".into()),
        });
        let mut turn = SubagentOfficeTurn {
            working: HashSet::new(),
            last_tool_role: None,
            tool_failed: false,
            artifact_count: 0,
            active_tools: HashMap::new(),
            workspace: workspace.to_path_buf(),
            pending_artifacts: HashMap::new(),
            model_output: String::new(),
            model_pending: String::new(),
            model_output_chars: 0,
        };
        self.turn_started(workspace_id, &mut turn.working);
        turn
    }

    /// 功能：投影子任务引擎活动。
    /// 参数：父子身份、当前投影状态和引擎事件。
    /// 返回：无。
    /// 错误：投影失败不改变模型结果。
    /// 状态：更新角色、文本及实际成功产物状态。
    /// 并发与取消：同步内部状态；不等待网络。
    /// 权限：不投影 reasoning 或媒体原件。
    /// 关联：Q3、SubagentOfficeGuard。
    pub(crate) fn observe_subagent(
        &self,
        workspace_id: &str,
        event: &AgentEvent,
        turn: &mut SubagentOfficeTurn,
    ) {
        match event {
            AgentEvent::TextDelta { text } => {
                let remaining = MAX_SUBAGENT_OUTPUT_CHARS.saturating_sub(turn.model_output_chars);
                if remaining > 0 {
                    let accepted = text.chars().take(remaining).collect::<String>();
                    turn.model_output_chars += accepted.chars().count();
                    turn.model_output.push_str(&accepted);
                    turn.model_pending.push_str(&accepted);
                    if turn.model_pending.chars().count() >= SUBAGENT_OUTPUT_CHUNK_CHARS {
                        let scope = self.scope(workspace_id);
                        self.send(OfficeAction::WorkspaceOutput {
                            session_id: scope.session_id,
                            workspace_id: scope.workspace_id,
                            workspace_name: scope.workspace_name,
                            parent_workspace_id: scope.parent_workspace_id,
                            kind: "model_delta".into(),
                            text: std::mem::take(&mut turn.model_pending),
                        });
                    }
                }
            }
            AgentEvent::ToolExecutionStart {
                tool_use_id,
                name,
                input,
            } if matches!(name.as_str(), "write_file" | "edit_file" | "notebook_edit") => {
                if let Some(path) = input
                    .get("path")
                    .and_then(Value::as_str)
                    .and_then(|path| workspace_relative_path(&turn.workspace, path))
                {
                    turn.pending_artifacts.insert(tool_use_id.clone(), path);
                }
            }
            AgentEvent::ToolResult {
                tool_use_id,
                is_error,
                ..
            } => {
                let artifact = turn.pending_artifacts.remove(tool_use_id);
                if !*is_error {
                    if let Some(path) = artifact {
                        let scope = self.scope(workspace_id);
                        self.send(OfficeAction::RegisterArtifact {
                            session_id: scope.session_id,
                            workspace_id: scope.workspace_id,
                            path,
                        });
                        turn.artifact_count = turn.artifact_count.saturating_add(1);
                    }
                }
            }
            _ => {}
        }
        self.observe(
            workspace_id,
            event,
            &mut turn.working,
            &mut turn.last_tool_role,
            &mut turn.tool_failed,
            &mut turn.active_tools,
        );
    }

    /// 功能：结束一次子任务动画。
    /// 参数：父子身份、本次投影状态和是否完成。
    /// 返回：无。
    /// 错误：队列失败不改变结果。
    /// 状态：交接结果并释放角色占用。
    /// 并发与取消：同步入队；取消或 drop 按未完成处理。
    /// 权限：不伪造工具产物或实际执行成功。
    /// 关联：SubagentOfficeGuard、Q3。
    pub(crate) fn subagent_ended(
        &self,
        parent_session_id: &str,
        workspace_id: &str,
        workspace_name: &str,
        mut turn: SubagentOfficeTurn,
        completed: bool,
    ) {
        if let Some(main) = self.main() {
            main.child_end(
                parent_session_id,
                workspace_id,
                completed && !turn.tool_failed,
            );
        }
        if !turn.model_output.is_empty() {
            let scope = self.scope(workspace_id);
            self.send(OfficeAction::WorkspaceOutput {
                session_id: scope.session_id,
                workspace_id: scope.workspace_id,
                workspace_name: scope.workspace_name,
                parent_workspace_id: scope.parent_workspace_id,
                kind: "model_final".into(),
                text: turn.model_output.clone(),
            });
        }
        self.turn_ended(
            workspace_id,
            &mut turn.working,
            turn.last_tool_role,
            turn.artifact_count,
            completed && !turn.tool_failed,
        );
        self.send(OfficeAction::WorkspaceHandoff {
            from_workspace_id: workspace_id.to_string(),
            to_workspace_id: parent_session_id.to_string(),
            from_workspace_name: workspace_name.to_string(),
            to_workspace_name: "我的空间".into(),
            session_id: parent_session_id.to_string(),
            phase: "returning".into(),
            roster_no: Some(5),
            message: Some(if completed {
                "子任务已完成，正在交回结果".into()
            } else {
                "子任务已结束，正在返回状态".into()
            }),
        });
        self.scopes
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(workspace_id);
        let dispatcher = self.clone();
        let parent_session_id = parent_session_id.to_string();
        let workspace_id = workspace_id.to_string();
        let workspace_name = workspace_name.to_string();
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                tokio::time::sleep(Duration::from_millis(900)).await;
                dispatcher.send(OfficeAction::WorkspaceHandoff {
                    from_workspace_id: workspace_id,
                    to_workspace_id: parent_session_id.clone(),
                    from_workspace_name: workspace_name,
                    to_workspace_name: "我的空间".into(),
                    session_id: parent_session_id,
                    phase: "complete".into(),
                    roster_no: Some(5),
                    message: None,
                });
            });
        }
    }
}

fn workspace_relative_path(workspace: &Path, value: &str) -> Option<String> {
    let workspace = normalized_path(workspace)?;
    let input = Path::new(value);
    let absolute = if input.is_absolute() {
        normalized_path(input)?
    } else {
        normalized_path(&workspace.join(input))?
    };
    let relative = absolute.strip_prefix(&workspace).ok()?;
    if relative.as_os_str().is_empty() {
        return None;
    }
    Some(
        relative
            .components()
            .filter_map(|component| match component {
                Component::Normal(value) => Some(value.to_string_lossy()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("/"),
    )
}

fn normalized_path(path: &Path) -> Option<PathBuf> {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    return None;
                }
            }
            Component::Normal(value) => normalized.push(value),
        }
    }
    Some(normalized)
}

fn tool_role(name: &str) -> Option<u8> {
    match name {
        "web_search" | "web_fetch" | "web_extract" => Some(2),
        "read_file" | "grep" | "glob" => Some(3),
        "write_file" | "edit_file" | "notebook_edit" => Some(4),
        _ => None,
    }
}

fn handoff_message(role: u8) -> &'static str {
    match role {
        2 => "请搜索内容",
        3 => "请读取资料",
        4 => "请整理并写入",
        5 => "请验证结果",
        6 => "请审核操作",
        _ => "请处理任务",
    }
}

/// 功能：生成实际工具开始时的短活动文案。
/// 参数：工具名与白名单输入。
/// 返回：有界短文案。
/// 错误：无。
/// 状态：纯转换。
/// 并发与取消：同步无锁。
/// 权限：不展示完整路径、URL、正文或命令。
/// 关联：ToolExecutionStart。
pub(crate) fn tool_activity_summary(name: &str, input: &Value) -> String {
    match name {
        "web_search" => input
            .get("query")
            .and_then(Value::as_str)
            .map(compact_search_topic)
            .filter(|topic| !topic.is_empty())
            .map(|topic| format!("搜索：{topic}"))
            .unwrap_or_else(|| "搜索网页内容".into()),
        "web_fetch" | "web_extract" => web_source(input)
            .map(|source| format!("查阅：{source}"))
            .unwrap_or_else(|| "查阅网页资料".into()),
        "read_file" => file_topic(input)
            .map(|topic| format!("读取：{topic}"))
            .unwrap_or_else(|| "读取本地资料".into()),
        "write_file" => file_topic(input)
            .map(|topic| format!("写入：{topic}"))
            .unwrap_or_else(|| "整理并写入文档".into()),
        "edit_file" => file_topic(input)
            .map(|topic| format!("修改：{topic}"))
            .unwrap_or_else(|| "修改文档内容".into()),
        "notebook_edit" => file_topic(input)
            .map(|topic| format!("更新笔记：{topic}"))
            .unwrap_or_else(|| "更新笔记内容".into()),
        "grep" => "检索本地资料".into(),
        "glob" => "查找本地文件".into(),
        "bash" => "执行受控命令".into(),
        "todo_add" => "新增任务记录".into(),
        "todo_update" => "更新任务记录".into(),
        "todo_list" => "读取任务记录".into(),
        _ => "处理工具任务".into(),
    }
}

fn tool_activity_completion(name: &str, input: &Value) -> String {
    match name {
        "web_search" => input
            .get("query")
            .and_then(Value::as_str)
            .map(compact_search_topic)
            .filter(|topic| !topic.is_empty())
            .map(|topic| format!("已搜索：{topic}，正在整理"))
            .unwrap_or_else(|| "已完成搜索，正在整理".into()),
        "web_fetch" | "web_extract" => web_source(input)
            .map(|source| format!("已查阅：{source}，正在整理"))
            .unwrap_or_else(|| "已查阅网页资料，正在整理".into()),
        "read_file" => file_topic(input)
            .map(|topic| format!("已读取：{topic}，正在整理"))
            .unwrap_or_else(|| "已读取本地资料，正在整理".into()),
        "write_file" => file_topic(input)
            .map(|topic| format!("已写入：{topic}，继续处理"))
            .unwrap_or_else(|| "已写入文档，继续处理".into()),
        "edit_file" => file_topic(input)
            .map(|topic| format!("已修改：{topic}，继续处理"))
            .unwrap_or_else(|| "已修改文档，继续处理".into()),
        "notebook_edit" => file_topic(input)
            .map(|topic| format!("已更新笔记：{topic}，继续处理"))
            .unwrap_or_else(|| "已更新笔记，继续处理".into()),
        "grep" | "glob" => "已检索本地资料，正在整理".into(),
        "bash" => "受控命令已结束，继续处理".into(),
        "todo_add" | "todo_update" | "todo_list" => "任务记录已更新，继续处理".into(),
        _ => "工具任务已结束，继续处理".into(),
    }
}

/// 功能：生成工具结果活动文案。
/// 参数：可选工具名和是否失败。
/// 返回：固定短文案。
/// 错误：无。
/// 状态：纯转换；后续活动可覆盖。
/// 并发与取消：同步无锁。
/// 权限：不展示工具原始结果或媒体。
/// 关联：Office 工具事件。
pub(crate) fn tool_result_activity(name: Option<&str>, is_error: bool) -> &'static str {
    if is_error {
        return match name {
            Some("web_search" | "web_fetch" | "web_extract") => "搜索失败，正在调整",
            Some("read_file" | "grep" | "glob") => "资料处理失败，正在调整",
            Some("write_file" | "edit_file" | "notebook_edit") => "文档处理失败，正在调整",
            _ => "工具处理失败，正在调整",
        };
    }
    match name {
        Some("web_search" | "web_fetch" | "web_extract") => "正在整理搜索结果",
        Some("read_file" | "grep" | "glob") => "正在整理已读取资料",
        Some("write_file" | "edit_file" | "notebook_edit") => "文档处理完成，继续整理",
        Some("todo_add" | "todo_update" | "todo_list") => "任务记录已更新，继续处理",
        _ => "正在整理工具结果",
    }
}

fn compact_search_topic(value: &str) -> String {
    compact_topic(value, 18)
}

fn file_topic(input: &Value) -> Option<String> {
    let value = input.get("path")?.as_str()?;
    let normalized_path = value.replace('\\', "/");
    let stem = Path::new(&normalized_path).file_stem()?.to_str()?;
    let topic = compact_topic(stem, 12);
    (!topic.is_empty()).then_some(topic)
}

fn web_source(input: &Value) -> Option<String> {
    let value = input.get("url")?.as_str()?;
    let url = Url::parse(value).ok()?;
    let host = url.host_str()?.trim_start_matches("www.");
    let source = compact_topic(host, 20);
    (!source.is_empty()).then_some(source)
}

fn compact_topic(value: &str, max_chars: usize) -> String {
    let readable = value.replace(['_', '-'], " ");
    let normalized = readable.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = normalized.chars();
    let prefix: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{prefix}…")
    } else {
        prefix
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openpaw_engine::AgentEvent;

    #[tokio::test]
    async fn main_large_directory_publishes_all_offices_within_each_request_limit() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let received = Arc::new(Mutex::new(
            std::collections::BTreeMap::<String, Value>::new(),
        ));
        let captured = received.clone();
        let server = tokio::spawn(async move {
            let router = axum::Router::new().fallback(axum::routing::post(move |axum::Json(body): axum::Json<Value>| {
                let captured = captured.clone();
                async move {
                    let data = match body["type"].as_str() {
                        Some("office.capabilities") => serde_json::json!({"scene":{},"ui":{},"requests":{"properties":{"conversations":{}}},"responses":{"success":{},"error":{}}}),
                        Some("ui.next") => { tokio::time::sleep(Duration::from_millis(20)).await; serde_json::json!({"actions":[]}) },
                        _ => {
                            if body["type"] == "offices.initialize" {
                                assert!(serde_json::to_vec(&body).unwrap().len() <= 65536);
                                for snapshot in body["offices"].as_array().unwrap() {
                                    captured.lock().unwrap().insert(snapshot["workspace"]["id"].as_str().unwrap().into(), snapshot.clone());
                                }
                            }
                            serde_json::json!({"offices":captured.lock().unwrap().values().map(|snapshot|serde_json::json!({"id":snapshot["workspace"]["id"],"name":snapshot["workspace"]["name"],"seq":snapshot["seq"],"runtimeReady":true,"taskCount":snapshot["conversations"][0]["tasks"].as_array().map_or(0,Vec::len)})).collect::<Vec<_>>(),"actors":[],"supportedMessages":[],"sceneCommandProtocol":"2.0","ui":{}})
                        }
                    };
                    axum::Json(serde_json::json!({"code":200,"msg":"OK","data":data}))
                }
            })).layer(axum::extract::DefaultBodyLimit::max(65536));
            axum::serve(listener, router).await.unwrap();
        });
        let home = tempfile::tempdir().unwrap();
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let office = MainOffice::new(home.path(), first.path(), Some(format!("http://{address}")));
        office.register(second.path()).unwrap();
        {
            let mut guard = office.records.lock().unwrap();
            let records = guard.as_mut().unwrap();
            for (index, record) in records.values_mut().enumerate() {
                record.conversation = Some(format!("existing-{index}"));
                record.session = Some(format!("owner-{index}"));
                record.snapshot["tasks"] = serde_json::json!((0..20).map(|task|serde_json::json!({"id":format!("task-{task}"),"assigneeId":format!("owner-{index}"),"title":"保留历史任务","status":"blocked","progress":0,"updatedAt":"2026-10-27T00:00:00Z","summary":"s".repeat(1000)})).collect::<Vec<_>>());
                record.snapshot["whiteboard"]["detail"] = "d".repeat(16000).into();
            }
            office.save(records).unwrap();
        }
        let original = office.directory().unwrap();
        assert!(serde_json::to_vec(&original).unwrap().len() > 65536);
        for snapshot in &original {
            assert!(serde_json::to_vec(snapshot).unwrap().len() + 4096 < 65536);
        }
        let stop = tokio_util::sync::CancellationToken::new();
        office.start(stop.clone());
        let synchronized = tokio::time::timeout(Duration::from_secs(3), async {
            while received.lock().unwrap().len() != 2 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
        stop.cancel();
        server.abort();
        synchronized.expect("目录累计超过64KiB时仍须逐办公室发布全部事实");
        for snapshot in office.directory().unwrap() {
            assert_eq!(
                received.lock().unwrap()[snapshot["workspace"]["id"].as_str().unwrap()],
                snapshot
            );
        }
    }

    #[test]
    fn main_large_directory_binding_does_not_inherit_another_conversations_goal() {
        let home = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let office = MainOffice::new(
            home.path(),
            workspace.path(),
            Some("http://127.0.0.1:1".into()),
        );
        let team = office.directory().unwrap()[0]["workspace"]["id"]
            .as_str()
            .unwrap()
            .to_owned();
        office.bind(&team, "old", "owner").unwrap();
        {
            let mut guard = office.records.lock().unwrap();
            let records = guard.as_mut().unwrap();
            records.get_mut(&team).unwrap().snapshot["goal"] =
                serde_json::json!({"id":"old-goal","phase":"paused"});
            office.save(records).unwrap();
        }
        office.bind(&team, "new", "new-owner").unwrap();
        let record = office.conversation_record(&team, "new").unwrap().unwrap();
        assert!(
            record.snapshot.get("goal").is_none(),
            "新会话不能继承旧Goal"
        );
        assert!(record.snapshot["tasks"].as_array().unwrap().is_empty());
        assert_eq!(
            office.record(&team).unwrap().snapshot["goal"]["id"],
            "old-goal"
        );
    }

    #[tokio::test]
    async fn main_original_goal_activity_negotiate_and_preserve_binary_facts() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let router = axum::Router::new().fallback(axum::routing::post(|| async {
                axum::Json(serde_json::json!({"code":200,"msg":"OK","data":{
                    "scene":{},"ui":{"whiteboardSchema":{"properties":{"summary":{},"detail":{}}}},
                    "requests":{"snapshot":{"properties":{"conversations":{}}}},"responses":{"success":{},"error":{}}
                }}))
            }));
            axum::serve(listener, router).await.unwrap();
        });
        let home = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let office = MainOffice::new(
            home.path(),
            workspace.path(),
            Some(format!("http://{address}")),
        );
        let team = office.directory().unwrap()[0]["workspace"]["id"]
            .as_str()
            .unwrap()
            .to_owned();
        office.bind(&team, "native-goal", "owner").unwrap();
        let mut snapshot = office.record(&team).unwrap().snapshot;
        snapshot["employees"] = serde_json::json!([{"id":"owner","sceneActorId":"office-lead","name":"王明","online":false}]);
        let goal = serde_json::json!({"id":"goal-fixture","session_id":"owner","objective":"原版前端兼容验证","phase":"active","plan_version":1,"confirmed_version":1,"reason":"等待整体验收","tasks":(0..5).map(|i| serde_json::json!({"id":format!("task-{i}"),"title":format!("任务{}",i+1),"current":true,"status":if i==0 {"accepted"} else {"pending"},"dependencies":[],"expected_result":"实际结果","acceptance_criteria":["实际检查"]})).collect::<Vec<_>>()});
        apply_goal_projection(&mut snapshot, &goal);
        {
            let mut records = office.records.lock().unwrap();
            let records = records.as_mut().unwrap();
            records.get_mut(&team).unwrap().snapshot = snapshot.clone();
            office.save(records).unwrap();
        }
        let persisted = std::fs::read(&office.path).unwrap();
        assert_eq!(office.wire_snapshot(&snapshot), snapshot);
        office
            .message(
                serde_json::json!({"type":"office.capabilities"}),
                Duration::from_secs(2),
            )
            .await
            .unwrap();
        let directory = office.directory().unwrap();
        let message = office.wire_message(
            serde_json::json!({"type":"office.snapshot","teamId":team,"snapshot":snapshot}),
        );
        let initialized = office
            .wire_message(serde_json::json!({"type":"offices.initialize","offices":[snapshot]}));
        for wire in [
            &directory[0],
            &message["snapshot"],
            &initialized["offices"][0],
        ] {
            let conversation = &wire["conversations"][0];
            assert!(conversation.get("goal").is_none());
            let tasks = conversation["tasks"].as_array().unwrap();
            assert_eq!(tasks.len(), 5);
            for (i, task) in tasks.iter().enumerate() {
                assert!(task.get("goalId").is_none());
                assert_eq!(task["progress"], if i == 0 { 100 } else { 0 });
                assert_eq!(task["summary"], snapshot["tasks"][i]["summary"]);
            }
            let board = &conversation["whiteboard"];
            assert!(board.get("progressMode").is_none());
            assert_eq!(board["steps"], snapshot["whiteboard"]["steps"]);
            assert_eq!(board["steps"].as_array().unwrap().len(), 3);
            assert_eq!(board["status"], "running");
            assert!(board["summary"].as_str().unwrap().contains("时间动画"));
            let detail = board["detail"].as_str().unwrap();
            assert!(detail.contains("任务5 — 0%"));
            assert!(detail.contains("等待整体验收"));
            assert!(detail.contains("实际检查"));
            if let Ok(path) = std::env::var("GOAL_NATIVE_PROJECTION_FILE") {
                std::fs::write(path, serde_json::to_vec(wire).unwrap()).unwrap();
            }
        }
        assert_eq!(office.record(&team).unwrap().snapshot, snapshot);
        assert_eq!(std::fs::read(&office.path).unwrap(), persisted);
        for (phase, status) in [
            ("draft", "queued"),
            ("active", "running"),
            ("paused", "blocked"),
            ("blocked", "blocked"),
            ("ended", "failed"),
            ("completed", "completed"),
        ] {
            let mut goal = goal.clone();
            goal["phase"] = phase.into();
            if phase == "completed" || phase == "draft" {
                for task in goal["tasks"].as_array_mut().unwrap() {
                    task["status"] = if phase == "completed" {
                        "accepted"
                    } else {
                        "pending"
                    }
                    .into();
                }
            }
            let mut projected = snapshot.clone();
            apply_goal_projection(&mut projected, &goal);
            let original = projected.clone();
            let wire = office.display_snapshot(projected);
            assert_eq!(wire["whiteboard"]["status"], status);
            assert_eq!(wire["whiteboard"]["steps"], original["whiteboard"]["steps"]);
            assert_eq!(wire["tasks"].as_array().unwrap().len(), 5);
            assert!(wire["whiteboard"].get("progressMode").is_none());
            if let Ok(path) = std::env::var("GOAL_ACTIVITY_PROJECTION_DIR") {
                std::fs::write(
                    PathBuf::from(path).join(format!("{phase}.json")),
                    serde_json::to_vec(&wire).unwrap(),
                )
                .unwrap();
            }
        }
        assert_eq!(std::fs::read(&office.path).unwrap(), persisted);
        let long = snapshot["tasks"][0]["summary"]
            .as_str()
            .unwrap()
            .repeat(1000);
        let mut large = snapshot.clone();
        large["tasks"][0]["summary"] = long.into();
        assert!(
            office.display_snapshot(large)["whiteboard"]["detail"]
                .as_str()
                .unwrap()
                .encode_utf16()
                .count()
                <= 16000
        );
        server.abort();
    }

    #[tokio::test]
    async fn main_employee_display_keeps_roster_names_and_original_work_facts() {
        let home = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let office = MainOffice::new(home.path(), workspace.path(), None);
        let initial = office.directory().unwrap().remove(0);
        let team = initial["workspace"]["id"].as_str().unwrap();
        let actors = [
            "office-lead",
            "code-agent",
            "file-agent",
            "app-agent",
            "review-agent",
            "data-agent",
            "",
            "unknown-actor",
        ];
        {
            let mut guard = office.records.lock().unwrap();
            let entry = guard.as_mut().unwrap().get_mut(team).unwrap();
            entry.conversation = Some("original-conversation".into());
            entry.snapshot["employees"] = actors.iter().enumerate().map(|(index, actor)| {
                serde_json::json!({"id":format!("employee-{index}"),"sceneActorId":actor,"name":"天气检索","role":"子 Agent","online":false})
            }).collect::<Vec<_>>().into();
            entry.snapshot["tasks"] = serde_json::json!([{"id":"original-task","title":"查询天气","assigneeId":"employee-1"}]);
            let mut other = entry.primary.clone();
            other.conversation = Some("other-conversation".into());
            entry
                .conversations
                .insert("other-conversation".into(), other);
        }
        let original = office.record(team).unwrap().snapshot;
        let expected = serde_json::json!([
            "王明",
            "李研",
            "周理",
            "陈书",
            "刘市",
            "赵审",
            "天气检索",
            "天气检索"
        ]);
        for wire in [false, true] {
            office
                .conversation_wire
                .store(wire, std::sync::atomic::Ordering::Release);
            let directory = office.directory().unwrap().remove(0);
            let message = office.wire_message(
                serde_json::json!({"type":"office.snapshot","teamId":team,"snapshot":original}),
            );
            let initialized = office.wire_message(
                serde_json::json!({"type":"offices.initialize","offices":[original]}),
            );
            for snapshot in [&directory, &message["snapshot"], &initialized["offices"][0]] {
                let records = if wire {
                    snapshot["conversations"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .collect::<Vec<_>>()
                } else {
                    vec![snapshot]
                };
                for record in records {
                    let employees = record["employees"].as_array().unwrap();
                    assert_eq!(
                        Value::from(
                            employees
                                .iter()
                                .map(|employee| employee["name"].clone())
                                .collect::<Vec<_>>()
                        ),
                        expected
                    );
                    assert_eq!(record["tasks"], original["tasks"]);
                    for (index, employee) in employees.iter().enumerate() {
                        assert_eq!(employee["id"], format!("employee-{index}"));
                        assert_eq!(employee["sceneActorId"], actors[index]);
                    }
                }
            }
            assert_eq!(office.record(team).unwrap().snapshot, original);
        }
    }

    fn recording_dispatcher() -> (OfficeDispatcher, mpsc::Receiver<OfficeAction>) {
        OfficeDispatcher::recording()
    }

    #[tokio::test]
    async fn main_conversation_wire_preserves_storage_and_scopes_decisions_and_actions() {
        let home = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let office = MainOffice::new(home.path(), workspace.path(), None);
        let snapshot = office.directory().unwrap().remove(0);
        let team = snapshot["workspace"]["id"].as_str().unwrap();
        assert_eq!(office.wire_snapshot(&snapshot), snapshot);
        office
            .conversation_wire
            .store(true, std::sync::atomic::Ordering::Release);
        assert_eq!(
            office.wire_snapshot(&snapshot)["conversations"],
            serde_json::json!([])
        );
        let state = crate::runtime::AppState::new(crate::protocol::HostConfig {
            home: home.path().into(),
            workspace: workspace.path().into(),
            provider: "echo".into(),
            ..Default::default()
        });
        let handle = state
            .create_session(Some("echo"), None, None, Some("真实登记会话"))
            .await
            .unwrap();
        office.bind(team, "real-conversation", &handle.id).unwrap();
        let record = office.record(team).unwrap();
        let wire = office.wire_snapshot(&record.snapshot);
        assert_eq!(wire["conversations"][0]["id"], "real-conversation");
        assert!(wire.get("employees").is_none());
        assert!(record.snapshot.get("conversations").is_none());
        for message in [
            serde_json::json!({"teamId":team,"command":{"type":"actor.presentation.set"}}),
            serde_json::json!({"teamId":team,"type":"decision.request","decision":{"id":"permission"}}),
        ] {
            assert_eq!(
                office.wire_message(message)["conversationId"],
                "real-conversation"
            );
        }
        let reopened = MainOffice::new(home.path(), workspace.path(), None);
        assert_eq!(
            reopened.record(team).unwrap().conversation.as_deref(),
            Some("real-conversation")
        );
    }

    #[test]
    fn attached_prompt_requires_delegation_when_user_explicitly_requests_multi_agent_work() {
        assert!(OFFICE_VISUAL_SYSTEM_PROMPT.contains("明确要求多 Agent、子 Agent、多空间协作"));
        assert!(OFFICE_VISUAL_SYSTEM_PROMPT.contains("必须先调用 delegate_task"));
        assert!(OFFICE_VISUAL_SYSTEM_PROMPT.contains("不能静默退化为单 Agent"));
    }

    #[tokio::test]
    async fn subagent_actions_keep_parent_session_and_child_workspace_scope() {
        let (dispatcher, mut actions) = recording_dispatcher();
        let workspace = tempfile::tempdir().unwrap();
        let mut turn = dispatcher.subagent_started(
            "parent-session",
            "parent-session:agent:1",
            "研究 Agent",
            workspace.path(),
        );
        dispatcher.observe_subagent(
            "parent-session:agent:1",
            &AgentEvent::ToolExecutionStart {
                tool_use_id: "tool-1".into(),
                name: "read_file".into(),
                input: serde_json::json!({"path":"README.md"}),
            },
            &mut turn,
        );
        dispatcher.subagent_ended(
            "parent-session",
            "parent-session:agent:1",
            "研究 Agent",
            turn,
            true,
        );
        assert!(!dispatcher
            .scopes
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .contains_key("parent-session:agent:1"));

        let emitted: Vec<_> = std::iter::from_fn(|| actions.try_recv().ok()).collect();
        assert!(emitted.iter().any(|action| matches!(
            action,
            OfficeAction::WorkspaceHandoff {
                from_workspace_id,
                to_workspace_id,
                session_id,
                phase,
                ..
            } if from_workspace_id == "parent-session"
                && to_workspace_id == "parent-session:agent:1"
                && session_id == "parent-session"
                && phase == "going"
        )));
        assert!(emitted.iter().any(|action| matches!(
            action,
            OfficeAction::SetState {
                workspace_id,
                workspace_name,
                session_id,
                parent_workspace_id: Some(parent_workspace_id),
                roster_no: 1,
                state,
                ..
            } if workspace_id == "parent-session:agent:1"
                && workspace_name == "研究 Agent"
                && session_id == "parent-session"
                && parent_workspace_id == "parent-session"
                && state == "thinking"
        )));
        assert!(emitted.iter().any(|action| matches!(
            action,
            OfficeAction::DeskVisit {
                workspace_id,
                session_id,
                parent_workspace_id: Some(parent_workspace_id),
                visitor: 1,
                host: 3,
                ..
            } if workspace_id == "parent-session:agent:1"
                && session_id == "parent-session"
                && parent_workspace_id == "parent-session"
        )));
        assert!(emitted.iter().any(|action| matches!(
            action,
            OfficeAction::WorkspaceHandoff {
                from_workspace_id,
                to_workspace_id,
                phase,
                ..
            } if from_workspace_id == "parent-session:agent:1"
                && to_workspace_id == "parent-session"
                && phase == "returning"
        )));

        tokio::time::sleep(Duration::from_millis(950)).await;
        assert!(matches!(
            actions.try_recv(),
            Ok(OfficeAction::WorkspaceHandoff { phase, .. }) if phase == "complete"
        ));
    }

    #[tokio::test]
    async fn deleted_session_drops_delayed_workspace_handoff_completion() {
        let (dispatcher, mut actions) = recording_dispatcher();
        let workspace = tempfile::tempdir().unwrap();
        let turn = dispatcher.subagent_started(
            "deleted-session",
            "deleted-session:agent:1",
            "研究 Agent",
            workspace.path(),
        );
        dispatcher.subagent_ended(
            "deleted-session",
            "deleted-session:agent:1",
            "研究 Agent",
            turn,
            true,
        );
        while actions.try_recv().is_ok() {}
        dispatcher.remove_scope("deleted-session");

        tokio::time::sleep(Duration::from_millis(950)).await;
        assert!(actions.try_recv().is_err());
    }

    #[tokio::test]
    async fn subagent_registers_only_successful_workspace_relative_artifacts() {
        let (dispatcher, mut actions) = recording_dispatcher();
        let workspace = tempfile::tempdir().unwrap();
        let mut turn = dispatcher.subagent_started(
            "parent-session",
            "parent-session:agent:1",
            "实现 Agent",
            workspace.path(),
        );
        while actions.try_recv().is_ok() {}

        let cases = [
            (
                "write",
                "write_file",
                serde_json::json!({"path":"reports/result.md","content":"private body"}),
                "reports/result.md",
            ),
            (
                "edit",
                "edit_file",
                serde_json::json!({
                    "path": workspace.path().join("notes/plan.md"),
                    "old_string":"private old",
                    "new_string":"private new"
                }),
                "notes/plan.md",
            ),
            (
                "notebook",
                "notebook_edit",
                serde_json::json!({
                    "path":"charts/demo.ipynb",
                    "operation":"insert",
                    "cell_index":0,
                    "source":"private source"
                }),
                "charts/demo.ipynb",
            ),
        ];
        for (tool_use_id, name, input, _) in &cases {
            dispatcher.observe_subagent(
                "parent-session:agent:1",
                &AgentEvent::ToolExecutionStart {
                    tool_use_id: (*tool_use_id).into(),
                    name: (*name).into(),
                    input: input.clone(),
                },
                &mut turn,
            );
            dispatcher.observe_subagent(
                "parent-session:agent:1",
                &AgentEvent::ToolResult {
                    media: Vec::new(),
                    tool_use_id: (*tool_use_id).into(),
                    content: "ok".into(),
                    is_error: false,
                    name: Some((*name).into()),
                    duration_ms: Some(1),
                },
                &mut turn,
            );
        }
        dispatcher.observe_subagent(
            "parent-session:agent:1",
            &AgentEvent::ToolExecutionStart {
                tool_use_id: "failed".into(),
                name: "write_file".into(),
                input: serde_json::json!({"path":"failed.md","content":"must not register"}),
            },
            &mut turn,
        );
        dispatcher.observe_subagent(
            "parent-session:agent:1",
            &AgentEvent::ToolResult {
                media: Vec::new(),
                tool_use_id: "failed".into(),
                content: "denied".into(),
                is_error: true,
                name: Some("write_file".into()),
                duration_ms: Some(1),
            },
            &mut turn,
        );

        let artifacts: Vec<_> = std::iter::from_fn(|| actions.try_recv().ok())
            .filter_map(|action| match action {
                OfficeAction::RegisterArtifact {
                    session_id,
                    workspace_id,
                    path,
                } => Some((session_id, workspace_id, path)),
                _ => None,
            })
            .collect();
        assert_eq!(
            artifacts,
            cases
                .iter()
                .map(|(_, _, _, path)| {
                    (
                        "parent-session".to_string(),
                        "parent-session:agent:1".to_string(),
                        (*path).to_string(),
                    )
                })
                .collect::<Vec<_>>()
        );
        let wire = serde_json::to_string(&OfficeAction::RegisterArtifact {
            session_id: "parent-session".into(),
            workspace_id: "parent-session:agent:1".into(),
            path: "reports/result.md".into(),
        })
        .unwrap();
        assert!(!wire.contains("private"));
        assert!(!wire.contains(&workspace.path().display().to_string()));
        assert!(!wire.contains("command"));
    }

    #[tokio::test]
    async fn subagent_projects_incremental_and_final_model_text_without_reasoning() {
        let (dispatcher, mut actions) = recording_dispatcher();
        let workspace = tempfile::tempdir().unwrap();
        let mut turn = dispatcher.subagent_started(
            "parent-session",
            "parent-session:agent:1",
            "研究 Agent",
            workspace.path(),
        );
        while actions.try_recv().is_ok() {}

        dispatcher.observe_subagent(
            "parent-session:agent:1",
            &AgentEvent::ThinkingDelta {
                text: "不得投影的内部推理".into(),
            },
            &mut turn,
        );
        let first = "研究结果".repeat(30);
        let second = "，这是最终结论。";
        dispatcher.observe_subagent(
            "parent-session:agent:1",
            &AgentEvent::TextDelta {
                text: first.clone(),
            },
            &mut turn,
        );
        dispatcher.observe_subagent(
            "parent-session:agent:1",
            &AgentEvent::TextDelta {
                text: second.into(),
            },
            &mut turn,
        );
        dispatcher.subagent_ended(
            "parent-session",
            "parent-session:agent:1",
            "研究 Agent",
            turn,
            true,
        );

        let outputs = std::iter::from_fn(|| actions.try_recv().ok())
            .filter_map(|action| match action {
                OfficeAction::WorkspaceOutput { kind, text, .. } => Some((kind, text)),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert!(outputs.iter().any(|(kind, text)| kind == "model_delta"
            && text.contains("研究结果")
            && !text.contains("内部推理")));
        assert_eq!(
            outputs.last(),
            Some(&("model_final".into(), format!("{first}{second}")))
        );
    }

    #[tokio::test]
    async fn tool_roles_form_a_handoff_chain_in_event_order() {
        assert_eq!(tool_role("read_file"), Some(3));
        assert_eq!(tool_role("todo_add"), None);
        assert_eq!(tool_role("unknown"), None);
        let (d, mut actions) = recording_dispatcher();
        let mut roles = HashSet::new();
        let mut last_tool_role = None;
        let mut tool_failed = false;
        let mut active_tool_roles = HashMap::new();
        d.observe(
            "session-a",
            &AgentEvent::ToolExecutionStart {
                tool_use_id: "1".into(),
                name: "web_search".into(),
                input: serde_json::json!({"query":"主题甲"}),
            },
            &mut roles,
            &mut last_tool_role,
            &mut tool_failed,
            &mut active_tool_roles,
        );
        d.observe(
            "session-a",
            &AgentEvent::ToolResult {
                media: Vec::new(),
                tool_use_id: "1".into(),
                content: "ok".into(),
                is_error: false,
                name: Some("web_search".into()),
                duration_ms: Some(1),
            },
            &mut roles,
            &mut last_tool_role,
            &mut tool_failed,
            &mut active_tool_roles,
        );
        d.observe(
            "session-a",
            &AgentEvent::ToolExecutionStart {
                tool_use_id: "2".into(),
                name: "read_file".into(),
                input: serde_json::json!({"path":"README.md"}),
            },
            &mut roles,
            &mut last_tool_role,
            &mut tool_failed,
            &mut active_tool_roles,
        );
        d.observe(
            "session-a",
            &AgentEvent::ToolResult {
                media: Vec::new(),
                tool_use_id: "2".into(),
                content: "ok".into(),
                is_error: false,
                name: Some("read_file".into()),
                duration_ms: Some(1),
            },
            &mut roles,
            &mut last_tool_role,
            &mut tool_failed,
            &mut active_tool_roles,
        );
        d.observe(
            "session-a",
            &AgentEvent::ToolExecutionStart {
                tool_use_id: "3".into(),
                name: "write_file".into(),
                input: serde_json::json!({"path":"private-resource"}),
            },
            &mut roles,
            &mut last_tool_role,
            &mut tool_failed,
            &mut active_tool_roles,
        );
        assert_eq!(last_tool_role, Some(4));
        assert!(!roles.contains(&2));
        assert!(!roles.contains(&3));
        assert!(roles.contains(&4));
        let visits: Vec<_> = std::iter::from_fn(|| actions.try_recv().ok())
            .filter_map(|action| match action {
                OfficeAction::DeskVisit {
                    visitor,
                    host,
                    message,
                    ..
                } => Some((visitor, host, message)),
                _ => None,
            })
            .collect();
        assert_eq!(
            visits,
            vec![
                (1, 2, "请搜索内容".into()),
                (2, 3, "请读取资料".into()),
                (3, 4, "请整理并写入".into()),
            ]
        );
    }

    #[tokio::test]
    async fn todo_tools_do_not_emit_office_actions() {
        let (dispatcher, mut actions) = recording_dispatcher();
        let mut roles = HashSet::new();
        let mut last_tool_role = None;
        let mut tool_failed = false;
        let mut active_tool_roles = HashMap::new();

        for (index, name) in ["todo_add", "todo_update", "todo_list"].iter().enumerate() {
            dispatcher.observe(
                "session-a",
                &AgentEvent::ToolExecutionStart {
                    tool_use_id: format!("todo-{index}"),
                    name: (*name).into(),
                    input: Value::Null,
                },
                &mut roles,
                &mut last_tool_role,
                &mut tool_failed,
                &mut active_tool_roles,
            );
        }

        assert_eq!(last_tool_role, None);
        assert!(roles.is_empty());
        assert!(actions.try_recv().is_err());
    }

    #[tokio::test]
    async fn approval_sets_reviewer_without_adding_a_visit() {
        let (dispatcher, mut actions) = recording_dispatcher();
        let mut roles = HashSet::new();
        let mut last_tool_role = None;
        let mut tool_failed = false;
        let mut active_tool_roles = HashMap::new();
        dispatcher.observe(
            "session-a",
            &AgentEvent::ClassifierStatus {
                tool_use_id: "call".into(),
                status: ClassifierStatus::SuggestConfirm {
                    reason: "manual".into(),
                },
            },
            &mut roles,
            &mut last_tool_role,
            &mut tool_failed,
            &mut active_tool_roles,
        );
        assert_eq!(last_tool_role, None);
        assert!(roles.contains(&6));
        let emitted: Vec<_> = std::iter::from_fn(|| actions.try_recv().ok()).collect();
        assert_eq!(
            emitted
                .iter()
                .filter(|action| matches!(action, OfficeAction::DeskVisit { .. }))
                .count(),
            0
        );
        assert!(emitted.iter().any(|action| matches!(
            action,
            OfficeAction::SetState {
                roster_no: 6,
                state,
                ..
            } if state == "working"
        )));
    }

    #[tokio::test]
    async fn chain_tail_stays_working_after_its_last_tool_result() {
        let (dispatcher, mut actions) = recording_dispatcher();
        let mut roles = HashSet::new();
        let mut last_tool_role = None;
        let mut tool_failed = false;
        let mut active_tool_roles = HashMap::new();

        for tool_use_id in ["read-1", "read-2"] {
            dispatcher.observe(
                "session-a",
                &AgentEvent::ToolExecutionStart {
                    tool_use_id: tool_use_id.into(),
                    name: "read_file".into(),
                    input: serde_json::json!({"path":"README.md"}),
                },
                &mut roles,
                &mut last_tool_role,
                &mut tool_failed,
                &mut active_tool_roles,
            );
        }
        while actions.try_recv().is_ok() {}

        dispatcher.observe(
            "session-a",
            &AgentEvent::ToolResult {
                media: Vec::new(),
                tool_use_id: "read-1".into(),
                content: "ok".into(),
                is_error: false,
                name: Some("read_file".into()),
                duration_ms: Some(1),
            },
            &mut roles,
            &mut last_tool_role,
            &mut tool_failed,
            &mut active_tool_roles,
        );
        assert!(roles.contains(&3));
        assert!(actions.try_recv().is_err());

        dispatcher.observe(
            "session-a",
            &AgentEvent::ToolResult {
                media: Vec::new(),
                tool_use_id: "read-2".into(),
                content: "ok".into(),
                is_error: false,
                name: Some("read_file".into()),
                duration_ms: Some(1),
            },
            &mut roles,
            &mut last_tool_role,
            &mut tool_failed,
            &mut active_tool_roles,
        );
        assert!(roles.contains(&3));
        assert!(matches!(
            actions.try_recv(),
            Ok(OfficeAction::SetState {
                roster_no: 3,
                state,
                ..
            }) if state == "working"
        ));
    }

    #[tokio::test]
    async fn active_previous_role_is_released_only_after_its_last_result() {
        let (dispatcher, mut actions) = recording_dispatcher();
        let mut roles = HashSet::new();
        let mut last_tool_role = None;
        let mut tool_failed = false;
        let mut active_tool_roles = HashMap::new();

        dispatcher.observe(
            "session-a",
            &AgentEvent::ToolExecutionStart {
                tool_use_id: "web".into(),
                name: "web_search".into(),
                input: serde_json::json!({"query":"主题甲"}),
            },
            &mut roles,
            &mut last_tool_role,
            &mut tool_failed,
            &mut active_tool_roles,
        );
        dispatcher.observe(
            "session-a",
            &AgentEvent::ToolExecutionStart {
                tool_use_id: "read".into(),
                name: "read_file".into(),
                input: serde_json::json!({"path":"README.md"}),
            },
            &mut roles,
            &mut last_tool_role,
            &mut tool_failed,
            &mut active_tool_roles,
        );
        assert_eq!(last_tool_role, Some(3));
        assert!(roles.contains(&2));
        assert!(roles.contains(&3));
        while actions.try_recv().is_ok() {}

        dispatcher.observe(
            "session-a",
            &AgentEvent::ToolResult {
                media: Vec::new(),
                tool_use_id: "web".into(),
                content: "ok".into(),
                is_error: false,
                name: Some("web_search".into()),
                duration_ms: Some(1),
            },
            &mut roles,
            &mut last_tool_role,
            &mut tool_failed,
            &mut active_tool_roles,
        );

        assert!(!roles.contains(&2));
        assert!(roles.contains(&3));
        assert!(matches!(
            actions.try_recv(),
            Ok(OfficeAction::SetState {
                roster_no: 2,
                state,
                ..
            }) if state == "idle"
        ));
    }

    #[test]
    fn action_wire_uses_gateway_names() {
        let state = serde_json::to_value(OfficeAction::SetState {
            workspace_id: "session-a".into(),
            workspace_name: "我的空间".into(),
            session_id: "session-a".into(),
            parent_workspace_id: None,
            roster_no: 1,
            state: "thinking".into(),
            task: None,
        })
        .unwrap();
        assert_eq!(state["type"], "set_state");
        assert_eq!(state["rosterNo"], 1);
        assert_eq!(state["workspaceId"], "session-a");
        assert_eq!(state["sessionId"], "session-a");
        let visit = serde_json::to_value(OfficeAction::DeskVisit {
            workspace_id: "session-a".into(),
            workspace_name: "我的空间".into(),
            session_id: "session-a".into(),
            parent_workspace_id: None,
            visitor: 1,
            host: 3,
            message: "read_file".into(),
        })
        .unwrap();
        assert_eq!(visit["type"], "desk_visit");
    }

    #[test]
    fn turn_started_only_clears_previous_working_roles() {
        let dispatcher = OfficeDispatcher::new(None);
        let mut working = HashSet::from([3, 5]);
        dispatcher.turn_started("session-a", &mut working);
        assert_eq!(working, HashSet::from([1]));
    }

    #[tokio::test]
    async fn turn_ended_releases_all_roles_without_fake_review_work() {
        let (dispatcher, mut actions) = recording_dispatcher();
        let mut working = HashSet::new();
        let mut last_tool_role = None;
        let active_tool_roles = HashMap::from([(
            "read".to_string(),
            ActiveToolProjection {
                role: 3,
                completed_task: "已读取资料，正在整理".into(),
                failed_task: "读取资料；失败，正在调整".into(),
            },
        )]);
        dispatcher.turn_started("session-a", &mut working);
        dispatcher.send_tool_working(
            "session-a",
            3,
            "读取本地资料",
            &mut working,
            &mut last_tool_role,
            &active_tool_roles,
        );
        while actions.try_recv().is_ok() {}

        dispatcher.turn_ended("session-a", &mut working, last_tool_role, 0, false);

        assert!(working.is_empty());
        let emitted: Vec<_> = std::iter::from_fn(|| actions.try_recv().ok()).collect();
        assert!(emitted.iter().any(|action| matches!(
            action,
            OfficeAction::SetState { roster_no: 1, state, .. } if state == "idle"
        )));
        assert!(emitted.iter().any(|action| matches!(
            action,
            OfficeAction::SetState { roster_no: 3, state, .. } if state == "idle"
        )));
        assert!(!emitted
            .iter()
            .any(|action| matches!(action, OfficeAction::SetState { roster_no: 5, .. })));
    }

    #[tokio::test]
    async fn parallel_sessions_release_only_their_own_workspace_role() {
        let (dispatcher, mut actions) = recording_dispatcher();
        let mut first = HashSet::new();
        let mut second = HashSet::new();
        dispatcher.turn_started("session-a", &mut first);
        dispatcher.turn_started("session-b", &mut second);
        dispatcher.turn_ended("session-a", &mut first, None, 0, false);

        let first_batch: Vec<_> = std::iter::from_fn(|| actions.try_recv().ok()).collect();
        assert!(first_batch.iter().any(|action| matches!(
            action,
            OfficeAction::SetState {
                session_id,
                roster_no: 1,
                state,
                ..
            } if session_id == "session-a" && state == "idle"
        )));

        dispatcher.turn_ended("session-b", &mut second, None, 0, false);
        let second_batch: Vec<_> = std::iter::from_fn(|| actions.try_recv().ok()).collect();
        assert!(second_batch.iter().any(|action| matches!(
            action,
            OfficeAction::SetState {
                session_id,
                roster_no: 1,
                state,
                ..
            } if session_id == "session-b" && state == "idle"
        )));
    }

    #[tokio::test]
    async fn completed_tool_turn_emits_generic_delivery() {
        let (dispatcher, mut actions) = recording_dispatcher();
        let mut working = HashSet::new();
        dispatcher.turn_started("session-a", &mut working);
        while actions.try_recv().is_ok() {}

        dispatcher.turn_ended("session-a", &mut working, Some(4), 0, true);

        assert!(matches!(
            actions.try_recv(),
            Ok(OfficeAction::DeskVisit {
                visitor: 4,
                host: 1,
                message,
                ..
            }) if message == "任务已完成，请查收"
        ));
    }

    #[tokio::test]
    async fn clear_owner_idles_only_the_aborted_session_workspace() {
        let (dispatcher, mut actions) = recording_dispatcher();
        let mut first = HashSet::new();
        let mut second = HashSet::new();
        dispatcher.turn_started("session-a", &mut first);
        dispatcher.turn_started("session-b", &mut second);
        while actions.try_recv().is_ok() {}

        dispatcher.clear_owner("session-a");

        let emitted: Vec<_> = std::iter::from_fn(|| actions.try_recv().ok()).collect();
        assert!(emitted.iter().any(|action| matches!(
            action,
            OfficeAction::SetState {
                session_id,
                roster_no: 1,
                state,
                ..
            } if session_id == "session-a" && state == "idle"
        )));
        assert!(!emitted.iter().any(|action| matches!(
            action,
            OfficeAction::SetState { session_id, .. } if session_id == "session-b"
        )));

        dispatcher.clear_owner("session-b");
        assert!(matches!(
            actions.try_recv(),
            Ok(OfficeAction::SetState {
                session_id,
                roster_no: 1,
                state,
                ..
            }) if session_id == "session-b" && state == "idle"
        ));
    }

    #[tokio::test]
    async fn completed_artifact_turn_is_delivered_through_liu_shi() {
        let (dispatcher, mut actions) = recording_dispatcher();
        let mut working = HashSet::new();
        dispatcher.turn_started("session-a", &mut working);
        while actions.try_recv().is_ok() {}

        dispatcher.turn_ended("session-a", &mut working, Some(4), 2, true);

        assert!(matches!(
            actions.try_recv(),
            Ok(OfficeAction::DeskVisit {
                visitor: 4,
                host: 5,
                message,
                ..
            }) if message == "请整理产物"
        ));
        assert!(matches!(
            actions.try_recv(),
            Ok(OfficeAction::SetState {
                roster_no: 5,
                state,
                task: Some(task),
                ..
            }) if state == "working" && task == "整理 2 项产物"
        ));
        assert!(matches!(
            actions.try_recv(),
            Ok(OfficeAction::DeskVisit {
                visitor: 5,
                host: 1,
                message,
                ..
            }) if message == "产物已整理，请查收"
        ));
    }

    #[test]
    fn activity_summary_exposes_only_allowed_fields() {
        let document = serde_json::json!({
            "path":"private/项目资料汇总版本.md",
            "content":"private-body"
        });
        assert_eq!(
            tool_activity_summary(
                "web_search",
                &serde_json::json!({"query":"主题甲","token":"private-token"}),
            ),
            "搜索：主题甲"
        );
        assert_eq!(
            tool_activity_summary("write_file", &document),
            "写入：项目资料汇总版本"
        );
        assert_eq!(
            tool_activity_summary(
                "web_fetch",
                &serde_json::json!({"url":"https://user:password@example.invalid/private-resource"}),
            ),
            "查阅：example.invalid"
        );
        assert_eq!(
            tool_activity_summary("read_file", &document),
            "读取：项目资料汇总版本"
        );
        assert_eq!(
            tool_activity_completion("read_file", &document),
            "已读取：项目资料汇总版本，正在整理"
        );
        let visible = [
            tool_activity_summary("write_file", &document),
            tool_activity_summary("read_file", &document),
            tool_activity_completion("read_file", &document),
        ]
        .join(" ");
        assert!(!visible.contains("private/"));
        assert!(!visible.contains(".md"));
        assert!(!visible.contains("private-body"));
    }

    #[test]
    fn tool_result_activity_allows_later_events_to_replace_failures() {
        assert_eq!(
            tool_result_activity(Some("web_search"), true),
            "搜索失败，正在调整"
        );
        assert_eq!(
            tool_result_activity(Some("web_search"), false),
            "正在整理搜索结果"
        );
        assert_eq!(
            tool_result_activity(Some("read_file"), false),
            "正在整理已读取资料"
        );
    }

    #[tokio::test]
    async fn later_success_replaces_a_failed_search_state() {
        let (dispatcher, mut actions) = recording_dispatcher();
        let mut roles = HashSet::new();
        let mut last_tool_role = None;
        let mut tool_failed = false;
        let mut active_tools = HashMap::new();

        dispatcher.observe(
            "session-a",
            &AgentEvent::ToolExecutionStart {
                tool_use_id: "search-1".into(),
                name: "web_search".into(),
                input: serde_json::json!({"query":"主题甲"}),
            },
            &mut roles,
            &mut last_tool_role,
            &mut tool_failed,
            &mut active_tools,
        );
        dispatcher.observe(
            "session-a",
            &AgentEvent::ToolResult {
                media: Vec::new(),
                tool_use_id: "search-1".into(),
                content: "error".into(),
                is_error: true,
                name: Some("web_search".into()),
                duration_ms: Some(1),
            },
            &mut roles,
            &mut last_tool_role,
            &mut tool_failed,
            &mut active_tools,
        );
        dispatcher.observe(
            "session-a",
            &AgentEvent::ToolExecutionStart {
                tool_use_id: "search-2".into(),
                name: "web_search".into(),
                input: serde_json::json!({"query":"主题乙"}),
            },
            &mut roles,
            &mut last_tool_role,
            &mut tool_failed,
            &mut active_tools,
        );
        dispatcher.observe(
            "session-a",
            &AgentEvent::ToolResult {
                media: Vec::new(),
                tool_use_id: "search-2".into(),
                content: "ok".into(),
                is_error: false,
                name: Some("web_search".into()),
                duration_ms: Some(1),
            },
            &mut roles,
            &mut last_tool_role,
            &mut tool_failed,
            &mut active_tools,
        );

        let tasks: Vec<_> = std::iter::from_fn(|| actions.try_recv().ok())
            .filter_map(|action| match action {
                OfficeAction::SetState {
                    roster_no: 2,
                    task: Some(task),
                    ..
                } => Some(task),
                _ => None,
            })
            .collect();
        assert_eq!(
            tasks,
            vec![
                "搜索：主题甲",
                "搜索：主题甲；失败，正在调整",
                "搜索：主题乙",
                "已搜索：主题乙，正在整理",
            ]
        );
        assert!(!tool_failed);
    }

    #[tokio::test]
    async fn failed_tool_does_not_emit_a_successful_delivery() {
        let (dispatcher, mut actions) = recording_dispatcher();
        let mut roles = HashSet::new();
        let mut last_tool_role = None;
        let mut tool_failed = false;
        let mut active_tools = HashMap::new();
        dispatcher.observe(
            "session-a",
            &AgentEvent::ToolExecutionStart {
                tool_use_id: "write".into(),
                name: "write_file".into(),
                input: serde_json::json!({"path":"private-resource"}),
            },
            &mut roles,
            &mut last_tool_role,
            &mut tool_failed,
            &mut active_tools,
        );
        dispatcher.observe(
            "session-a",
            &AgentEvent::ToolResult {
                media: Vec::new(),
                tool_use_id: "write".into(),
                content: "denied".into(),
                is_error: true,
                name: Some("write_file".into()),
                duration_ms: Some(1),
            },
            &mut roles,
            &mut last_tool_role,
            &mut tool_failed,
            &mut active_tools,
        );
        while actions.try_recv().is_ok() {}
        dispatcher.turn_ended("session-a", &mut roles, last_tool_role, 0, !tool_failed);
        assert!(!std::iter::from_fn(|| actions.try_recv().ok())
            .any(|action| matches!(action, OfficeAction::DeskVisit { host: 1, .. })));
    }

    #[tokio::test]
    async fn unresponsive_gateway_does_not_block_event_projection() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listener");
        let address = listener.local_addr().expect("address");
        let server = tokio::spawn(async move {
            let mut connections = Vec::new();
            while let Ok((stream, _)) = listener.accept().await {
                connections.push(stream);
            }
        });
        let dispatcher = OfficeDispatcher::new(Some(format!("http://{address}/actions")));
        let mut roles = HashSet::new();
        let started = std::time::Instant::now();

        dispatcher.turn_started("session-a", &mut roles);

        assert!(started.elapsed() < Duration::from_millis(100));
        assert_eq!(roles, HashSet::from([1]));
        server.abort();
    }
}
