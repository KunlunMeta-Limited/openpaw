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
  assert.ok(final.whiteboard.steps.length<=3);assert.ok(final.whiteboard.description.includes('5/5'))
  await page.getByRole('button',{name:'收起聊天',exact:true}).click()
  const snapshot=await scene('complete')
  assert.equal(snapshot.records.filter(r=>r.command.capability==='office.visit').length,5,'每个真实派发只播放一次交接')
  const db=new DatabaseSync(join(output,'host-data/engine-host/host.db'),{readOnly:true})
  const parent=db.prepare('SELECT run FROM host_requests WHERE key=?').get(request.turnId).run
  const children=db.prepare("SELECT reference FROM host_events WHERE kind='agent_delegation' AND json_extract(reference,'$.run_id')=? ORDER BY revision").all(parent).map(r=>JSON.parse(r.reference))
  assert.equal(children.length,5);assert.equal(new Set(children.map(c=>c.child_session)).size,5);assert.equal(new Set(children.map(c=>c.child_run)).size,5)
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
    await page.screenshot({path:join(output,`t6-actor-${person.id}.png`),fullPage:true})
    await page.getByRole('button',{name:'历史',exact:true}).click();await page.getByRole('button',{name:'返回目录',exact:true}).click();await dialog.waitFor()
    await page.getByRole('button',{name:'关闭详情',exact:true}).click();actors.push({request:actual,response:body})
    await page.locator('.runtime-person').filter({hasText:person.name}).click();await dialog.waitFor()
    const text=await dialog.innerText();assert.ok(text.includes(person.name))
    await page.screenshot({path:join(output,`t6-sidebar-${person.id}.png`),fullPage:true})
    members.push({person:person.id,text});await page.getByRole('button',{name:'关闭详情',exact:true}).click()
  }
  assert.equal(new Set(actors.map(a=>a.request.actorId)).size,6);assert.equal(members.length,6)
  report.t6={request,parent,children,handoffs,actors,members,final,scene:snapshot,model:'deterministic real HTTP Provider; animation pacing only in test fixture; real Host execution'}
  report.assertions.push('T6 six actual Pixi actor clicks and six sidebar members show the correct scoped identity with return navigation','T6 real parent + five independent Session/Run/JSONL with real read_file and Provider HTTP calls','T6 five task.assigned events cause exactly five observed office.visit completions; each worker presents its real current task','T6 whiteboard bounded business goal and true 5/5; no per-call task cards')
}


// Browser transport loss is injected only in its isolated context; Host/Provider continue their real Run.
export async function networkProduct({page,context,report,output,readOffice,conv,latestSend,fresh,send,waitFor,release,interrupt,restore}) {
  await fresh(page);await send('T6主动断网续接')
  const request=structuredClone(latestSend().request)
  const log=page.getByRole('log',{name:'对话内容'})
  await waitFor(async()=>(await log.innerText()).includes('断网前已保存的真实文本。'))
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
  await page.getByRole('button',{name:'重新同步',exact:true}).click()
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
  report.network={request,parent,requestsBefore,requestsAfter:report.modelRequests.length,transcript,final,trigger:'Chromium offline + SIGKILL isolated Office forwarding process; Host and Provider remain alive'}
  report.assertions.push('T6 actual Chromium offline loss detaches; public manual history/resume keeps one persisted Run, one model request and nonduplicated text')
  db.close()
}
