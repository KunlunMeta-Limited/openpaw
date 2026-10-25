// T6 acceptance helper invoked by the existing product runner; no extra server or dependency.
import assert from 'node:assert/strict'
import { readFile } from 'node:fs/promises'
import { join } from 'node:path'
import { DatabaseSync } from 'node:sqlite'
export async function teamProduct({page,report,output,readOffice,conv,latestSend,fresh,send,expand,chooseOffice,waitFor,release}) {
  const roles=[['research','command','code-agent','李研'],['organize','files','file-agent','周理'],['draft','draft','app-agent','陈书'],['review','review','review-agent','刘市'],['audit','search','data-agent','赵审']]
  let exportIndex=0
  async function scene(label) {
    const pending=page.waitForEvent('download')
    await page.getByRole('button',{name:'导出快照',exact:true}).click()
    const path=join(output,`t6-${exportIndex++}-${label}.json`)
    await (await pending).saveAs(path)
    return JSON.parse(await readFile(path,'utf8'))
  }
  // Use the existing user control to keep this walkthrough free of idle coffee traffic.
  // The preceding unmodified full product journey keeps default idle behavior; its traffic failure is retained separately.
  await page.reload();await chooseOffice(page);await page.getByRole('button',{name:'关闭空闲闲聊',exact:true}).click()
  await fresh(page);await send('T6真实五人协作')
  const request=structuredClone(latestSend().request)
  await page.getByRole('button',{name:'收起聊天',exact:true}).click()
  const handoffs=[]
  for(const [role,employee,actor,name] of roles) {
    const data=await waitFor(async()=>{const r=await readOffice();const c=conv(r,request.conversationId);return c?.tasks.some(t=>t.assigneeId===employee&&t.status==='running')?c:false})
    const task=data.tasks.find(t=>t.assigneeId===employee&&t.status==='running')
    const event=data.events.find(e=>e.kind==='task.assigned'&&e.taskId===task.id&&e.employeeId===employee&&e.fromEmployeeId==='assistant')
    assert.ok(event);assert.ok(!/call_|web_fetch|read_file/.test(task.title));assert.ok(data.tasks[0].progress<100)
    assert.ok(data.events.some(e=>e.kind==='task.started'&&e.taskId===task.id&&e.employeeId===employee),'真实工作态必须向成员最近动态投影开始事实')
    const started=await waitFor(async()=>{const s=await scene(role+'-active');return s.records.some(r=>r.command.capability==='office.visit'&&r.command.participants[0].entityId==='office-lead'&&r.command.participants[1].entityId===actor)?s:false},45000)
    const visit=started.records.findLast(r=>r.command.capability==='office.visit'&&r.command.participants[1].entityId===actor)
    const person=started.world.actors.find(a=>a.id===actor)
    assert.equal(person.presentation.title,task.title);assert.equal(person.presentation.status,'working')
    await page.screenshot({path:join(output,`t6-handoff-${role}.png`),fullPage:true})
    const finished=await waitFor(async()=>{const s=await scene(role+'-arrived');return s.records.some(r=>r.command.commandId===visit.command.commandId&&r.status==='completed')?s:false},45000)
    // Only the deterministic model is gated for a visible walkthrough. Production code never waits on a fake animation ACK.
    await page.locator('.runtime-person').filter({hasText:name}).click()
    await page.getByRole('dialog',{name:`${name} · 成员详情`,exact:true}).waitFor()
    assert.ok((await page.getByRole('dialog').innerText()).includes(task.title))
    await page.screenshot({path:join(output,`t6-member-working-${role}.png`),fullPage:true})
    await page.getByRole('button',{name:'关闭详情',exact:true}).click()
    handoffs.push({role,employee,actor,task,event,command:visit.command,record:finished.records.find(r=>r.command.commandId===visit.command.commandId)})
    release(role)
  }
  await expand(page)
  await waitFor(async()=>(await page.getByRole('log',{name:'对话内容'}).innerText()).includes('T6五个独立角色核查完成'),45000)
  await page.getByRole('button',{name:'停止生成',exact:true}).waitFor({state:'hidden',timeout:15000})
  const final=await waitFor(async()=>{const c=conv(await readOffice(),request.conversationId);return c?.tasks[0].status==='completed'&&c.tasks[0].progress===100?c:false})
  assert.equal(final.tasks.length,6);assert.equal(final.events.filter(e=>e.kind==='task.assigned').length,5)
  for(const [,employee] of roles){const personal=final.events.filter(e=>e.employeeId===employee);assert.equal(personal.filter(e=>e.kind==='task.started').length,1);assert.equal(personal.filter(e=>e.kind==='task.completed').length,1);assert.ok(personal.find(e=>e.kind==='task.completed').summary.includes('已核验'))}
  assert.ok(final.whiteboard.steps.length<=3);assert.ok(final.whiteboard.description.includes('5/5'))
  await page.getByRole('button',{name:'收起聊天',exact:true}).click()
  const snapshot=await scene('complete')
  assert.equal(snapshot.records.filter(r=>r.command.capability==='office.visit').length,5,'每个真实派发只播放一次交接')
  // 成员布局必须实际显示投影事件，不能只在HTTP快照里通过断言。
  await page.getByRole('button',{name:'切换页面布局',exact:true}).click()
  await page.getByRole('button',{name:'成员布局',exact:true}).click()
  const timelines=[]
  for(const [role,,,name] of roles) {
    await page.getByRole('button',{name:'查看'+name+'的资料',exact:true}).click()
    const region=page.getByRole('region',{name:'当前任务与最近动态'})
    await region.waitFor()
    const text=await region.innerText()
    assert.ok(text.includes('观察到独立执行'),name+'应显示实际观察过的开始动态')
    assert.ok(text.includes('已核验完成'),name+'应显示已核验的完成动态')
    await page.screenshot({path:join(output,`t6-timeline-${role}.png`),fullPage:true})
    timelines.push({role,name,text})
  }
  await page.getByRole('button',{name:'切换页面布局',exact:true}).click()
  await page.getByRole('button',{name:'经典布局',exact:true}).click()

  const db=new DatabaseSync(join(output,'host-data/engine-host/host.db'),{readOnly:true})
  const parent=db.prepare('SELECT run FROM host_requests WHERE key=?').get(request.turnId).run
  const children=db.prepare("SELECT reference FROM host_events WHERE kind='agent_delegation' AND json_extract(reference,'$.run_id')=? ORDER BY revision").all(parent).map(r=>JSON.parse(r.reference))
  assert.equal(children.length,5);assert.equal(new Set(children.map(c=>c.child_session)).size,5);assert.equal(new Set(children.map(c=>c.child_run)).size,5)
  const parentBinding=JSON.parse(db.prepare('SELECT data FROM host_bindings WHERE id=(SELECT session FROM host_runs WHERE id=?)').get(parent).data);assert.equal(parentBinding.root_id,'project');
  for(const child of children){const binding=JSON.parse(db.prepare('SELECT data FROM host_bindings WHERE id=?').get(child.child_session).data);assert.equal(binding.root_id,parentBinding.root_id,'真实主子Agent必须共享所属团队根');assert.equal(binding.scope.slot_id,parentBinding.scope.slot_id)}
  const secondBindings=db.prepare("SELECT data FROM host_bindings WHERE slot='office-2'").all().map(row=>JSON.parse(row.data));assert.ok(secondBindings.length>0);assert.ok(secondBindings.every(binding=>binding.root_id==='project-2'));
  report.workspaceIsolation={...report.workspaceIsolation,firstTeamRoot:parentBinding.root_id,childRootsVerified:children.length,secondTeamRoot:'project-2',secondBindings:secondBindings.length};
  for(const child of children){const ledger=(await readFile(join(output,'host-data/sessions',child.child_session+'.jsonl'),'utf8')).trim().split('\n').map(JSON.parse);assert.ok(ledger.some(e=>e.type==='tool_call_complete'&&e.tool_name==='read_file'));assert.ok(ledger.some(e=>e.type==='run_finished'&&e.status==='completed'))}
  db.close()
  // Actual Pixi clicks use exported world positions and the frozen renderer transform. HTTP actorId remains authoritative.
  const actors=[];const members=[]
  for(const person of snapshot.world.actors) {
    const canvas=await page.locator('canvas').first().boundingBox();assert.ok(canvas)
    const scale=Math.min(canvas.width/1100,canvas.height/850)
    const x=canvas.x+(canvas.width-1100*scale)/2+((person.position.x+.5)*50+(person.posture==='seated'?25:0))*scale
    const y=canvas.y+(canvas.height-850*scale)/2+((person.position.y+.5)*50-(person.posture==='seated'?10:0)-40)*scale
    const reply=page.waitForResponse(r=>r.url().endsWith('/api/pawoffice/ui')&&r.request().postDataJSON()?.action==='actor.inspect')
    await page.mouse.click(x,y)
    const response=await reply;const body=await response.json();assert.equal(body.data.target,'dialog')
    const actual=response.request().postDataJSON();assert.equal(actual.actorId,person.id)
    const dialog=page.getByRole('dialog',{name:`${person.name} · 成员详情`,exact:true});await dialog.waitFor()
    assert.ok(body.data.content.fields.length>=5)
    const progress=body.data.content.fields.find(f=>f.label==='已确认进度');assert.equal(progress?.value,'100%')
    assert.ok(body.data.content.fields.some(f=>f.label==='业务状态'&&f.value==='已核验完成'))
    assert.equal(body.data.content.actions.length,8)
    const refreshReply=page.waitForResponse(r=>r.url().endsWith('/api/pawoffice/ui')&&r.request().postDataJSON()?.action==='dialog.action'&&r.request().postDataJSON()?.optionId==='refresh')
    await page.getByRole('button',{name:'刷新任务状态',exact:true}).click();const refreshed=await(await refreshReply).json();assert.deepEqual(refreshed.data.content,body.data.content)
    await page.screenshot({path:join(output,`t6-actor-${person.id}.png`),fullPage:true})
    await page.getByRole('button',{name:'历史',exact:true}).click();await page.getByRole('button',{name:'返回目录',exact:true}).click();await dialog.waitFor()
    await page.getByRole('button',{name:'关闭详情',exact:true}).click();actors.push({request:actual,response:body})
    await page.locator('.runtime-person').filter({hasText:person.name}).click();await dialog.waitFor()
    const text=await dialog.innerText();assert.ok(text.includes(person.name))
    await page.screenshot({path:join(output,`t6-sidebar-${person.id}.png`),fullPage:true})
    await page.getByRole('button',{name:'任务进度与结果',exact:true}).click()
    const taskList=page.getByRole('dialog',{name:'任务进度与结果',exact:true});await taskList.waitFor()
    assert.ok((await taskList.innerText()).includes('已核验完成'))
    await page.getByRole('button',{name:/^查看 /}).first().click()
    await page.getByRole('button',{name:'返回任务列表',exact:true}).waitFor()
    assert.ok((await page.getByRole('dialog').innerText()).includes('实际执行状态'))
    assert.ok((await page.getByRole('dialog').innerText()).includes('执行已结束'))
    await page.screenshot({path:join(output,`t6-task-detail-${person.id}.png`),fullPage:true})
    await page.getByRole('button',{name:'返回任务列表',exact:true}).click();await taskList.waitFor()
    await page.getByRole('button',{name:'返回目录',exact:true}).click();await dialog.waitFor()
    members.push({person:person.id,text});await page.getByRole('button',{name:'关闭详情',exact:true}).click()
  }
  assert.equal(new Set(actors.map(a=>a.request.actorId)).size,6);assert.equal(members.length,6)
  // A normal follow-up must not replace the real team ledger, whiteboard or member results.
  await expand(page);await send('谢谢，请保留团队成果','网页协议对话正常。已处理：谢谢，请保留团队成果')
  const retained=await waitFor(async()=>{const c=conv(await readOffice(),request.conversationId);return c?.tasks.length===7?c:false})
  for(const task of final.tasks)assert.deepEqual(retained.tasks.find(t=>t.id===task.id),task)
  assert.deepEqual(retained.whiteboard,final.whiteboard)
  await page.getByRole('button',{name:'收起聊天',exact:true}).click()
  await page.locator('.runtime-person').filter({hasText:'陈书'}).click()
  await page.getByRole('dialog',{name:'陈书 · 成员详情',exact:true}).waitFor()
  assert.ok((await page.getByRole('dialog').innerText()).includes('子任务已完成'))
  await page.getByRole('button',{name:'任务进度与结果',exact:true}).click()
  await page.getByRole('dialog',{name:'任务进度与结果',exact:true}).waitFor()
  assert.ok((await page.getByRole('dialog').innerText()).includes('陈书核对项目说明'))
  await page.screenshot({path:join(output,'t6-followup-retains-team.png'),fullPage:true})
  await page.getByRole('button',{name:'关闭详情',exact:true}).click()
  await page.getByRole('dialog').waitFor({state:'hidden'})
  await page.locator('.runtime-person').filter({hasText:'王明'}).click()
  await page.getByRole('dialog',{name:'王明 · 成员详情',exact:true}).waitFor()
  await page.getByRole('button',{name:'任务进度与结果',exact:true}).click()
  await page.getByRole('dialog',{name:'任务进度与结果',exact:true}).waitFor()
  // 王明只看本人任务，完整团队卡片仍在Office任务中心，不混算各角色进度。
  assert.ok((await page.getByRole('dialog').innerText()).includes('已核验完成'))
  await page.getByRole('button',{name:'关闭详情',exact:true}).click()
  report.t6={request,parent,children,handoffs,actors,members,timelines,final,scene:snapshot,model:'deterministic real HTTP Provider; animation pacing only in test fixture; real Host execution'}
  report.assertions.push('D61 five real member timelines preserve observed start and verified completion, without additional assignment or visit', 'T6 six actual Pixi actor clicks and six sidebar members show the correct scoped identity with return navigation','T6 real parent + five independent Session/Run/JSONL with real read_file and Provider HTTP calls','T6 five task.assigned events cause exactly five observed office.visit completions; each worker presents its real current task','T6 whiteboard bounded business goal and true 5/5; no per-call task cards','r6: real five children share their parent team root while second-team existing sessions stay on a distinct physical root')
  report.assertions.push('r7: six members open their own real task/result details and return without losing identity','D65: ordinary follow-up retains completed team cards, original whiteboard and independent member results')
  report.assertions.push('Six actual actor dialogs show their own verified task/progress/result, and refresh keeps identity without creating another Run')
}


// Browser transport loss is injected only in its isolated context; Host/Provider continue their real Run.
export async function networkProduct({page,context,report,output,readOffice,conv,latestSend,fresh,send,chooseOffice,select,waitFor,release,interrupt,restore,post}) {
  await fresh(page);await send('T6主动断网续接')
  const request=structuredClone(latestSend().request)
  const log=page.getByRole('log',{name:'对话内容'})
  await waitFor(async()=>(await log.innerText()).includes('断网前已保存的真实文本。'))
  await memberLayoutProduct({page,report,output,readOffice,conv,request,post,chooseOffice,select,waitFor})
  const db=new DatabaseSync(join(output,'host-data/engine-host/host.db'),{readOnly:true})
  const parent=db.prepare('SELECT run FROM host_requests WHERE key=?').get(request.turnId).run
  const requestsBefore=report.modelRequests.length
  const historyBefore=report.browserRequests.filter(r=>r.request.type==='history').length
  try {
    await context.setOffline(true)
    // Chromium离线开关不保证关闭已经建立且暂时没有流量的HTTP连接。
    // 同时真实关闭隔离转发进程，让当前聊天链路实际断开；Host及模型仍保持运行。
    await interrupt()
    await waitFor(async()=>(await log.innerText()).includes('回复未完成'))
    assert.equal(JSON.parse(db.prepare('SELECT data FROM host_runs WHERE id=?').get(parent).data).state,'running','浏览器断网不能取消真实Run')
    await page.screenshot({path:join(output,'t6-network-offline-detached.png'),fullPage:true})
  } finally {await context.setOffline(false);await restore()}
  // 服务目录重建可能默认选中另一办公室；先经公开导航恢复本次请求的会话。
  // history/resume发生在后台不代表当前可见的对话就是这个Run。
  await chooseOffice(page)
  await select(page,'T6主动断网续接')
  const resync=page.getByRole('button',{name:'重新同步',exact:true})
  let recoveryAction='重新同步'
  if(await resync.isVisible())await resync.click()
  else {
    // 转发重启可能重建Office页面并收起聊天；公开展开操作会按原useEffect同步历史。
    recoveryAction='转发恢复后选择原办公室及原会话触发官方history/resume'
  }
  await waitFor(()=>report.browserRequests.filter(r=>r.request.type==='history').length>historyBefore&&report.browserRequests.some(r=>r.request.type==='resume'&&r.request.turnId===request.turnId))
  assert.equal(db.prepare('SELECT run FROM host_requests WHERE key=?').get(request.turnId).run,parent)
  assert.equal(report.modelRequests.length,requestsBefore,'续接不再次调用模型')
  await page.screenshot({path:join(output,'t6-network-resumed-same-run.png'),fullPage:true})
  release()
  await waitFor(async()=>(await log.innerText()).includes('断网后继续同一个Run。'))
  await page.getByRole('button',{name:'停止生成',exact:true}).waitFor({state:'hidden',timeout:15000})
  const transcript=await log.innerText()
  assert.equal(transcript.split('断网前已保存的真实文本。').length-1,1,'snapshot替换后不重复拼接文本')
  assert.equal(report.browserRequests.filter(r=>r.request.type==='send'&&r.request.turnId===request.turnId).length,1)
  const final=await waitFor(async()=>{const c=conv(await readOffice(),request.conversationId);return c?.tasks[0].status==='completed'?c:false})
  await page.screenshot({path:join(output,'t6-network-completed-same-run.png'),fullPage:true})
  report.network={request,parent,requestsBefore,requestsAfter:report.modelRequests.length,transcript,final,recoveryAction,trigger:'Chromium offline + SIGKILL isolated Office forwarding process; Host and Provider remain alive'}
  report.assertions.push('T6 actual Chromium offline loss detaches; public manual history/resume keeps one persisted Run, one model request and nonduplicated text')
  db.close()
}

// 仅在隔离浏览器中中止真实IndexedDB事务；不改Office源码、不模拟磁盘耗尽。
export async function storageProduct({page,report,output,fresh,waitFor}) {
  async function archive() {
    return page.evaluate(async()=>{
      const db=await new Promise((resolve,reject)=>{const r=indexedDB.open('pawoffice-chat',2);r.onsuccess=()=>resolve(r.result);r.onerror=()=>reject(r.error)})
      return new Promise((resolve,reject)=>{
        const tx=db.transaction(['conversations','chat-pages'],'readonly')
        const directories=tx.objectStore('conversations').getAll(),pages=tx.objectStore('chat-pages').getAll()
        tx.oncomplete=()=>{db.close();resolve({directories:directories.result,pages:pages.result})}
        tx.onabort=()=>{db.close();reject(tx.error??new Error('存档读取事务中止'))}
      })
    })
  }
  await fresh(page)
  const box=page.getByRole('textbox',{name:'给办公室发送消息'})
  await box.fill('存储拒绝前的原草稿')
  await waitFor(async()=>JSON.stringify(await archive()).includes('存储拒绝前的原草稿'))
  const before=await archive(),sends=report.browserRequests.filter(r=>r.request.type==='send').length
  try {
    await page.evaluate(()=>{
      const original=IDBDatabase.prototype.transaction
      globalThis.t6StorageAborts=[]
      IDBDatabase.prototype.transaction=function(...args){
        const tx=Reflect.apply(original,this,args)
        if(this.name==='pawoffice-chat'&&args[1]==='readwrite'){
          tx.addEventListener('abort',()=>globalThis.t6StorageAborts.push({mode:tx.mode,error:tx.error?.name??'explicit abort'}))
          queueMicrotask(()=>tx.abort())
        }
        return tx
      }
      globalThis.t6RestoreStorage=()=>{IDBDatabase.prototype.transaction=original}
    })
    await box.fill('中止事务后的新草稿，只在内存中保留')
    const alert=page.getByRole('alert').filter({hasText:/保存失败/})
    await alert.waitFor()
    const aborts=await page.evaluate(()=>globalThis.t6StorageAborts)
    assert.ok(aborts.length>0,'必须实际中止浏览器事务，不能仅伪造错误消息')
    const after=await archive()
    assert.deepEqual(after,before,'写入中止后原目录、分页和草稿不得被覆盖')
    assert.equal(report.browserRequests.filter(r=>r.request.type==='send').length,sends,'本场景不发送用户消息或启动模型')
    await page.screenshot({path:join(output,'t6-storage-write-aborted-original-preserved.png'),fullPage:true})
    report.storageFailure={trigger:'actual browser IndexedDB transaction.abort, isolated browser context only; not a quota/disk-full test',aborts,warning:await alert.innerText(),before,after}
    report.assertions.push('T6 actual browser IndexedDB write abort shows save warning and preserves exact original archive; no model request')
  } finally {await page.evaluate(()=>globalThis.t6RestoreStorage?.())}
}


// 新布局仍消费同一Host快照；本函数只操作原Office按钮和公开布局路由。
async function memberLayoutProduct({page,report,output,readOffice,conv,request,post,chooseOffice,select,waitFor}) {
  const db=new DatabaseSync(join(output,'host-data/engine-host/host.db'),{readOnly:true})
  const runs=()=>db.prepare('SELECT id,session FROM host_runs ORDER BY id').all()
  const before=runs(), modelsBefore=report.modelRequests.length
  const parent=db.prepare('SELECT run FROM host_requests WHERE key=?').get(request.turnId).run
  const collapse=page.getByRole('button',{name:'收起聊天',exact:true});if(await collapse.isVisible())await collapse.click()
  const initial=(await post({type:'office.layout.read'})).data
  assert.equal(initial.currentLayoutId,'classic')
  await page.getByRole('button',{name:'切换页面布局',exact:true}).click()
  await page.getByRole('button',{name:'成员布局',exact:true}).click()
  const roster=page.getByRole('complementary',{name:'办公室成员',exact:true});await roster.waitFor()
  const profiles=[]
  for(const name of ['王明','李研','周理','陈书','刘市','赵审']) {
    await page.getByRole('button',{name:'查看'+name+'的资料',exact:true}).click()
    await page.locator('.member-profile-heading').getByRole('heading',{name:new RegExp('^'+name)}).waitFor()
    const text=await page.getByRole('region',{name:'当前任务与最近动态'}).innerText()
    await page.getByRole('button',{name:'查看完整档案',exact:true}).click()
    const dialog=page.getByRole('dialog',{name:name+' · 成员详情',exact:true});await dialog.waitFor()
    assert.ok((await dialog.innerText()).includes(name))
    await page.screenshot({path:join(output,'t6-members-layout-'+profiles.length+'.png'),fullPage:true})
    await page.getByRole('button',{name:'关闭详情',exact:true}).click()
    profiles.push({name,text})
  }
  const add=page.getByRole('button',{name:'添加成员',exact:true})
  assert.equal(await add.isEnabled(),false);assert.ok((await add.getAttribute('title')).includes('6'))
  const selected=(await post({type:'office.layout.read'})).data
  assert.equal(selected.currentLayoutId,'members');assert.ok(selected.revision>initial.revision)
  assert.equal((await post({type:'office.layout.set',layoutId:'members'})).data.revision,selected.revision)
  const invalid=await page.request.post(new URL('/api/pawoffice/layout',page.url()).href,{headers:{origin:new URL(page.url()).origin},data:{type:'office.layout.set',layoutId:'invented'}})
  assert.equal(invalid.status(),400)
  const other=await page.context().newPage()
  try {
    await other.goto(new URL('/office/',page.url()).href)
    await other.getByRole('complementary',{name:'办公室成员',exact:true}).waitFor({timeout:60000})
    await other.screenshot({path:join(output,'t6-members-second-page.png'),fullPage:true})
    await page.reload();await chooseOffice(page);await roster.waitFor()
    assert.equal((await post({type:'office.layout.read'})).data.revision,selected.revision)
    await page.getByRole('button',{name:'切换页面布局',exact:true}).click()
    await page.getByRole('button',{name:'经典布局',exact:true}).click()
    await roster.waitFor({state:'hidden'});await other.getByRole('complementary',{name:'办公室成员',exact:true}).waitFor({state:'hidden'})
  } finally {await other.close()}
  await select(page,'T6主动断网续接')
  assert.deepEqual(runs(),before);assert.equal(report.modelRequests.length,modelsBefore)
  assert.equal(JSON.parse(db.prepare('SELECT data FROM host_runs WHERE id=?').get(parent).data).state,'running')
  assert.ok(conv(await readOffice(),request.conversationId).tasks.some(t=>t.status==='running'))
  db.close()
  report.memberLayout={parent,request,profiles,selected,initial,runsBefore:before,runsAfter:before,modelsBefore,modelsAfter:report.modelRequests.length,invalidStatus:invalid.status()}
  report.assertions.push('T6 active Run survives classic/members switching, refresh and cross-page layout synchronization without duplicate Run/model request','T6 six members layout profile buttons reach six real Host details; full roster disables add; layout revision stays separate from snapshot seq')
}
