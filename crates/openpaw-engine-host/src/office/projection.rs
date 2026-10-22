//! 职责：将共享Run/call事实投影成完整Office快照并可靠同步。
//! 输入输出：原JSONL和Host状态到持久seq/完整内容及官方消息。
//! 依赖：原Store、官方client、单办公室协调任务。
//! 禁止职责：不伪造进度、工具开始或多Agent，不复制会话账本。
use super::{client::Client, dto, OfficeConfig};
use crate::{Host, HostError, HostResult, RunState, RunView};
use serde_json::{json, Value};
use std::{collections::BTreeMap, time::Duration};
use tokio_util::sync::CancellationToken;

/// 用途：持有Office同步任务；关闭时在Store之前完成最终投影。
/// 字段：停止token、任务和只读配置。
/// 来源去向：HostServer启动到Host关闭。
/// 约束：网络失败不改变Run结果，冲突停止自动投递。
/// 稳定性：T4内部生命周期。
/// 关联：需求/设计REQ-OP-011/012；DES-OP-009。
pub(crate) struct Coordinator {
    stop: CancellationToken,
    tasks: Vec<tokio::task::JoinHandle<()>>,
    config: OfficeConfig,
}
impl Coordinator {
    /// 功能：为每个授权办公室启动独立投影和同步循环。
    /// 参数：host为唯一运行服务，config为显式Office配置。
    /// 返回：同步任务所有者。
    /// 错误：凭据或客户端初始化错误立即拒绝启用。
    /// 状态：每个办公室先持久投影再发送；同seq重试不改变内容。
    /// 并发与取消：每办公室一个worker，不持资源锁等待网络。
    /// 权限：只读取配置授权槽，人物先验证capabilities。
    /// 关联：需求/设计REQ-OP-011/012；DES-OP-007/009。
    pub(crate) fn start(host: Host, config: OfficeConfig) -> HostResult<Self> {
        let _ = Client::new(&config)?;
        let stop = CancellationToken::new();
        let mut tasks = Vec::new();
        for team in host.inner.config.slots.keys() {
            let team = team.clone();
            let host = host.clone();
            let config = config.clone();
            let token = stop.clone();
            tasks.push(tokio::spawn(async move {
                let client=match Client::new(&config){Ok(c)=>c,Err(_)=>return};
                let mut previous_status=String::new();let mut halted=false;let mut delay=1;
                loop {
                    if token.is_cancelled(){break;}
                    let result=tokio::select!{_=token.cancelled()=>break,result=sync_once(&host,&config,&team,&client,halted)=>result};
                    let status=match result {Ok(s)=>{delay=1;s},Err(e)=>{if matches!(e.kind.as_str(),"sequence_conflict"|"protocol_error"|"office_auth"){
                        halted=true;let team=team.clone();
                        let _=host.inner.store.call(move|db|{if let Some((snapshot,_))=db.office_projection(&team)?{db.save_office_projection(&team,&snapshot,"halted")?;}Ok(())}).await;
                    }delay=(delay*2).min(30);e.kind}};
                    if status!=previous_status {eprintln!("engine-host Office {team}: {status}");previous_status=status;}
                    tokio::select!{_=token.cancelled()=>break,_=tokio::time::sleep(Duration::from_secs(delay))=>{}}
                }
            }));
        }
        Ok(Self {
            stop,
            tasks,
            config,
        })
    }
    /// 功能：停止网络循环，在Store关闭前保存全部最终快照。
    /// 参数：self为worker所有者，host为已完成Run清理的共享服务。
    /// 返回：最终投影可靠保存的结果。
    /// 错误：存储失败上报；网络不可用保留pending并诊断。
    /// 状态：先全部保存，再最多三秒尝试外发；下次启动继续同版本。
    /// 并发与取消：等待worker退出，不在Store释放后执行投影。
    /// 权限：只访问已配置办公室。
    /// 关联：需求/设计REQ-OP-009/011；DES-OP-009。
    pub(crate) async fn finish(self, host: &Host) -> HostResult<()> {
        self.stop.cancel();
        for task in self.tasks {
            task.await.map_err(HostError::storage)?;
        }
        for team in host.inner.config.slots.keys() {
            prepare(host, &self.config, team).await?;
        }
        let client = Client::new(&self.config)?;
        let flush = async {
            for team in host.inner.config.slots.keys() {
                let _ = sync_once(host, &self.config, team, &client, false).await;
            }
        };
        if tokio::time::timeout(Duration::from_secs(3), flush)
            .await
            .is_err()
        {
            eprintln!("engine-host Office: 最终快照已保存，部分投递待下次启动核对");
        }
        Ok(())
    }
}
fn status(state: RunState) -> &'static str {
    match state {
        RunState::Accepting | RunState::Accepted => "queued",
        RunState::Running => "running",
        RunState::Cancelling => "blocked",
        RunState::Completed => "completed",
        _ => "failed",
    }
}
fn actor(tool: &str) -> (&'static str, &'static str) {
    match tool {
        "read_file" => ("files", "file-agent"),
        "glob" | "grep" => ("search", "data-agent"),
        _ => ("assistant", "office-lead"),
    }
}
fn tool_status(run: &RunView, stage: &str) -> &'static str {
    match stage {
        "completed" => "completed",
        "failed" => "failed",
        "running" if !run.state.is_terminal() => "running",
        "proposed" if !run.state.is_terminal() => "queued",
        _ => {
            if run.state.is_terminal() {
                "failed"
            } else {
                "blocked"
            }
        }
    }
}
fn task(
    id: String,
    title: String,
    who: &str,
    state: &str,
    summary: String,
    old: &BTreeMap<String, Value>,
    now: &str,
) -> Value {
    let mut task = json!({"id":id,"title":dto::bounded(&title,160),"assigneeId":who,"status":state,"progress":if state=="completed"{100}else{-1},"summary":dto::bounded(&summary,1000),"updatedAt":now});
    if let Some(prior) = old.get(&id) {
        let mut comparable = prior.clone();
        comparable["updatedAt"] = Value::String(now.into());
        if comparable == task {
            task["updatedAt"] = prior["updatedAt"].clone();
        }
    }
    task
}
/// 功能：读取事实、生成有界完整快照并在变更时分配持久版本。
/// 参数：host为共享服务，config为Office配置，team为固定办公室。
/// 返回：已保存的规范化完整快照。
/// 错误：账本/SQL、容量或seq溢出错误。
/// 状态：不变内容保持原seq和时间；新内容先保存再返回。
/// 并发与取消：所有读取/保存通过同一Store命令，不读取自己HTTP接口。
/// 权限：只读取namespace=office及team绑定。
/// 关联：需求/设计REQ-OP-011/012；DES-OP-007/009。
pub(super) async fn prepare(host: &Host, config: &OfficeConfig, team: &str) -> HostResult<Value> {
    let team = team.to_owned();
    let name = config
        .names
        .get(&team)
        .cloned()
        .unwrap_or_else(|| team.clone());
    host.inner.store.call(move|db|{
        let previous=db.office_projection(&team)?;
        let prior=previous.as_ref().map(|(v,_)|v);
        let mut old=BTreeMap::new();if let Some(a)=prior.and_then(|v|v["tasks"].as_array()){for t in a {if let Some(id)=t["id"].as_str(){old.insert(id.to_owned(),t.clone());}}}
        let bindings:Vec<_>=db.bindings()?.into_iter().filter(|b|b.scope.namespace=="office"&&b.scope.slot_id==team).collect();
        let mut runs:Vec<_>=db.runs()?.into_iter().filter(|r|bindings.iter().any(|b|b.session_id==r.session_id)).collect();
        runs.sort_by_key(|r|(r.state.is_terminal(),std::cmp::Reverse(r.revision)));
        let now=chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis,true);
        let mut tasks=Vec::new();let mut events=Vec::new();
        for r in runs.iter().take(24) {
            let r=db.get_run(&r.run_id)?;
            let ledger=db.events(&r.session_id)?;
            let title=ledger.iter().position(|e|matches!(e,openpaw_core::SessionEvent::RunStarted{run_id} if run_id==&r.run_id))
                .and_then(|i|ledger[..i].iter().rev().find_map(|e|if let openpaw_core::SessionEvent::Message(m)=e {if m.role==openpaw_core::Role::User{return Some(m.text());}None}else{None})).unwrap_or_else(||"已接受的对话请求".into());
            let main_id=format!("run:{}",r.run_id);
            let summary=match r.state {RunState::Cancelled=>"用户取消，清理已完成",RunState::Interrupted=>"Host重启中断，不自动重放",RunState::TimedOut=>"预算到期，清理已完成",RunState::Cancelling=>"正在取消，等待实际资源清理",RunState::Failed=>"运行失败；完整原因保存在会话账本",_=>"由OpenPaw真实运行驱动；完整历史保存在原会话账本"};
            tasks.push(task(main_id.clone(),title,"assistant",status(r.state),summary.into(),&old,&now));
            for t in r.tools.iter().take(12) {
                // Provider call ID可能过长且后续排序变化；持久配对映射稳定短展示ID。
                let id=db.office_call_id(&r.run_id,&t.call_id)?;let (who,_)=actor(&t.tool_name);
                let item=task(id.clone(),format!("{} · {}",t.tool_name,dto::bounded(&t.call_id,40)),who,tool_status(&r,&t.state),format!("真实调用阶段：{}；归属Run {}",t.state,r.run_id),&old,&now);
                if matches!(t.state.as_str(),"running"|"completed"|"failed")&&who!="assistant" {
                    let time=item["updatedAt"].clone();
                    let existing=prior.and_then(|v|v["events"].as_array()).and_then(|a|a.iter().find(|e|e["id"]==format!("assign:{id}")));
                    events.push(existing.cloned().unwrap_or_else(||json!({"id":format!("assign:{id}"),"kind":"task.assigned","summary":format!("交由{}处理{}",who,t.tool_name),"occurredAt":time,"taskId":id,"employeeId":who,"fromEmployeeId":"assistant"})));
                }
                tasks.push(item);
            }
        }
        events.sort_by(|a,b| b["occurredAt"].as_str().cmp(&a["occurredAt"].as_str()).then_with(||a["id"].as_str().cmp(&b["id"].as_str())));
        let selected=tasks.first().cloned();
        let mut snapshot=json!({"schemaVersion":"1.0","seq":prior.map(|v|v["seq"].clone()).unwrap_or(json!(1)),"workspace":{"id":team,"name":name},
            "employees":[{"id":"assistant","name":"OpenPaw助手","sceneActorId":"office-lead","role":"真实模型对话与协调","online":true},{"id":"files","name":"文件读取","sceneActorId":"file-agent","role":"同一引擎的只读文件工具","online":true},{"id":"search","name":"项目检索","sceneActorId":"data-agent","role":"同一引擎的检索工具","online":true}],
            "tasks":tasks,"events":events,"system":{"cpuPercent":-1,"memoryPercent":-1,"networkKbps":-1},
            "whiteboard":{"template":"task-board-v1","title":selected.as_ref().map(|t|t["title"].as_str().unwrap_or("")).unwrap_or("等待对话"),"status":selected.as_ref().map(|t|t["status"].as_str().unwrap_or("queued")).unwrap_or("queued"),"tagline":"OpenPaw真实运行","description":"任务及人物状态来自真实Run和工具事实。","note":"仅展示有界近期任务，完整历史保存在OpenPaw。","link":{"label":"","url":""},"steps":selected.as_ref().map(|t|vec![json!({"id":"run","title":"当前请求处理","progress":t["progress"]})]).unwrap_or_default()}});
        // 完整请求按UTF-8计量；裁剪只影响展示，引用保持闭包，绝不发送局部快照。
        while serde_json::to_vec(&json!({"type":"office.snapshot","teamId":team,"snapshot":snapshot})).map_err(HostError::storage)?.len()>64000 {
            let popped=snapshot["tasks"].as_array_mut().and_then(|a|a.pop()).ok_or_else(||HostError::new("protocol_error","Office完整快照超限"))?;
            snapshot["events"].as_array_mut().unwrap().retain(|e|e["taskId"]!=popped["id"]);
        }
        if prior==Some(&snapshot) {return Ok(snapshot);}
        if let Some(prior)=prior {
            let next=prior["seq"].as_u64().filter(|n|*n<9007199254740991).ok_or_else(||HostError::new("sequence_conflict","Office序号已达安全整数上限"))?+1;
            snapshot["seq"]=json!(next);
        }
        let state=if previous.as_ref().is_some_and(|(_,s)|s=="halted"){ "halted" }else{"pending"};
        db.save_office_projection(&team,&snapshot,state)?;
        Ok(snapshot)
    }).await
}
async fn sync_once(
    host: &Host,
    config: &OfficeConfig,
    team: &str,
    client: &Client,
    halted: bool,
) -> HostResult<String> {
    let snapshot = prepare(host, config, team).await?;
    let key = team.to_owned();
    let saved_halt = host
        .inner
        .store
        .call(move |db| {
            Ok(db
                .office_projection(&key)?
                .is_some_and(|(_, s)| s == "halted"))
        })
        .await?;
    if halted || saved_halt {
        return Ok("halted".into());
    }
    let caps = client.capabilities().await?;
    for actor in ["office-lead", "file-agent", "data-agent"] {
        if !caps["scene"]["actors"]
            .as_array()
            .is_some_and(|a| a.iter().any(|a| a["id"] == actor))
        {
            return Err(HostError::new(
                "protocol_error",
                "Office缺少配置映射所需人物",
            ));
        }
    }
    let (_, directory) = client.post(json!({"type":"office.read"})).await?;
    let initialized = directory["offices"]
        .as_array()
        .is_some_and(|a| a.iter().any(|o| o["id"] == team));
    if !initialized {
        client
            .post(json!({"type":"offices.initialize","offices":[snapshot]}))
            .await?;
    }
    let (_, receipt) = client
        .post(json!({"type":"office.snapshot","teamId":team,"snapshot":snapshot}))
        .await?;
    let state = receipt["status"].as_str().unwrap_or("pending").to_owned();
    if state == "ignored" && receipt["reasonCode"] == "STALE_SEQ" {
        return Err(HostError::new(
            "sequence_conflict",
            "Office存在较新版本，停止投递以免覆盖外部所有者",
        ));
    }
    let team = team.to_owned();
    let saved_state = state.clone();
    host.inner
        .store
        .call(move |db| {
            if db
                .office_projection(&team)?
                .is_some_and(|(current, _)| current == snapshot)
            {
                db.save_office_projection(&team, &snapshot, &saved_state)?;
            }
            Ok(())
        })
        .await?;
    Ok(state)
}
