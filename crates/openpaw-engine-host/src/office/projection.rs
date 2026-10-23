//! 职责：将共享Run/call事实投影成完整Office快照并可靠同步。
//! 输入输出：原JSONL和Host状态到持久seq/完整内容及官方消息。
//! 依赖：原Store、官方client、单办公室协调任务。
//! 禁止职责：不伪造进度、工具开始或多Agent，不复制会话账本。
use super::{client::Client, dto, OfficeConfig};
use crate::{ClientScope, Host, HostError, HostResult, RunState};
use serde_json::{json, Value};
use std::{collections::BTreeMap, time::Duration};
use tokio_util::sync::CancellationToken;

/// 用途：持有Office同步任务；关闭时在Store之前完成最终投影。
/// 字段：停止token、任务、只读配置和有界会话恢复通知。
/// 来源去向：HostServer启动到Host关闭。
/// 约束：网络失败不改变Run结果，冲突停止自动投递。
/// 稳定性：T4内部生命周期。
/// 关联：需求/设计REQ-OP-011/012；DES-OP-009。
pub(crate) struct Coordinator {
    stop: CancellationToken,
    tasks: Vec<tokio::task::JoinHandle<()>>,
    config: OfficeConfig,
    address: std::net::SocketAddr,
    ui_refresh: tokio::sync::broadcast::Sender<ClientScope>,
}
impl Coordinator {
    /// 功能：为每个授权办公室启动独立投影和同步循环。
    /// 参数：host为唯一运行服务，config为显式Office配置，address为实际回环监听地址。
    /// 返回：同步任务所有者。
    /// 错误：凭据或客户端初始化错误立即拒绝启用。
    /// 状态：每个办公室先持久投影再发送；同seq重试不改变内容。
    /// 并发与取消：每办公室一个worker，不持资源锁等待网络。
    /// 权限：只读取配置授权槽，人物先验证capabilities。
    /// 关联：需求/设计REQ-OP-011/012；DES-OP-007/009。
    pub(crate) fn start(
        host: Host,
        config: OfficeConfig,
        address: std::net::SocketAddr,
    ) -> HostResult<Self> {
        let client = std::sync::Arc::new(Client::new(&config)?);
        let stop = CancellationToken::new();
        let (ui_refresh, _) = tokio::sync::broadcast::channel(32);
        let mut tasks = Vec::new();
        for team in host.inner.config.slots.keys() {
            let team = team.clone();
            let host = host.clone();
            let config = config.clone();
            let token = stop.clone();
            let client = client.clone();
            tasks.push(tokio::spawn(async move {

                let mut previous_status=String::new();let mut halted=false;let mut delay=1;
                loop {
                    if token.is_cancelled(){break;}
                    let result=tokio::select!{_=token.cancelled()=>break,result=sync_once(&host,&config,&team,&client,halted,address)=>result};
                    let status=match result {Ok(s)=>{delay=1;s},Err(e)=>{if matches!(e.kind.as_str(),"sequence_conflict"|"protocol_error"|"office_auth"){
                        halted=true;let team=team.clone();
                        let _=host.inner.store.call(move|db|{if let Some((snapshot,_))=db.office_projection(&team)?{db.save_office_projection(&team,&snapshot,"halted")?;}Ok(())}).await;
                    }delay=(delay*2).min(30);e.kind}};
                    if status!=previous_status {eprintln!("engine-host Office {team}: {status}");previous_status=status;}
                    tokio::select!{_=token.cancelled()=>break,_=tokio::time::sleep(Duration::from_secs(delay))=>{}}
                }
            }));
        }
        tasks.push(tokio::spawn(super::approvals::run(
            host.clone(),
            client.clone(),
            ui_refresh.clone(),
            address,
            stop.clone(),
        )));
        tasks.push(tokio::spawn(super::actions::run(
            host.clone(),
            client.clone(),
            config.clone(),
            address,
            stop.clone(),
        )));
        tasks.push(tokio::spawn(super::scene::run(host, client, stop.clone())));
        Ok(Self {
            stop,
            tasks,
            config,
            address,
            ui_refresh,
        })
    }
    /// 功能：通知成功恢复聊天的会话需要重新展示待审批菜单。
    /// 参数：scope为已经轮次服务校验的完整作用域。
    /// 返回：无。
    /// 错误：没有接收器时忽略，原审批仍由原期限管理。
    /// 状态：不创建审批、Run或持久化事件。
    /// 并发与取消：非阻塞广播；其他作用域的展示忽略该通知。
    /// 权限：仅恢复展示，不改变输入、权限或审批状态。
    /// 关联：REQ-OP-013；DES-OP-008。
    pub(crate) fn refresh_ui(&self, scope: &ClientScope) {
        let _ = self.ui_refresh.send(scope.clone());
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
            if super::workspaces::active(host, &self.config, team).await? {
                prepare(host, &self.config, team).await?;
            }
        }
        let client = Client::new(&self.config)?;
        let flush = async {
            for team in host.inner.config.slots.keys() {
                let _ = sync_once(host, &self.config, team, &client, false, self.address).await;
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
        "read_file" | "write_file" | "edit_file" => ("files", "file-agent"),
        "glob" | "grep" | "web_search" | "web_fetch" => ("search", "data-agent"),
        "bash" => ("command", "code-agent"),
        _ => ("assistant", "office-lead"),
    }
}
fn task(
    id: String,
    title: String,
    state: &str,
    summary: String,
    old: &BTreeMap<String, Value>,
    now: &str,
    progress: u64,
) -> Value {
    let mut task = json!({"id":id,"title":dto::bounded(&title,160),"assigneeId":"assistant","status":state,"progress":progress,"summary":dto::bounded(&summary,1000),"updatedAt":now});
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
    let command_enabled = host.inner.config.tools.iter().any(|name| name == "bash");
    host.inner.store.call(move|db|{
        let name = db.office_creations()?.iter().find(|v|v["slot_id"]==team)
            .and_then(|v|v["name"].as_str()).map(str::to_owned).unwrap_or(name);
        let previous=db.office_projection(&team)?;
        let prior=previous.as_ref().map(|(v,_)|v);
        let bindings:Vec<_>=db.bindings()?.into_iter().filter(|b|b.scope.namespace=="office"&&b.scope.slot_id==team).collect();
        let all_runs=db.runs()?;
        let now=chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis,true);
        let mut conversations=Vec::new();
        let mut priorities=BTreeMap::new();
        for binding in bindings {
            let mut runs=Vec::new();
            for r in all_runs.iter().filter(|r|r.session_id==binding.session_id) {
                let (offset,input)=db.run_input(r)?;
                if !input.is_empty(){runs.push((offset,r,input));}
            }
            runs.sort_by_key(|(offset,_,_)|*offset);
            let Some((_,latest,_))=runs.last() else {continue;};
            let title=dto::bounded(runs[0].2.trim(),40);
            let r=db.get_run(&latest.run_id)?;
            let prior_conv=prior.and_then(|v|v["conversations"].as_array()).and_then(|a|a.iter().find(|c|c["id"]==binding.scope.conversation_id));
            let mut old=BTreeMap::new();
            if let Some(a)=prior_conv.and_then(|v|v["tasks"].as_array()){for t in a {if let Some(id)=t["id"].as_str(){old.insert(id.to_owned(),t.clone());}}}
            // 每会话只投影最新Run，避免旧失败抢占最新成功的员工状态；完整历史走history。
            let (_,input)=db.run_input(&r)?;
            let main_id=format!("run:{}",r.run_id);
            let (steps,percent)=crate::task_progress::project(db.progress(&r.run_id)?,&r)?;
            let summary=match r.state {
                RunState::Cancelled=>"已停止，实际操作已清理".to_owned(),
                RunState::Interrupted=>"处理被中断，已完成事项保留".to_owned(),
                RunState::TimedOut|RunState::Failed=>super::turns::message(&r),
                RunState::Cancelling=>"正在停止，等待实际操作清理".to_owned(),
                RunState::Completed if percent<100=>"回复已结束，仍有步骤未完成".to_owned(),
                RunState::Completed=>"本次目标已完成".to_owned(),
                _ if r.tools.iter().any(|t|t.state=="awaiting_approval")=>"等待你确认本次操作".to_owned(),
                _ if r.tools.iter().any(|t|t.state=="awaiting_resource")=>"等待可用资源".to_owned(),
                _=>"正在处理你的请求".to_owned(),
            };
            let main_state=if r.state==RunState::Completed && percent<100 {"failed"} else if !r.state.is_terminal() && (r.tools.iter().any(|t|matches!(t.state.as_str(),"awaiting_approval"|"awaiting_resource")) || r.state==RunState::Cancelling) {"blocked"}else{status(r.state)};
            let mut tasks=vec![task(main_id,dto::bounded(&input,80),main_state,summary.clone(),&old,&now,percent)];
            for step in steps {
                tasks.push(task(step["id"].as_str().unwrap().into(),step["title"].as_str().unwrap().into(),step["status"].as_str().unwrap(),step["summary"].as_str().unwrap().into(),&old,&now,step["progress"].as_u64().unwrap()));
            }
            let mut events=Vec::new();
            let assignments=db.office_task_assignments(&r.run_id)?;
            // 工具仍是执行事实，公开任务只引用稳定的业务步骤；重试不增加卡片或重播交接。
            for t in r.tools.iter().filter(|t|t.tool_name!="report_progress") {
                // 保留原内部调用关联号供历史详情追踪，公开任务不再使用逐call卡片。
                let _call_reference=db.office_call_id(&r.run_id,&t.call_id)?;
                let (who,_)=actor(&t.tool_name);
                if who=="assistant" || !db.office_call_started(&r.run_id,&t.call_id)? {continue;}
                let current=assignments.get(&t.call_id).map(|i|i+1).filter(|i|*i<tasks.len()).unwrap_or(0);
                let id=tasks[current]["id"].as_str().unwrap().to_owned();
                let event_id=format!("assign:{id}:{who}");
                if events.iter().any(|e:&Value|e["id"]==event_id) {continue;}
                if !r.state.is_terminal() && matches!(t.state.as_str(),"running"|"awaiting_resource") {
                    tasks[current]["assigneeId"]=json!(who);
                }
                let label=match who {"files"=>"文件操作","search"=>"资料查询",_=>"执行操作"};
                let existing=prior_conv.and_then(|v|v["events"].as_array()).and_then(|a|a.iter().find(|e|e["id"]==event_id));
                events.push(existing.cloned().unwrap_or_else(||json!({"id":event_id,"kind":"task.assigned","summary":format!("开始{label}"),"occurredAt":tasks[current]["updatedAt"],"taskId":id,"employeeId":who,"fromEmployeeId":"assistant"})));
            }
            // assignee也是任务内容；完成全部映射后再比较，避免同内容每秒改变updatedAt/seq。
            for task in &mut tasks {
                if let Some(prior)=old.get(task["id"].as_str().unwrap()) {
                    let mut comparable=prior.clone();comparable["updatedAt"]=task["updatedAt"].clone();
                    if &comparable==task {task["updatedAt"]=prior["updatedAt"].clone();}
                }
            }
            events.sort_by(|a,b|b["occurredAt"].as_str().cmp(&a["occurredAt"].as_str()).then_with(||a["id"].as_str().cmp(&b["id"].as_str())));
            let selected=&tasks[0];
            let board_steps:Vec<_>=if tasks.len()>1 {
                let first=tasks[1..].iter().position(|t|t["status"]!="completed").unwrap_or(tasks.len().saturating_sub(4));
                tasks[1..].iter().skip(first).take(3).map(|t| {
                    let label=match t["status"].as_str(){Some("completed")=>"已完成",Some("running")=>"处理中",Some("blocked")=>"等待确认",Some("failed")=>"未完成",_=>"待处理"};
                    json!({"id":t["id"],"title":dto::bounded(&format!("{} · {label}",t["title"].as_str().unwrap()),80),"progress":t["progress"]})
                }).collect()
            } else {vec![json!({"id":"goal","title":dto::bounded(&summary,80),"progress":percent})]};
            let completed=tasks.iter().skip(1).filter(|t|t["status"]=="completed").count();
            let description=if tasks.len()>1 {format!("已确认完成 {completed}/{} 步；{summary}",tasks.len()-1)}else{summary};
            let whiteboard=json!({"template":"task-board-v1","title":selected["title"],"status":selected["status"],"tagline":"","description":dto::bounded(&description,500),"note":"","link":{"label":"","url":""},"steps":board_steps});
            priorities.insert(binding.scope.conversation_id.clone(),r.revision);
            let mut employees=json!([{"id":"assistant","name":"OpenPaw助手","sceneActorId":"office-lead","role":"任务处理与协调","online":true},{"id":"files","name":"文件操作","sceneActorId":"file-agent","role":"文件查看与记录，修改前确认","online":true},{"id":"search","name":"项目检索","sceneActorId":"data-agent","role":"项目检索与资料查询","online":true}]);
            if command_enabled {employees.as_array_mut().unwrap().push(json!({"id":"command","name":"命令执行","sceneActorId":"code-agent","role":"执行操作，开始前确认","online":true}));}
            conversations.push(json!({"id":binding.scope.conversation_id,"title":title,
                "employees":employees,
                "tasks":tasks,"events":events,"whiteboard":whiteboard}));
        }
        conversations.sort_by(|a,b|a["id"].as_str().cmp(&b["id"].as_str()));
        let mut snapshot=json!({"schemaVersion":"1.0","seq":prior.map(|v|v["seq"].clone()).unwrap_or(json!(1)),"workspace":{"id":team,"name":name},"conversations":conversations,"system":{"cpuPercent":-1,"memoryPercent":-1,"networkKbps":-1}});
        // 整办公室64KiB预算；先缩工具，再缩旧终态详情，始终保留全部会话ID和标题。
        while serde_json::to_vec(&json!({"type":"office.snapshot","teamId":team,"snapshot":snapshot})).map_err(HostError::storage)?.len()>64000 {
            let convs=snapshot["conversations"].as_array_mut().unwrap();
            if let Some(c)=convs.iter_mut().rev().find(|c|c["tasks"].as_array().is_some_and(|a|a.len()>1)){
                let popped=c["tasks"].as_array_mut().unwrap().pop().unwrap();
                c["events"].as_array_mut().unwrap().retain(|e|e["taskId"]!=popped["id"]);
                continue;
            }
            let candidate=convs.iter().enumerate().filter(|(_,c)|c["tasks"].as_array().is_some_and(|a|!a.is_empty())&&matches!(c["tasks"][0]["status"].as_str(),Some("completed"|"failed"))).min_by_key(|(_,c)|priorities.get(c["id"].as_str().unwrap_or("")));
            let Some((i,_))=candidate else {return Err(HostError::new("protocol_error","Office完整会话目录超限"));};
            let c=&mut convs[i];c["tasks"]=json!([]);c["events"]=json!([]);c["employees"]=json!([]);
            c["whiteboard"]=json!({"template":"task-board-v1","title":"","status":"queued","tagline":"","description":"","note":"","link":{"label":"","url":""},"steps":[]});
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
/// 功能：按办公室唯一外发门同步完整快照并核对回执。
/// 参数：host/config/team/client为既有服务，halted为冲突锁存，address为聊天地址。
/// 返回：真实applied、cached、ignored或halted状态。
/// 错误：连接、认证、版本冲突及协议错误，不假定服务端回滚。
/// 状态：先持久完整内容再外发，重复序号保持同内容。
/// 并发与取消：办公室专用门跨HTTP等待，不阻塞共享运行注册表。
/// 权限：只访问已配置team，服务凭据不下发浏览器。
/// 关联：需求/设计REQ-OP-011/013/014；CHG-2026-0006。
pub(super) async fn sync_once(
    host: &Host,
    config: &OfficeConfig,
    team: &str,
    client: &Client,
    halted: bool,
    address: std::net::SocketAddr,
) -> HostResult<String> {
    let gate = client.projection_gate(team).await;
    let _guard = gate.lock().await;
    if !super::workspaces::active(host, config, team).await? {
        return Ok("reserved".into());
    }
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
    let mut actors = vec!["office-lead", "file-agent", "data-agent"];
    if host.inner.config.tools.iter().any(|name| name == "bash") {
        actors.push("code-agent");
    }
    for actor in actors {
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
            .post(json!({"type":"offices.initialize","offices":[snapshot],"chat":{"url":format!("http://{address}/office/chat")}}))
            .await?;
    }
    // chat.url是Office进程内状态；重新注册幂等，Node重启后不依赖浏览器重建。
    if initialized {
        client.post(json!({"type":"offices.initialize","offices":[],"chat":{"url":format!("http://{address}/office/chat")}})).await?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ClientScope, HostConfig};
    #[tokio::test]
    async fn legacy_snapshot_upgrades_with_new_seq_and_full_catalogue_is_bounded() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        std::fs::create_dir(&root).unwrap();
        let config:HostConfig=serde_json::from_value(json!({"home":temp.path().join("home"),"roots":{"r":root},"slots":{"office-1":"r"},"provider":"echo"})).unwrap();
        let host = Host::start(config).await.unwrap();
        let office = OfficeConfig {
            base_url: "http://127.0.0.1:9".into(),
            api_key_env: "PAWOFFICE_TEST_KEY".into(),
            allowed_origins: vec!["http://127.0.0.1:8765".into()],
            names: Default::default(),
            creation_slots: vec![],
        };
        host.inner.store.call(|db|db.save_office_projection("office-1",&json!({"schemaVersion":"1.0","seq":77,"workspace":{"id":"office-1","name":"旧版"},"employees":[],"tasks":[],"events":[],"whiteboard":{}}),"cached")).await.unwrap();
        let upgraded = prepare(&host, &office, "office-1").await.unwrap();
        assert_eq!(upgraded["seq"], 78);
        assert!(upgraded.get("employees").is_none());
        assert_eq!(upgraded["conversations"], json!([]));
        assert_eq!(prepare(&host, &office, "office-1").await.unwrap(), upgraded);
        for i in 0..100 {
            let scope = ClientScope {
                namespace: "office".into(),
                slot_id: "office-1".into(),
                conversation_id: format!("conv-{i:03}"),
            };
            let b = host.open_session(scope.clone()).await.unwrap();
            let r = host
                .submit_run(
                    &scope,
                    &b.session_id,
                    format!("{i}：{}", "中文长请求".repeat(30)),
                )
                .await
                .unwrap();
            loop {
                if host
                    .get_run(&scope, &r.run_id)
                    .await
                    .unwrap()
                    .state
                    .is_terminal()
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        }
        let snapshot = prepare(&host, &office, "office-1").await.unwrap();
        assert_eq!(snapshot["conversations"].as_array().unwrap().len(), 100);
        assert!(
            serde_json::to_vec(
                &json!({"type":"office.snapshot","teamId":"office-1","snapshot":snapshot})
            )
            .unwrap()
            .len()
                <= 64000
        );
        assert_eq!(
            prepare(&host, &office, "office-1").await.unwrap(),
            snapshot,
            "裁剪后的内容必须稳定，不能每秒改变seq"
        );
        let scope = ClientScope {
            namespace: "office".into(),
            slot_id: "office-1".into(),
            conversation_id: "overflow".into(),
        };
        assert_eq!(
            host.open_session(scope).await.unwrap_err().kind,
            "capacity_exceeded"
        );
        host.shutdown().await.unwrap();
    }
}
