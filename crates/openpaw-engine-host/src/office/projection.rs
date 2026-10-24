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
    /// 关联：需求REQ-OP-013；设计DES-OP-008。
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
/// 功能：将真实工具名称映射到可信成员与场景人物。
/// 参数：tool为已经由Host记录的工具名称。
/// 返回：成员ID与Office人物ID；未知工具归属主角色。
/// 错误：无；未知工具不会扩大权限。
/// 状态：纯只读映射，不创建任务或Run。
/// 并发与取消：无锁、无等待、不可取消。
/// 权限：返回值仅用于有界展示，不能作为授权依据。
/// 关联：需求/设计REQ-OP-021/023；DES-OP-011。
pub(super) fn actor(tool: &str) -> (&'static str, &'static str) {
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
    let employees:Vec<_>=crate::agents::ROLES.iter().map(|role|json!({"id":role.employee,"name":role.name,"sceneActorId":role.actor,"role":role.duty,"online":crate::agents::enabled(role,&host.inner.config)})).collect();
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
        let mut terminal_conversations=std::collections::BTreeSet::new();
        for binding in bindings {
            let mut runs=Vec::new();
            for r in all_runs.iter().filter(|r|r.session_id==binding.session_id) {
                let (offset,input)=db.run_input(r)?;
                if !input.is_empty(){runs.push((offset,r,input));}
            }
            runs.sort_by_key(|(offset,_,_)|*offset);
            let Some((_,latest,_))=runs.last() else {continue;};
            let title=dto::bounded(runs[0].2.trim(),40);
            let prior_conv=prior.and_then(|v|v["conversations"].as_array()).and_then(|a|a.iter().find(|c|c["id"]==binding.scope.conversation_id));
            let mut old=BTreeMap::new();
            if let Some(a)=prior_conv.and_then(|v|v["tasks"].as_array()){for t in a {if let Some(id)=t["id"].as_str(){old.insert(id.to_owned(),t.clone());}}}
            // 当前轮次和至多两组近期真实业务保留独立身份，普通追问不擦除团队成果。
            // 不复制历史任务库：每次从原Run/计划/委派重建，容量不足仍按原预算裁剪。
            let mut display_ids=vec![latest.run_id.clone()];
            for (_,candidate,_) in runs.iter().rev() {
                if display_ids.len()>=3 {break;}
                if candidate.run_id==latest.run_id {continue;}
                if db.progress(&candidate.run_id)?.is_some() || !db.delegations(&candidate.run_id)?.is_empty() {
                    display_ids.push(candidate.run_id.clone());
                }
            }
            let latest_has_work=db.progress(&latest.run_id)?.is_some() || !db.delegations(&latest.run_id)?.is_empty() || latest.tools.iter().any(|t|t.tool_name!="report_progress");
            // 新业务事实出现后才接管白板；普通追问等待Provider时也保留原业务成果。
            // 当前Run状态仍由独立任务卡展示，无旧业务时自然回退到当前目标。
            let board_id=if latest_has_work {&display_ids[0]} else {display_ids.get(1).unwrap_or(&display_ids[0])};
            let mut retained_tasks=Vec::new();
            let mut retained_events=Vec::new();
            let mut retained_whiteboard=Value::Null;
            for run_id in &display_ids {
            let r=db.get_run(run_id)?;
            let (_,input)=db.run_input(&r)?;
            let main_id=format!("run:{}",r.run_id);
            let plan=db.progress(&r.run_id)?;
            let (steps,mut percent)=crate::task_progress::project(plan.clone(),&r)?;
            let delegations=db.delegations(&r.run_id)?;
            let mut assignments=db.office_task_assignments(&r.run_id)?;
            // 开始时多个连续委派可能暂挂同一步；主角色后来用准确证据核验不同
            // 业务步骤时，展示引用采用唯一明确证据，不能再生一张重复委派卡。
            // 原Store开始关联不改，assign事件ID/首次时间不改，细化taskId不重播拜访。
            for d in &delegations {
                let confirmed:Vec<_>=plan.as_ref().and_then(|p|p["steps"].as_array()).into_iter().flatten().enumerate().filter(|(_,step)|step["state"]=="completed"&&step["evidenceCallIds"].as_array().is_some_and(|ids|ids.iter().any(|id|id==&d.call_id))).map(|(index,_)|index).collect();
                if let [index]=confirmed.as_slice() {assignments.insert(d.call_id.clone(),*index);}
            }
            let mut recovered=std::collections::BTreeSet::new();
            // 只合并被主角色用准确成功证据替代的失败尝试；相似标题不构成重试关系。
            // 同角色、同父、同步骤缺一不可，旧Run/JSONL/派发事件仍保留。
            for d in &delegations {
                let Some(index)=assignments.get(&d.call_id).copied() else {continue;};
                let Some(step)=plan.as_ref().and_then(|p|p["steps"].get(index)) else {continue;};
                if step["state"]!="completed" {continue;}
                let failed=match db.get_run(&d.child_run) {Ok(c)=>c.state.is_terminal()&&c.state!=RunState::Completed,Err(e) if e.kind=="not_found"=>false,Err(e)=>return Err(e)};
                if !failed {continue;}
                for replacement in &delegations {
                    if replacement.role_id!=d.role_id || assignments.get(&replacement.call_id)!=Some(&index) || !step["evidenceCallIds"].as_array().is_some_and(|ids|ids.iter().any(|id|id==&replacement.call_id)) {continue;}
                    if db.get_run(&replacement.child_run)?.state==RunState::Completed {
                        recovered.insert(d.call_id.clone());break;
                    }
                }
            }
            let mut child_steps=BTreeMap::new();
            let mut child_tasks=Vec::new();
            let mut child_events=Vec::new();
            let mut completed_children=0u64;
            for d in &delegations {
                let role=crate::agents::role(&d.role_id).ok_or_else(||HostError::new("storage_unavailable","委派引用未知角色"))?;
                let child=match db.get_run(&d.child_run) {Ok(v)=>Some(v),Err(e) if e.kind=="not_found"=>None,Err(e)=>return Err(e)};
                let state=child.as_ref().map(|c| if !c.state.is_terminal() && (c.state==RunState::Cancelling || c.tools.iter().any(|t|matches!(t.state.as_str(),"awaiting_approval"|"awaiting_resource"))) {"blocked"}else{status(c.state)}).unwrap_or(if r.state.is_terminal(){"failed"}else{"queued"});
                let superseded=recovered.contains(&d.call_id);
                let done=state=="completed";
                completed_children+=u64::from(done);
                let summary=if let Some(c)=&child {
                    if done {dto::bounded(&c.text,180)} else if c.state.is_terminal() {"子任务未完成，已保存真实结果".into()}
                    else if state=="blocked" {"等待确认、资源或清理".into()} else {"正在独立处理此任务".into()}
                } else if r.state.is_terminal() {"委派中断，未接受执行；不会自动重放".into()} else {"已登记委派，等待接受执行".into()};
                let mut item=task(d.task_id.clone(),d.title.clone(),state,summary,&old,&now,if done{100}else{0});
                item["assigneeId"]=json!(role.employee);
                // 派发时已有明确进行中步骤则复用其业务卡片；不按相似标题猜测关联。
                let linked=assignments.get(&d.call_id).copied().filter(|i|*i<steps.len()&&(superseded||!child_steps.contains_key(i)));
                if let Some(index)=linked {
                    let step=&steps[index];
                    item["id"]=step["id"].clone();item["title"]=step["title"].clone();
                    if done && step["status"]!="completed" {
                        item["status"]=step["status"].clone();item["progress"]=step["progress"].clone();
                        item["summary"]=json!("子执行已结束，业务步骤仍待主角色核验");
                    }
                }
                let task_id=item["id"].clone();
                if let Some(c)=&child {
                    // 生命周期动态复用原完整快照保存首次观察时间，不伪造错过的开始时刻。
                    // 业务核验与子Run结束分开；只有assignment能驱动交接动画。
                    let phases=[
                        ("started","task.started",c.state==RunState::Running,"观察到独立执行"),
                        ("blocked","task.blocked",!c.state.is_terminal()&&state=="blocked","曾观察到等待确认、资源或清理"),
                        ("completed","task.completed",!superseded&&item["status"]=="completed","已核验完成"),
                        ("result","note",done&&item["status"]!="completed","独立执行已结束；业务步骤须由主角色核验"),
                        ("incomplete","note",c.state.is_terminal()&&!done,"此尝试未完成；真实结果保留在成员历史"),
                    ];
                    for (phase,kind,observed,label) in phases {
                        let id=format!("activity:{}:{phase}",d.task_id);
                        let existing=prior_conv.and_then(|v|v["events"].as_array()).and_then(|events|events.iter().find(|event|event["id"]==id));
                        if !observed&&existing.is_none() {continue;}
                        child_events.push(json!({"id":id,"kind":kind,"summary":dto::bounded(&format!("{label}：{}",d.title),300),"occurredAt":existing.map(|event|event["occurredAt"].clone()).unwrap_or_else(||json!(now)),"taskId":task_id,"employeeId":role.employee}));
                    }
                }
                if !superseded {
                    if let Some(index)=linked {child_steps.insert(index,item);} else {child_tasks.push(item);}
                }
                if child.is_some() {
                    child_events.push(json!({"id":format!("assign:{}",d.task_id),"kind":"task.assigned","summary":dto::bounded(&format!("交给{}：{}",role.name,d.title),300),"occurredAt":d.created_at,"taskId":task_id,"employeeId":role.employee,"fromEmployeeId":"assistant"}));
                }
            }
            if !delegations.is_empty() {
                let required_children=(delegations.len()-recovered.len()) as u64;
                let delivered=r.state==RunState::Completed && completed_children==required_children;
                let child_percent=(completed_children+u64::from(delivered))*100/(required_children+1);
                percent=if steps.is_empty() {child_percent}else{percent.min(child_percent)};
            }
            let child_blocked=child_tasks.iter().chain(child_steps.values()).any(|t|t["status"]=="blocked");
            let child_failed=child_tasks.iter().chain(child_steps.values()).any(|t|t["status"]=="failed");
            let summary=match r.state {
                RunState::Cancelled=>"已停止，实际操作已清理".to_owned(),
                RunState::Interrupted=>"处理被中断，已完成事项保留".to_owned(),
                RunState::TimedOut|RunState::Failed=>super::turns::message(&r),
                RunState::Cancelling=>"正在停止，等待实际操作清理".to_owned(),
                RunState::Completed if child_failed=>"回复已结束；仍有未恢复的子任务失败，已完成部分保留".to_owned(),
                RunState::Completed if percent<100=>"回复已结束；已完成部分保留，其余待核验或继续处理".to_owned(),
                RunState::Completed=>"本次目标已完成".to_owned(),
                _ if child_blocked=>"子任务等待确认或可用资源".to_owned(),
                _ if r.tools.iter().any(|t|t.state=="awaiting_approval")=>"等待你确认本次操作".to_owned(),
                _ if r.tools.iter().any(|t|t.state=="awaiting_resource")=>"等待可用资源".to_owned(),
                _=>"正在处理你的请求".to_owned(),
            };
            let main_state=if r.state==RunState::Completed && child_failed {"failed"} else if r.state==RunState::Completed && percent<100 || !r.state.is_terminal() && (child_blocked || r.tools.iter().any(|t|matches!(t.state.as_str(),"awaiting_approval"|"awaiting_resource")) || r.state==RunState::Cancelling) {"blocked"}else{status(r.state)};
            let mut tasks=vec![task(main_id,dto::bounded(&input,80),main_state,summary.clone(),&old,&now,percent)];
            for (index,step) in steps.into_iter().enumerate() {
                if let Some(child)=child_steps.remove(&index) {tasks.push(child);continue;}
                tasks.push(task(step["id"].as_str().unwrap().into(),step["title"].as_str().unwrap().into(),step["status"].as_str().unwrap(),step["summary"].as_str().unwrap().into(),&old,&now,step["progress"].as_u64().unwrap()));
            }
            tasks.extend(child_tasks);
            let mut events=child_events;
            // 工具仍是执行事实，公开任务只引用稳定的业务步骤；重试不增加卡片或重播交接。
            for t in r.tools.iter().filter(|t|t.tool_name!="report_progress") {
                // 保留原内部调用关联号供历史详情追踪，公开任务不再使用逐call卡片。
                let _call_reference=db.office_call_id(&r.run_id,&t.call_id)?;
                if !delegations.is_empty() {continue;} // 协作交接只来自真实委派，主角色核验工具不重复派发同一步。
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
                    json!({"id":t["id"],"title":dto::bounded(&format!("{} · {label}",dto::bounded(t["title"].as_str().unwrap(),24)),40),"progress":t["progress"]})
                }).collect()
            } else {vec![json!({"id":"goal","title":dto::bounded(&summary,80),"progress":percent})]};
            let completed=tasks.iter().skip(1).filter(|t|t["status"]=="completed").count();
            let description=if tasks.len()>1 {format!("已确认完成 {completed}/{} 步；{summary}",tasks.len()-1)}else{summary};
            let whiteboard=json!({"template":"task-board-v1","title":dto::bounded(selected["title"].as_str().unwrap_or(""),32),"status":selected["status"],"tagline":"","description":dto::bounded(&description,500),"note":"","link":{"label":"","url":""},"steps":board_steps});
            if run_id==board_id {retained_whiteboard=whiteboard;}
            retained_tasks.extend(tasks);
            retained_events.extend(events);
            }
            retained_events.sort_by(|a,b|b["occurredAt"].as_str().cmp(&a["occurredAt"].as_str()).then_with(||a["id"].as_str().cmp(&b["id"].as_str())));
            priorities.insert(binding.scope.conversation_id.clone(),latest.revision);
            if latest.state.is_terminal() {terminal_conversations.insert(binding.scope.conversation_id.clone());}
            // 常驻人物和成员来自同一可信目录，未启用角色也能说明真实状态。
            conversations.push(json!({"id":binding.scope.conversation_id,"title":title,
                "employees":employees,
                "tasks":retained_tasks,"events":retained_events,"whiteboard":retained_whiteboard}));
        }
        conversations.sort_by(|a,b|a["id"].as_str().cmp(&b["id"].as_str()));
        let mut snapshot=json!({"schemaVersion":"1.0","seq":prior.map(|v|v["seq"].clone()).unwrap_or(json!(1)),"workspace":{"id":team,"name":name},"conversations":conversations,"system":{"cpuPercent":-1,"memoryPercent":-1,"networkKbps":-1}});
        // 整办公室64KiB预算；按事实版本先缩旧终态详情，保留活动业务卡及全部成员。
        while serde_json::to_vec(&json!({"type":"office.snapshot","teamId":team,"snapshot":snapshot})).map_err(HostError::storage)?.len()>64000 {
            let convs=snapshot["conversations"].as_array_mut().unwrap();
            let candidate=convs.iter().enumerate().filter(|(_,c)|c["tasks"].as_array().is_some_and(|a|!a.is_empty())&&terminal_conversations.contains(c["id"].as_str().unwrap_or(""))).min_by_key(|(_,c)|priorities.get(c["id"].as_str().unwrap_or("")));
            let Some((i,_))=candidate else {return Err(HostError::new("protocol_error","Office完整会话目录超限"));};
            let c=&mut convs[i];
            // 会话ID的字典序不代表新旧；审批中的步骤不能被旧历史挤掉。
            if c["tasks"].as_array().is_some_and(|a|a.len()>1){
                let popped=c["tasks"].as_array_mut().unwrap().pop().unwrap();
                c["events"].as_array_mut().unwrap().retain(|e|e["taskId"]!=popped["id"]);
                continue;
            }
            c["tasks"]=json!([]);c["events"]=json!([]);
            // 成员身份是人物点击和侧栏的范围依据，不能随旧任务展示预算一起删除。
            // 可选职责/白板按上游Schema省略；详情仍从可信目录及原账本读取完整信息。
            c.as_object_mut().unwrap().remove("whiteboard");
            for member in c["employees"].as_array_mut().unwrap() {
                member.as_object_mut().unwrap().remove("role");
            }
            c["title"]=json!(dto::bounded(c["title"].as_str().unwrap_or(""),24));
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
    async fn consecutive_delegations_use_their_precise_completed_step_without_duplicate_cards() {
        use openpaw_llm::{MockMode, MockProvider};
        // 在真实Provider等待边界冻结运行，避免只测终态而遗漏用户可见的临时重置。
        struct Pausable {
            inner: MockProvider,
            pause: std::sync::atomic::AtomicBool,
            entered: tokio::sync::Notify,
            release: tokio::sync::Semaphore,
        }
        #[async_trait::async_trait]
        impl openpaw_core::LlmProvider for Pausable {
            async fn complete(
                &self,
                request: openpaw_core::CompletionRequest,
            ) -> openpaw_core::Result<openpaw_core::CompletionResponse> {
                self.inner.complete(request).await
            }
            async fn stream(
                &self,
                request: openpaw_core::CompletionRequest,
            ) -> openpaw_core::Result<
                futures::stream::BoxStream<
                    'static,
                    openpaw_core::Result<openpaw_core::StreamEvent>,
                >,
            > {
                if self.pause.swap(false, std::sync::atomic::Ordering::SeqCst) {
                    self.entered.notify_one();
                    self.release.acquire().await.unwrap().forget();
                }
                self.inner.stream(request).await
            }
            fn capabilities(&self) -> openpaw_core::Capabilities {
                self.inner.capabilities()
            }
            fn default_model(&self) -> &str {
                self.inner.default_model()
            }
            fn name(&self) -> &str {
                "controlled-projection-wait"
            }
        }
        for (second_evidence, expected_cards) in [("call_2_1", 3), ("call_2_0", 4)] {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().join("root");
            std::fs::create_dir(&root).unwrap();
            std::fs::write(root.join("brief.md"), "同一份真实简报").unwrap();
            let config: HostConfig = serde_json::from_value(json!({"home":temp.path().join("home"),"roots":{"r":root},"slots":{"office-1":"r"},"provider":"echo","tools":["read_file"],"agents":{"enabled":true}})).unwrap();
            let call = |name: &str, input| MockMode::ToolUse {
                tool_name: name.into(),
                input,
                text: String::new(),
            };
            let script = vec![
                call(
                    "report_progress",
                    json!({"type":"create","steps":[{"id":"research","title":"李研核验目标"},{"id":"organize","title":"周理整理清单"}]}),
                ),
                call(
                    "report_progress",
                    json!({"type":"update","stepId":"research","state":"in_progress"}),
                ),
                MockMode::ParallelToolUse {
                    tools: vec![
                        (
                            "delegate_task".into(),
                            json!({"role_id":"research","title":"李研读取简报","instructions":"读取brief.md核验目标","acceptance":"真实读取"}),
                        ),
                        (
                            "delegate_task".into(),
                            json!({"role_id":"organize","title":"周理读取简报","instructions":"读取brief.md整理清单","acceptance":"真实读取"}),
                        ),
                    ],
                    text: String::new(),
                },
                call("read_file", json!({"path":"brief.md"})),
                MockMode::Fixed("李研实际读取完成".into()),
                call("read_file", json!({"path":"brief.md"})),
                MockMode::Fixed("周理实际读取完成".into()),
                call(
                    "report_progress",
                    json!({"type":"update","stepId":"research","state":"completed","evidenceCallIds":["call_2_0"]}),
                ),
                call(
                    "report_progress",
                    json!({"type":"update","stepId":"organize","state":"completed","evidenceCallIds":[second_evidence]}),
                ),
                MockMode::Fixed("王明核验两个子任务完成".into()),
                MockMode::Fixed("普通追问已回复".into()),
                call(
                    "report_progress",
                    json!({"type":"create","steps":[{"id":"new","title":"独立新业务目标"}]}),
                ),
                MockMode::Fixed("新目标尚未核验".into()),
            ];
            let provider = std::sync::Arc::new(Pausable {
                inner: MockProvider::scripted(script),
                pause: std::sync::atomic::AtomicBool::new(true),
                entered: tokio::sync::Notify::new(),
                release: tokio::sync::Semaphore::new(0),
            });
            let host = Host::with_provider(config, provider.clone()).await.unwrap();
            let scope = ClientScope {
                namespace: "office".into(),
                slot_id: "office-1".into(),
                conversation_id: "consecutive".into(),
            };
            let binding = host.open_session(scope.clone()).await.unwrap();
            let office = OfficeConfig {
                base_url: "http://127.0.0.1:9".into(),
                api_key_env: "PAWOFFICE_TEST_KEY".into(),
                allowed_origins: vec!["http://127.0.0.1:8765".into()],
                names: Default::default(),
                creation_slots: vec![],
            };
            let run = host
                .submit_run(&scope, &binding.session_id, "核验目标并整理清单".into())
                .await
                .unwrap();
            tokio::time::timeout(
                std::time::Duration::from_secs(10),
                provider.entered.notified(),
            )
            .await
            .unwrap();
            let initial = prepare(&host, &office, "office-1").await.unwrap();
            assert_eq!(
                initial["conversations"][0]["whiteboard"]["status"],
                "running"
            );
            assert!(initial["conversations"][0]["whiteboard"]["title"]
                .as_str()
                .unwrap()
                .contains("核验目标"));
            provider.release.add_permits(1);
            tokio::time::timeout(std::time::Duration::from_secs(10), host.wait_idle())
                .await
                .unwrap();
            assert_eq!(
                host.get_run(&scope, &run.run_id).await.unwrap().state,
                RunState::Completed
            );

            let snapshot = prepare(&host, &office, "office-1").await.unwrap();
            let conv = &snapshot["conversations"][0];
            assert_eq!(
                conv["tasks"].as_array().unwrap().len(),
                expected_cards,
                "只有唯一准确证据才合并业务卡；同一call被多个步骤引用不能猜测归属"
            );
            assert_eq!(conv["tasks"][0]["progress"], 100);
            if expected_cards == 3 {
                for (index, employee) in ["command", "files"].into_iter().enumerate() {
                    assert_eq!(conv["tasks"][index + 1]["assigneeId"], employee);
                    assert_eq!(conv["tasks"][index + 1]["status"], "completed");
                    for event in conv["events"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .filter(|e| e["employeeId"] == employee)
                    {
                        assert_eq!(event["taskId"], conv["tasks"][index + 1]["id"]);
                    }
                }
            }
            assert_eq!(
                conv["events"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|e| e["kind"] == "task.assigned")
                    .count(),
                2
            );
            let run_id = run.run_id.clone();
            host.inner
                .store
                .call(move |db| {
                    let original = db.office_task_assignments(&run_id)?;
                    assert_eq!(original.get("call_2_0"), Some(&0));
                    assert_eq!(
                        original.get("call_2_1"),
                        Some(&0),
                        "原开始关联与执行事实不被展示细化改写"
                    );
                    let ds = db.delegations(&run_id)?;
                    assert_eq!(ds.len(), 2);
                    assert_ne!(ds[0].child_session, ds[1].child_session);
                    for d in ds {
                        assert_eq!(db.get_run(&d.child_run)?.state, RunState::Completed);
                    }
                    Ok(())
                })
                .await
                .unwrap();
            assert_eq!(
                prepare(&host, &office, "office-1").await.unwrap(),
                snapshot,
                "重复投影保持seq和派工ID，不再新增拜访"
            );
            if expected_cards == 3 {
                // 普通追问不能擦掉此前同事的真实工作、白板和成员详情。
                provider
                    .pause
                    .store(true, std::sync::atomic::Ordering::SeqCst);
                host.submit_run(&scope, &binding.session_id, "谢谢，请保留团队成果".into())
                    .await
                    .unwrap();
                tokio::time::timeout(
                    std::time::Duration::from_secs(10),
                    provider.entered.notified(),
                )
                .await
                .unwrap();
                let waiting = prepare(&host, &office, "office-1").await.unwrap();
                assert_eq!(
                    waiting["conversations"][0]["whiteboard"], conv["whiteboard"],
                    "普通追问仍在运行时也不能清空已完成业务白板"
                );
                assert!(
                    waiting["conversations"][0]["tasks"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|task| task["status"] == "running"),
                    "追问真实运行状态仍应显示在任务列表"
                );
                provider.release.add_permits(1);
                host.wait_idle().await;
                let after = prepare(&host, &office, "office-1").await.unwrap();
                let current = &after["conversations"][0];
                for previous in conv["tasks"].as_array().unwrap() {
                    assert!(current["tasks"].as_array().unwrap().iter().any(|t| t["id"] == previous["id"] && t["status"] == previous["status"]), "后续普通问答不能移除准确已完成业务任务");
                }
                assert_eq!(
                    current["whiteboard"], conv["whiteboard"],
                    "普通追问保留最近业务目标白板"
                );
                let details = crate::office::details::render(
                    &host,
                    &scope,
                    &binding.session_id,
                    "after-followup",
                    json!({"kind":"member","employeeId":"command","actorId":"code-agent"}),
                    &CancellationToken::new(),
                )
                .await
                .unwrap();
                assert!(
                    details["content"]["fields"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|f| f["label"] == "独立执行引用"),
                    "成员详情仍应指向其原独立Session/Run"
                );
                assert_eq!(
                    prepare(&host, &office, "office-1").await.unwrap(),
                    after,
                    "保留历史不能造成无事实seq增长"
                );
                host.submit_run(&scope, &binding.session_id, "建立独立的新业务计划".into())
                    .await
                    .unwrap();
                host.wait_idle().await;
                let next = prepare(&host, &office, "office-1").await.unwrap();
                assert!(
                    next["conversations"][0]["whiteboard"]["title"]
                        .as_str()
                        .unwrap()
                        .contains("新业务计划"),
                    "真实新计划必须接管白板"
                );
                assert_ne!(next["conversations"][0]["whiteboard"], conv["whiteboard"]);
                assert_ne!(
                    next["conversations"][0]["whiteboard"]["status"], "completed",
                    "不能沿用旧目标100%冒充新目标完成"
                );
            }
            host.shutdown().await.unwrap();
        }
    }
    #[tokio::test]
    async fn delegated_business_step_keeps_one_card_owner_and_stable_assignment() {
        use openpaw_llm::{MockMode, MockProvider};
        struct OnceFailure {
            inner: MockProvider,
            calls: std::sync::atomic::AtomicUsize,
            fail_at: Option<usize>,
        }
        #[async_trait::async_trait]
        impl openpaw_core::LlmProvider for OnceFailure {
            async fn complete(
                &self,
                request: openpaw_core::CompletionRequest,
            ) -> openpaw_core::Result<openpaw_core::CompletionResponse> {
                self.inner.complete(request).await
            }
            async fn stream(
                &self,
                request: openpaw_core::CompletionRequest,
            ) -> openpaw_core::Result<
                futures::stream::BoxStream<
                    'static,
                    openpaw_core::Result<openpaw_core::StreamEvent>,
                >,
            > {
                let index = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                if self.fail_at == Some(index) {
                    return Err(openpaw_core::Error::Provider(
                        "invalid child response".into(),
                    ));
                }
                self.inner.stream(request).await
            }
            fn capabilities(&self) -> openpaw_core::Capabilities {
                self.inner.capabilities()
            }
            fn default_model(&self) -> &str {
                self.inner.default_model()
            }
            fn name(&self) -> &str {
                "one-child-failure"
            }
        }
        for (mark_active, confirm, retry) in [
            (true, true, false),
            (true, false, false),
            (false, true, false),
            (false, false, false),
            (true, true, true),
            (true, false, true),
        ] {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().join("root");
            std::fs::create_dir(&root).unwrap();
            std::fs::write(root.join("README.md"), "actual project fact").unwrap();
            let config:HostConfig=serde_json::from_value(json!({"home":temp.path().join("home"),"roots":{"r":root},"slots":{"office-1":"r"},"provider":"echo","tools":["read_file"],"agents":{"enabled":true}})).unwrap();
            let call = |name: &str, input| MockMode::ToolUse {
                tool_name: name.into(),
                input,
                text: String::new(),
            };
            let mut script = vec![call(
                "report_progress",
                json!({"type":"create","steps":[{"id":"lookup","title":"核对项目说明"},{"id":"summary","title":"王明汇总同事的实际结果"}]}),
            )];
            if mark_active {
                script.push(call(
                    "report_progress",
                    json!({"type":"update","stepId":"lookup","state":"in_progress"}),
                ));
            }
            let delegate_call = if retry {
                "call_3"
            } else if mark_active {
                "call_2"
            } else {
                "call_1"
            };
            script.extend(vec![
            call(
                "delegate_task",
                json!({"role_id":"research","title":"李研核对项目目标","instructions":"读取README.md","acceptance":"有文件依据"}),
            ),
            call("read_file", json!({"path":"README.md"})),
            MockMode::Fixed("李研已实际读取项目说明".into()),
        ]);
            if retry {
                // 首个子模型确实失败；父角色明确发起新的委派，不复用或抹去失败Run。
                script.insert(3, call("delegate_task", json!({"role_id":"research","title":"李研重新核对项目目标","instructions":"读取README.md","acceptance":"有文件依据"})));
            }
            if confirm {
                script.push(call(
                "report_progress",
                json!({"type":"update","stepId":"lookup","state":"completed","evidenceCallIds":[delegate_call]}),
                ));
            }
            script.extend(vec![
                call(
                    "report_progress",
                    json!({"type":"update","stepId":"summary","state":"in_progress"}),
                ),
                MockMode::Fixed("王明汇总：同事已读取项目说明，证据已核实".into()),
            ]);
            let provider = OnceFailure {
                inner: MockProvider::scripted(script),
                calls: std::sync::atomic::AtomicUsize::new(0),
                fail_at: retry.then_some(3),
            };
            let host = Host::with_provider(config, std::sync::Arc::new(provider))
                .await
                .unwrap();
            let scope = ClientScope {
                namespace: "office".into(),
                slot_id: "office-1".into(),
                conversation_id: "linked".into(),
            };
            let session = host.open_session(scope.clone()).await.unwrap();
            host.submit_run(&scope, &session.session_id, "请安排李研核对并汇总".into())
                .await
                .unwrap();
            tokio::time::timeout(Duration::from_secs(5), host.wait_idle())
                .await
                .unwrap();
            let office = OfficeConfig {
                base_url: "http://127.0.0.1:9".into(),
                api_key_env: "PAWOFFICE_TEST_KEY".into(),
                allowed_origins: vec!["http://127.0.0.1:8765".into()],
                names: Default::default(),
                creation_slots: vec![],
            };
            let snapshot = prepare(&host, &office, "office-1").await.unwrap();
            let conv = &snapshot["conversations"][0];
            // 动态按观察时间排序，首条不再必然是最近派工；用持久委派ID精确对账。
            let session_id = session.session_id.clone();
            let latest_task = host
                .inner
                .store
                .call(move |db| {
                    let parent = db
                        .runs()?
                        .into_iter()
                        .find(|r| r.session_id == session_id)
                        .unwrap();
                    Ok(db
                        .delegations(&parent.run_id)?
                        .last()
                        .unwrap()
                        .task_id
                        .clone())
                })
                .await
                .unwrap();
            let latest_assignment = conv["events"]
                .as_array()
                .unwrap()
                .iter()
                .find(|event| event["id"] == format!("assign:{latest_task}"))
                .unwrap();
            assert_eq!(latest_assignment["kind"], "task.assigned");
            assert_eq!(
                conv["tasks"].as_array().unwrap().len(),
                if confirm || (mark_active && !retry) {
                    3
                } else {
                    4
                },
                "有准确证据才合并；不能按相似任务标题猜测关联"
            );
            if confirm {
                assert_eq!(conv["tasks"][0]["status"], "completed");
                assert_eq!(
                    conv["tasks"][0]["progress"], 100,
                    "正常保存最终汇总后才确认交付"
                );
                assert_eq!(conv["tasks"][1]["assigneeId"], "command");
                assert_eq!(latest_assignment["taskId"], conv["tasks"][1]["id"]);
            } else {
                assert_eq!(
                    conv["tasks"][0]["status"],
                    if retry { "failed" } else { "blocked" }
                );
                assert!(conv["tasks"][0]["progress"].as_u64().unwrap() < 100);
                assert_eq!(
                    conv["tasks"][1]["status"],
                    if retry { "failed" } else { "blocked" },
                    "子任务成功不自动确认未报告步骤"
                );
                let child_index = if mark_active && !retry { 1 } else { 3 };
                assert_eq!(conv["tasks"][child_index]["assigneeId"], "command");
                assert_eq!(
                    latest_assignment["taskId"],
                    conv["tasks"][child_index]["id"]
                );
            }
            assert_eq!(
                conv["events"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|event| event["kind"] == "task.assigned")
                    .count(),
                if retry { 2 } else { 1 }
            );
            let completions: Vec<_> = conv["events"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|event| event["kind"] == "task.completed")
                .collect();
            assert_eq!(
                completions.len(),
                usize::from(confirm || !mark_active || retry),
                "关联业务步骤尚未核验不能伪报完成；独立子卡则依照其真实终态"
            );
            if let Some(event) = completions.first() {
                assert_eq!(event["employeeId"], "command");
                let child_index = if confirm { 1 } else { 3 };
                assert_eq!(event["taskId"], conv["tasks"][child_index]["id"]);
                assert!(event["summary"].as_str().unwrap().contains("已核验"));
            }
            if retry {
                let session_id = session.session_id.clone();
                host.inner
                    .store
                    .call(move |db| {
                        let parent = db
                            .runs()?
                            .into_iter()
                            .find(|r| r.session_id == session_id)
                            .unwrap();
                        let ds = db.delegations(&parent.run_id)?;
                        assert_eq!(ds.len(), 2);
                        assert_eq!(db.get_run(&ds[0].child_run)?.state, RunState::Failed);
                        assert_eq!(db.get_run(&ds[1].child_run)?.state, RunState::Completed);
                        assert_ne!(ds[0].child_session, ds[1].child_session);
                        Ok(())
                    })
                    .await
                    .unwrap();
                if confirm {
                    assert!(
                        conv["events"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .all(|event| event["taskId"] == conv["tasks"][1]["id"]),
                        "重试的两次真实交接保留并引用同一业务步骤"
                    );
                }
            }
            assert!(conv["events"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|event| event["kind"] == "task.assigned")
                .all(|event| event["fromEmployeeId"] == "assistant"));
            assert!(
                !conv["events"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|event| event["kind"] == "task.started"),
                "首次只看到终态，不能捏造已观察开始的历史"
            );
            assert_eq!(
                conv["events"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|event| event["id"].as_str().unwrap().ends_with(":incomplete"))
                    .count(),
                usize::from(retry)
            );
            assert_eq!(
                prepare(&host, &office, "office-1").await.unwrap(),
                snapshot,
                "无新事实不改变seq或重播交接"
            );
            host.shutdown().await.unwrap();
        }
    }
    #[tokio::test]
    async fn legacy_snapshot_upgrades_with_new_seq_and_full_catalogue_is_bounded() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        std::fs::create_dir(&root).unwrap();
        use openpaw_llm::{MockMode, MockProvider};
        let config:HostConfig=serde_json::from_value(json!({"home":temp.path().join("home"),"roots":{"r":root},"slots":{"office-1":"r"},"provider":"echo","tools":["write_file"],"approval_timeout_secs":30})).unwrap();
        let mut script = vec![MockMode::Fixed("已完成旧任务".into()); 100];
        script.extend([
            MockMode::ToolUse { tool_name: "report_progress".into(), input: json!({"type":"create","steps":[{"id":"draft","title":"撰写本轮报告"},{"id":"check","title":"核验本轮报告"}]}), text: String::new() },
            MockMode::ToolUse { tool_name: "write_file".into(), input: json!({"path":"output.txt","content":"must wait for approval"}), text: String::new() },
            MockMode::Fixed("本轮已完成".into()),
        ]);
        let host = Host::with_provider(config, std::sync::Arc::new(MockProvider::scripted(script)))
            .await
            .unwrap();
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
        for conversation in snapshot["conversations"].as_array().unwrap() {
            let members = conversation["employees"].as_array().unwrap();
            assert_eq!(members.len(), 6, "容量裁剪不能移除会话人物绑定或左侧成员");
            for role in crate::agents::ROLES.iter() {
                assert!(members.iter().any(|member| member["id"] == role.employee
                    && member["sceneActorId"] == role.actor
                    && member["name"] == role.name));
            }
        }
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
        let active_scope = ClientScope {
            namespace: "office".into(),
            slot_id: "office-1".into(),
            conversation_id: "conv-099".into(),
        };
        let active_session = host.open_session(active_scope.clone()).await.unwrap();
        let active_run = host
            .submit_run(
                &active_scope,
                &active_session.session_id,
                "满目录中等待确认写报告".into(),
            )
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let (view, mut events) = host.snapshot_and_subscribe(&active_scope).await.unwrap();
                if !view.pending_approvals.is_empty() {
                    break;
                }
                events.recv().await.unwrap();
            }
        })
        .await
        .unwrap();
        let active_snapshot = prepare(&host, &office, "office-1").await.unwrap();
        let current = active_snapshot["conversations"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["id"] == "conv-099")
            .unwrap();
        assert_eq!(
            current["tasks"].as_array().unwrap().len(),
            3,
            "64KiB裁剪不得删除正在审批的业务步骤"
        );
        assert_eq!(current["tasks"][0]["status"], "blocked");
        assert_eq!(current["employees"].as_array().unwrap().len(), 6);
        assert_eq!(
            active_snapshot["conversations"].as_array().unwrap().len(),
            100
        );
        assert!(
            serde_json::to_vec(
                &json!({"type":"office.snapshot","teamId":"office-1","snapshot":active_snapshot})
            )
            .unwrap()
            .len()
                <= 64000
        );
        assert_eq!(
            prepare(&host, &office, "office-1").await.unwrap(),
            active_snapshot
        );
        assert!(!root.join("output.txt").exists());
        host.cancel_run(&active_scope, &active_run.run_id)
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), host.wait_idle())
            .await
            .unwrap();
        let terminal_snapshot = prepare(&host, &office, "office-1").await.unwrap();
        let latest = terminal_snapshot["conversations"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["id"] == "conv-099")
            .unwrap();
        assert_eq!(
            latest["tasks"].as_array().unwrap().len(),
            3,
            "先缩旧终态，不能先清掉最新目标"
        );
        assert!(!root.join("output.txt").exists());
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
