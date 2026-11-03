// Actual production Office + Host + Chromium. Only the external model/image APIs
// are deterministic HTTP fixtures. Never uses the user's data or credentials.
import assert from 'node:assert/strict'
import { spawn, execFileSync } from 'node:child_process'
import { createServer } from 'node:http'
import { once } from 'node:events'
import { mkdir, readFile, writeFile, copyFile, access } from 'node:fs/promises'
import { resolve, join, dirname } from 'node:path'
import { pathToFileURL, fileURLToPath } from 'node:url'
import { createHash } from 'node:crypto'
const root = resolve(dirname(fileURLToPath(import.meta.url)), '../..')
const office = resolve(process.env.OPENPAW_OFFICE_SOURCE ?? '')
const binary = resolve(process.env.OPENPAW_HOST_BINARY ?? join(root, 'target/debug/openpaw'))
const output = resolve(process.env.OPENPAW_CONTINUOUS_OUTPUT ?? `/tmp/openpaw-continuous-${Date.now()}`)
assert.ok(process.env.OPENPAW_OFFICE_SOURCE, 'Read-only, prebuilt frozen Office checkout required')
assert.ok(!output.startsWith(root + '/'), 'Evidence must be outside the repository')
await mkdir(output, { recursive: false })
const report = { startedAt: new Date().toISOString(), commit: execFileSync('git',['rev-parse','HEAD'],{cwd:root}).toString().trim(), dirty: true, provider: 'deterministic HTTP fixture, actual AgentLoop/tools', screenshots: [], model: [], snapshots: [], errors: [], assertions: [], result:'running' }
const processes = [], servers = []
let browser, context, page, phase='finite', leadCalls=0, childActive=0, peak=0, images=0, releaseFinal
const finalGate = new Promise(r => {releaseFinal=r})
const png=Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jRZkAAAAASUVORK5CYII=','base64')
const roles=['research','organize','draft','review','audit']
const batch=[]
const delay=ms=>new Promise(r=>setTimeout(r,ms))
async function wait(fn, timeout=45000) {const end=Date.now()+timeout;while(Date.now()<end){const v=await fn();if(v)return v;await delay(100)}throw new Error('Timed out waiting for actual state')}
async function listen(server) {servers.push(server);server.listen(0,'127.0.0.1');await once(server,'listening');return `http://127.0.0.1:${server.address().port}`}
function start(cmd,args,cwd,env,label) {
  const p=spawn(cmd,args,{cwd,env,stdio:['ignore','pipe','pipe']});let stdout='',stderr=''
  p.stdout.on('data',b=>stdout+=b);p.stderr.on('data',b=>stderr+=b)
  p.on('error',e=>report.errors.push(`${label}: ${e.message}`))
  const closed=once(p,'close');const item={p,closed,stdout:()=>stdout,save:()=>Promise.all([writeFile(join(output,label+'.stdout.log'),stdout),writeFile(join(output,label+'.stderr.log'),stderr)])};processes.push(item);return item
}
function finish(res,delta,reason='stop') {
  res.writeHead(200,{'content-type':'text/event-stream'})
  res.write('data: '+JSON.stringify({choices:[{index:0,delta,finish_reason:null}]})+'\n\n')
  res.write('data: '+JSON.stringify({choices:[{index:0,delta:{},finish_reason:reason}],usage:{prompt_tokens:100,completion_tokens:30}})+'\n\n')
  res.end('data: [DONE]\n\n')
}
function tool(res,id,name,args) {finish(res,{tool_calls:[{index:0,id,type:'function',function:{name,arguments:JSON.stringify(args)}}]},'tool_calls')}
async function screenshot(name) {await page.screenshot({path:join(output,name),fullPage:true});report.screenshots.push(name)}
// Generic asynchronous work-item journey; all article content is test data.
let pipelineNext=0, pipelinePeakItems=0
const pipelineTimeline=[]
function toolResults(data){return new Map(data.messages.filter(m=>m.role==='tool').map(m=>[m.tool_call_id,m.content]))}
function receipt(map,id){
  const value=JSON.parse(map.get(id).split('\n\n[Host执行回执]')[0])
  if(value.task_id){assert.equal(typeof value.title,'string');assert.ok(value.title.length>0);assert.ok(roles.includes(value.role_id));assert.ok(Array.isArray(value.available_tools))}
  if(value.tasks){
    assert.ok(Array.isArray(value.pending_task_ids))
    assert.deepEqual(value.pending_task_ids,value.tasks.filter(t=>!['completed','failed','cancelled','timed_out','interrupted'].includes(t.state)).map(t=>t.task_id))
    for(const task of value.tasks){assert.equal(typeof task.title,'string');assert.ok(Array.isArray(task.available_tools))}
  }
  return value
}
function pipelineDelegate(map, id, role, item, source, instructions) {
  if(source){assert.equal(receipt(map,source).work_item,item,'成果来源不能串入另一个事项');assert.ok(receipt(map,source).title.startsWith(item+' · '))}
  return {id,name:'delegate_task',args:{role_id:role,work_item:item,wait_for_completion:false,title:`${item} · ${instructions}`,instructions:`事项${item}，${instructions}。所有文件在outputs/${item}/；只处理该事项。`,acceptance:'实际文件和有限结果，不代他人声明完成',...(source?{source_task_id:receipt(map,source).task_id,context:`前序真实成果在outputs/${item}/，先用read_file读取。`}:{})}}
}
async function pipelineModel(data,res,role) {
  const results=toolResults(data)
  if(role!=='lead') {
    const user=data.messages.find(m=>m.role==='user').content
    const item=String(user).includes('事项A')?'A':'B'
    const path=`outputs/${item}/`
    const n=results.size
    const revision=String(user).includes('修订正文')
    const activity=(name,args)=>tool(res,`${item}-${role}-${n}`,name,args)
    const article=`# ${item}：可恢复的协作\n\n把有限任务交给明确负责人，保留真实工具结果。任务接受不等于成果完成；需要核对文件及交接依赖。审查只读实际稿件，问题交回对应负责人。\n`
    if(role==='organize') {
      if(n===0){activity('write_file',{path:path+'outline.md',content:`# ${item}提纲\n目标：说明可靠协作；章节：任务、依赖、复核。`});return}
    } else if(role==='draft'&&revision) {
      if(n===0){activity('read_file',{path:path+'article.md'});return}
      if(n===1){activity('edit_file',{path:path+'article.md',old_string:'可恢复的协作',new_string:'可恢复且可审计的协作'});return}
    } else if(role==='draft') {
      if(n===0){activity('read_file',{path:path+'outline.md'});return}
      if(n===1){activity('write_file',{path:path+'article.md',content:article});return}
    } else if(role==='research') {
      if(n===0){activity('read_file',{path:path+'article.md'});return}
      if(n===1){activity('render_visual',{path:path+'diagram.svg',kind:'diagram',title:'真实交接',nodes:[{id:'a',label:'提纲'},{id:'b',label:'正文'},{id:'c',label:'复核'}],edges:[{from:'a',to:'b'},{from:'b',to:'c'}]});return}
      if(n===2){activity('edit_file',{path:path+'article.md',old_string:article,new_string:article+'\n![协作流程](diagram.svg)\n\n图源：本地结构化SVG示意，不是照片。\n'});return}
    } else {
      if(n===0){activity('read_file',{path:path+'article.md'});return}
      if(role==='audit'&&n===1){activity('read_file',{path:path+'diagram.svg'});return}
      if(n===(role==='audit'?2:1)){activity('write_file',{path:path+role+'.md',content:role==='audit'?'已核对合稿和真实SVG相对链接；仅示意不构成外部实证。':'已读实际正文，结构与主题一致；最终图文由复核再次检查。'});return}
    }
    // Real model request remains in flight so the actual browser can observe
    // the task/action and delivery animation. No Host execution delay is added.
    await delay(role==='audit'?7500:5500)
    pipelineTimeline.push({at:Date.now(),item,role,event:'result'})
    finish(res,{content:`${item} ${role}：真实产物已保存，有限工作完成。`});return
  }
  const n=pipelineNext++
  const task=id=>receipt(results,id).task_id
  const commands=[
    {id:'p-start',name:'continuous_task',args:{action:'start',goal:'两篇短文图文合稿、真实交错协作',summary:'启动两事项演示',completion_condition:'两篇图文合稿及复核完成'}},
    {id:'p-open-a',name:'continuous_task',args:{action:'open_item',item_id:'A',summary:'开放第一篇'}},
    {id:'p-open-b',name:'continuous_task',args:{action:'open_item',item_id:'B',summary:'开放第二篇'}},
    {id:'p-open-c',name:'continuous_task',args:{action:'open_item',item_id:'C',summary:'第三项须被上限拒绝'}},
    ()=>{assert.match(results.get('p-open-c'),/capacity_exceeded/);return pipelineDelegate(results,'oa','organize','A',null,'选题并写提纲')},
    ()=>({id:'wait-oa',name:'delegation_status',args:{task_ids:[task('oa')],wait_for:'all'}}),
    ()=>pipelineDelegate(results,'da','draft','A','oa','根据提纲写正文'),
    ()=>pipelineDelegate(results,'ob','organize','B',null,'选题并写提纲'),
    ()=>({id:'wait-da-ob',name:'delegation_status',args:{task_ids:[task('da'),task('ob')],wait_for:'all'}}),
    ()=>pipelineDelegate(results,'ra','research','A','da','绘图并嵌入原稿'),
    ()=>pipelineDelegate(results,'va','review','A','da','审查实际正文'),
    ()=>pipelineDelegate(results,'db','draft','B','ob','根据提纲写正文'),
    ()=>({id:'wait-a-b',name:'delegation_status',args:{task_ids:[task('ra'),task('va'),task('db')],wait_for:'all'}}),
    ()=>pipelineDelegate(results,'aa','audit','A','va','复核最终图文合稿'),
    ()=>pipelineDelegate(results,'rb','research','B','db','绘图并嵌入原稿'),
    ()=>pipelineDelegate(results,'vb','review','B','db','审查实际正文'),
    ()=>({id:'wait-aa',name:'delegation_status',args:{task_ids:[task('aa')],wait_for:'all'}}),
    ()=>pipelineDelegate(results,'fix-a','draft','A','aa','修订正文并保留图文链接'),
    ()=>({id:'wait-fix-a',name:'delegation_status',args:{task_ids:[task('fix-a')],wait_for:'all'}}),
    ()=>pipelineDelegate(results,'audit-again','audit','A','fix-a','复核修订后图文合稿'),
    ()=>({id:'wait-audit-again',name:'delegation_status',args:{task_ids:[task('audit-again')],wait_for:'all'}}),
    {id:'p-close-a',name:'continuous_task',args:{action:'close_item',item_id:'A',summary:'第一篇图文合稿复核完成'}},
    ()=>({id:'wait-rb-vb',name:'delegation_status',args:{task_ids:[task('rb'),task('vb')],wait_for:'all'}}),
    ()=>pipelineDelegate(results,'ab','audit','B','vb','复核最终图文合稿'),
    ()=>({id:'wait-ab',name:'delegation_status',args:{task_ids:[task('ab')],wait_for:'all'}}),
    {id:'p-close-b',name:'continuous_task',args:{action:'close_item',item_id:'B',summary:'第二篇图文合稿复核完成'}},
    {id:'p-finish',name:'continuous_task',args:{action:'finish',summary:'两篇短文实际合稿完成，五位同事各有有限任务成果'}},
  ]
  if(n>=commands.length){finish(res,{content:'两篇短文已图文合稿并复核完成。'});return}
  const c=typeof commands[n]==='function'?commands[n]():commands[n]
  pipelineTimeline.push({at:Date.now(),event:'lead',name:c.name,args:c.args})
  tool(res,c.id,c.name,c.args)
}
// Each fixture response still travels over real HTTP into the original AgentLoop.
let queueNext=0, networkCalls=0, quotaCalls=0, releaseHealthyChild
const healthyChildGate = new Promise(r => { releaseHealthyChild=r })
const queueTimeline=[], queueActive=new Map(), queuePeak=new Map()
async function queuedModel(data,res,role) {
  const results=toolResults(data)
  if(role==='lead') {
    const n=queueNext++
    if(n<21){tool(res,'queue-'+n,'delegate_task',{role_id:n%3===0?'review':'draft',title:`队列任务${n}`,instructions:`只处理独立待办 queue-${n}，返回一句实际结果，不写文件。`,acceptance:'真实有限结果',wait_for_completion:false});return}
    if(n===21||n===22){
      const first=n===21?0:16, length=n===21?16:5
      tool(res,'queue-wait-'+n,'delegation_status',{task_ids:Array.from({length},(_,i)=>receipt(results,'queue-'+(first+i)).task_id),wait_for:'all'});return
    }
    for(const id of ['queue-wait-21','queue-wait-22']) {
      const result=receipt(results,id);assert.equal(result.pending_task_ids.length,0)
      const first=id==='queue-wait-21'?0:16
      for(const [index,task] of result.tasks.entries()){
        assert.equal(task.state,'completed');assert.equal(task.title,`队列任务${first+index}`)
        assert.equal(task.role_id,(first+index)%3===0?'review':'draft')
      }
    }
    finish(res,{content:'21项待办均已收取真实成果；同一成员依次处理，其他成员并行。'});return
  }
  const user=data.messages.find(m=>m.role==='user').content
  const id=Number(String(user).match(/queue-(\d+)/)?.[1]);assert.ok(Number.isInteger(id))
  const active=(queueActive.get(role)??0)+1;queueActive.set(role,active);queuePeak.set(role,Math.max(queuePeak.get(role)??0,active))
  queueTimeline.push({kind:'start',role,id,at:Date.now()})
  await delay(id<3?8000:300)
  queueTimeline.push({kind:'end',role,id,at:Date.now()});queueActive.set(role,active-1)
  finish(res,{content:`队列任务${id}的真实有限结果。`})
}
try {
  const manifest=JSON.parse(await readFile(join(root,'docs/engine-host/contracts/sources.json'),'utf8'))
  report.officeExpectedCommit=manifest.office.commit
  try {await access(join(office,'.git'));report.officeCommit=execFileSync('git',['rev-parse','HEAD'],{cwd:office}).toString().trim();assert.equal(report.officeCommit,manifest.office.commit)} catch(e) {if(e.code!=='ENOENT')throw e;report.officeCommit=null;report.officeSourceKind='prebuilt source export, verified pinned source hashes below'}
  report.officeServerSha256=createHash('sha256').update(await readFile(join(office,'server/dist/app.js'))).digest('hex')
  for(const item of manifest.office.files) assert.equal(createHash('sha256').update(await readFile(join(office,item.path))).digest('hex'),item.sha256)
  const api=await listen(createServer(async(req,res)=>{
    try {
      let body='';for await(const b of req){body+=b;assert.ok(body.length<4*1024*1024)}const data=JSON.parse(body)
      if(req.url==='/v1/images/generations') {
        assert.equal(req.headers.authorization,'Bearer fixture-image-only');assert.equal(data.model,'image-fixture')
        images++;res.writeHead(200,{'content-type':'application/json'});res.end(JSON.stringify({data:[{b64_json:png.toString('base64')}]}));return
      }
      assert.equal(req.url,'/v1/chat/completions');assert.equal(req.headers.authorization,'Bearer fixture-chat-only')
      const role=data.messages.filter(m=>m.role==='system').map(m=>m.content?.match(/本轮可信角色：[^\n]*（(lead|research|organize|draft|review|audit)）/)?.[1]).find(Boolean)
      report.model.push({at:new Date().toISOString(),role,phase,tools:data.tools?.map(t=>t.function.name),messages:data.messages})
      if(!data.tools?.length){finish(res,{content:'保留原持续目标、产物目录和下一步。'});return}
      if(phase==='pipeline'){await pipelineModel(data,res,role);return}
      if(phase==='queue'){await queuedModel(data,res,role);return}
      if(phase==='child-error') {
        const results=toolResults(data)
        if(role==='lead') {
          if(results.size===0){finish(res,{tool_calls:['research','review'].map((r,index)=>({index,id:'error-delegate-'+r,type:'function',function:{name:'delegate_task',arguments:JSON.stringify({role_id:r,title:r==='research'?'核查资料来源':'整理已知资料',instructions:'只处理已给资料，返回有限结果，不写文件',acceptance:'实际结果及未完成原因'})}}))},'tool_calls');return}
          finish(res,{content:'刘市已完成整理；李研的来源核查因模型响应中断未完成，已有成果保留。'});return
        }
        res.writeHead(200,{'content-type':'text/event-stream'})
        res.write('data: '+JSON.stringify({choices:[{index:0,delta:{content:role==='research'?'来源核查的部分结果已返回。':'正在整理给定资料。'},finish_reason:null}]})+'\n\n')
        if(role==='research') {setTimeout(()=>res.destroy(),500);return}
        const beat=setInterval(()=>res.write('data: '+JSON.stringify({choices:[{index:0,delta:{content:'继续整理。'},finish_reason:null}]})+'\n\n'),500)
        res.on('close',()=>clearInterval(beat))
        await healthyChildGate;clearInterval(beat)
        res.write('data: '+JSON.stringify({choices:[{index:0,delta:{},finish_reason:'stop'}],usage:{prompt_tokens:100,completion_tokens:30}})+'\n\n');res.end('data: [DONE]\n\n');return
      }
      if(phase==='quota'){
        quotaCalls++
        res.writeHead(402,{'content-type':'application/json'})
        res.end(JSON.stringify({error:{type:'insufficient_balance_error',message:'Insufficient Balance NEVER_SHOW_QUOTA_SECRET'}}));return
      }
      if(phase==='network'){
        networkCalls++
        if(networkCalls===1){tool(res,'before-network','think',{thought:'保留已完成的分析事实'});return}
        // Deliberately break the real upstream TCP connection; no fabricated Host error.
        req.socket.destroy();return
      }
      if(role&&role!=='lead') {
        const results=data.messages.filter(m=>m.role==='tool')
        if(phase!=='stop') {
          assert.ok(results.every(r=>!r.content.includes('invalid_progress')), 'Actual media progress rejected')
          if(results.length===0){tool(res,'plan-'+role,'report_progress',{type:'create',steps:[{id:'media',title:`保存 outputs/${role}.svg`}]});return}
          if(results.length===2){assert.ok(results.some(r=>r.content.includes('saved')));tool(res,'confirmed-'+role,'report_progress',{type:'update',stepId:'media',state:'completed',evidenceCallIds:['svg-'+role]});return}
          if(results.length>=3){finish(res,{content:role+'实际图表已保存并完成核查。'});return}
        }
        childActive++;peak=Math.max(peak,childActive)
        res.once('close',()=>{childActive--})
        if(phase==='stop') {res.writeHead(200,{'content-type':'text/event-stream'});res.write('data: '+JSON.stringify({choices:[{index:0,delta:{content:'正在执行独立成员工作，等待停止验收。'},finish_reason:null}]})+'\n\n');return}
        batch.push({role,res})
        if(batch.length===5) for(const {role,res} of batch.splice(0)) tool(res,'svg-'+role,'render_visual',{path:`outputs/${role}.svg`,kind:'diagram',title:role+'核查',nodes:[{id:'a',label:'获取资料'},{id:'b',label:'核对产物'},{id:'c',label:'保存成果'}],edges:[{from:'a',to:'b',label:'复核'},{from:'b',to:'c',label:'可重试'}]})
        return
      }
      assert.equal(role,'lead');const n=leadCalls++
      if(n===0){const args={action:'start',goal:'持续协作验收',summary:'开始持续协作，保存真实产物',next:'分派五位成员'};if(phase==='finite')args.completion_condition='完成五人核查、图片和文件后停止';tool(res,'start-'+phase,'continuous_task',args);return}
      if(phase==='stop'&&n===1){tool(res,'stale-startup-plan','report_progress',{type:'create',steps:[{id:'startup',title:'启动期固定步骤不应遮住后续持续事项'}]});return}
      if(n===1||phase==='stop'&&n===2){finish(res,{tool_calls:roles.map((r,i)=>({index:i,id:'delegate-'+phase+'-'+r,type:'function',function:{name:'delegate_task',arguments:JSON.stringify({role_id:r,title:r+'资料核查',instructions:'使用render_visual在outputs下保存本角色独立图表，并返回实际路径。',acceptance:'真实文件及本角色实际结果'})}}))},'tool_calls');return}
      if(phase==='stop'){finish(res,{content:'持续推进直到用户停止'});return}
      if(n===2){tool(res,'parent-plan','report_progress',{type:'create',steps:roles.map(r=>({id:r,title:`保存 outputs/${r}.svg`}))});return}
      if(n===3){finish(res,{tool_calls:roles.map((r,i)=>({index:i,id:'verify-'+r,type:'function',function:{name:'report_progress',arguments:JSON.stringify({type:'update',stepId:r,state:'completed',evidenceCallIds:['delegate-finite-'+r]})}}))},'tool_calls');return}
      if(n===4){tool(res,'checkpoint-a','continuous_task',{action:'checkpoint',summary:'五位成员已完成真实图表，进入汇总阶段',next:'保存汇总与真实图片'});return}
      if(n===5){finish(res,{content:'第一阶段结束，仍需自动执行下一阶段'});return}
      if(n===6){tool(res,'actual-write','write_file',{path:'outputs/result.md',content:'# 持续协作验收\n\n五位成员已完成真实SVG产物。\n'});return}
      if(n===7){tool(res,'actual-image','generate_image',{path:'outputs/actual.png',prompt:'HTTP契约验收图片，不是商业模型生图验收'});return}
      if(n===8){tool(res,'checkpoint-b','continuous_task',{action:'checkpoint',summary:'第二阶段自动执行完成，汇总文件和实际图片均已保存',next:'完成有限验收目标'});return}
      if(n===9){await finalGate;tool(res,'finish-finite','continuous_task',{action:'finish',summary:'有限目标已全部完成，保留五人、汇总和图片成果'});return}
      finish(res,{content:'验收已完成'})
    } catch(e){report.errors.push(e.stack);if(!res.headersSent)res.writeHead(500);res.end('fixture failed')}
  }))
  const portProbe=createServer();const probe=await listen(portProbe);const port=new URL(probe).port;await new Promise(r=>portProbe.close(r))
  // Fault injection is confined to this fixture transport. Office source and
  // the model execution path are unchanged; verify a failed version is retried.
  let rejectedSnapshot=null, retryVerified=false
  const relayProbe=await listen(createServer(async(req,res)=>{
    const chunks=[];for await(const chunk of req)chunks.push(chunk)
    const body=Buffer.concat(chunks);let message
    try{message=JSON.parse(body.toString())}catch{}
    if(message?.type==='office.snapshot'&&message.snapshot.conversations.length) {
      const digest=createHash('sha256').update(body).digest('hex')
      if(!rejectedSnapshot){
        rejectedSnapshot={seq:message.snapshot.seq,digest,bytes:body.length}
        report.snapshotRetry={rejected:rejectedSnapshot}
        res.writeHead(503,{'content-type':'application/json'});res.end(JSON.stringify({code:503,msg:'Fixture transport rejection',data:{reasonCode:'RECEIVER_UNAVAILABLE'}}));return
      }
      if(!retryVerified){
        try {assert.equal(message.snapshot.seq,rejectedSnapshot.seq);assert.equal(digest,rejectedSnapshot.digest);retryVerified=true;report.snapshotRetry.retried={seq:message.snapshot.seq,digest,bytes:body.length}}
        catch(error){report.errors.push(error.stack)}
      }
    }
    try{
      const response=await fetch(probe+req.url,{method:req.method,headers:{authorization:req.headers.authorization??'','content-type':'application/json'},...(body.length?{body}:{} )})
      res.writeHead(response.status,{'content-type':response.headers.get('content-type')??'application/json'});res.end(Buffer.from(await response.arrayBuffer()))
    }catch{res.writeHead(502,{'content-type':'application/json'});res.end(JSON.stringify({code:502,msg:'Fixture upstream unavailable',data:{reasonCode:'RECEIVER_UNAVAILABLE'}}))}
  }))
  const env={...process.env,OPENPAW_HOME:join(output,'model-home'),PAWOFFICE_API_KEY:'fixture-office-only'}
  await mkdir(env.OPENPAW_HOME);const workspace=join(output,'workspace');await mkdir(workspace)
  const envFile=join(env.OPENPAW_HOME,'.env');await writeFile(envFile,'OPENAI_API_KEY=fixture-chat-only\nOPENPAW_TEST_IMAGE_KEY=fixture-image-only\n')
  // Remove inherited keys for these isolated fixtures; actual user values are never read or logged.
  delete env.OPENAI_API_KEY;delete env.OPENPAW_TEST_IMAGE_KEY
  const config=join(output,'host.toml')
  await writeFile(config,`home=${JSON.stringify(join(output,'host-data'))}\nprovider="openai"\nmodel="continuous-fixture"\nbase_url=${JSON.stringify(api+'/v1')}\napi_key_env="OPENAI_API_KEY"\nmax_active_runs=6\nrun_timeout_secs=0\ntools=["read_file","write_file","edit_file","render_visual","generate_image"]\n[roots]\nproject=${JSON.stringify(workspace)}\n[slots]\noffice-1="project"\n[agents]\nenabled=true\nmax_active_children=5\nmax_delegations=0\nmax_model_requests=0\nmax_output_tokens=0\n[agents.continuous]\nenabled=true\noutput_dir="outputs"\nauto_approve=true\nstage_pause_ms=30\n[agents.images]\nbase_url=${JSON.stringify(api+'/v1')}\nmodel="image-fixture"\napi_key_env="OPENPAW_TEST_IMAGE_KEY"\n[office]\nbase_url=${JSON.stringify(relayProbe)}\napi_key_env="PAWOFFICE_API_KEY"\nallowed_origins=[${JSON.stringify(probe)}]\n[office.names]\noffice-1="持续协作验收"\n`)
  const fixedBinary=join(output,process.platform==='win32'?'openpaw.exe':'openpaw');await copyFile(binary,fixedBinary)
  report.binarySha256=createHash('sha256').update(await readFile(fixedBinary)).digest('hex')
  const host=start(fixedBinary,['engine-host','--config',config,'--env-file',envFile,'--port','0'],root,env,'host')
  const hostBase=await wait(()=>host.stdout().match(/listening on (http:\/\/127\.0\.0\.1:\d+)/)?.[1])
  start(process.execPath,['server/dist/index.js'],office,{...env,PORT:port,HOST:'127.0.0.1',PAWOFFICE_PUBLIC_ORIGIN:probe},'office')
  await wait(async()=>{try{return(await fetch(probe+'/office/')).ok}catch{return false}})
  const {chromium}=await import(pathToFileURL(join(office,'node_modules/playwright-core/index.mjs')))
  browser=await chromium.launch({headless:true,...(process.env.OPENPAW_CHROMIUM?{executablePath:process.env.OPENPAW_CHROMIUM}:{}),args:['--use-angle=swiftshader','--enable-webgl','--enable-unsafe-swiftshader']})
  context=await browser.newContext({viewport:{width:1440,height:1050},locale:'zh-CN',recordVideo:{dir:join(output,'video')}})
  await context.tracing.start({screenshots:true,snapshots:true,sources:false});page=await context.newPage()
  const sends=[];page.on('pageerror',e=>report.errors.push(e.message));page.on('request',r=>{if(r.url()===probe+'/api/pawoffice/chat'&&r.method()==='POST'){const data=r.postDataJSON();if(data.type==='send')sends.push(data)}})
  const readOffice=async()=>{const r=await fetch(probe+'/api/pawoffice/messages',{method:'POST',headers:{authorization:'Bearer fixture-office-only','content-type':'application/json'},body:JSON.stringify({type:'office.read',teamId:'office-1'})});assert.equal(r.status,200);return(await r.json()).data.snapshot}
  async function send(text){const before=sends.length;const expand=page.getByRole('button',{name:'展开对话',exact:true});if(await expand.isVisible())await expand.click();await page.getByRole('button',{name:'新会话',exact:true}).first().click();const input=page.getByRole('textbox',{name:'给办公室发送消息'});await input.fill(text);await page.getByRole('button',{name:'发送消息',exact:true}).click();await wait(()=>sends.length>before)}
  await page.goto(probe+'/office/',{waitUntil:'domcontentloaded'})
  await page.getByText(/正在加载办公室/).waitFor({state:'hidden',timeout:60000})
  await page.getByRole('button',{name:/^团队空间，当前团队/}).click();await page.getByRole('button',{name:'进入持续协作验收',exact:true}).click();await page.getByRole('dialog').waitFor({state:'hidden'})
  await send('组织五位成员持续协作，保存图表、汇总和图片，完成这批有限验收目标后停止。')
  const first=sends.at(-1).conversationId
  await wait(()=>leadCalls>=10);assert.equal(sends.length,1);assert.equal(peak,5);assert.equal(images,1)
  const active=await wait(async()=>{const s=await readOffice();return s.conversations.some(c=>c.id===first)?s:false});report.snapshots.push({phase:'before-finish',snapshot:active});assert.ok(active.conversations.some(c=>c.id===first))
  await screenshot('01-stage-two-continued.png')
  const svg=await readFile(join(workspace,'outputs/research.svg'),'utf8');assert.ok(svg.includes('<svg'))
  // Inspect the actual tool output in Chromium, including text layout; parsing SVG alone misses occlusion.
  const visualPage=await context.newPage()
  await visualPage.setContent(svg)
  report.visual=await visualPage.evaluate(()=>{
    const nodes=[...document.querySelectorAll('g > rect')].map(n=>{const b=n.getBBox();return {x:b.x,y:b.y,width:b.width,height:b.height}})
    return [...document.querySelectorAll('text')].filter(n=>['复核','可重试'].includes(n.textContent)).map(n=>{
      const b=n.getBBox();return {label:n.textContent,x:b.x,y:b.y,width:b.width,height:b.height,overlaps:nodes.filter(r=>b.x<r.x+r.width&&b.x+b.width>r.x&&b.y<r.y+r.height&&b.y+b.height>r.y)}
    })
  })
  await visualPage.screenshot({path:join(output,'svg-rendered.png'),fullPage:true});report.screenshots.push('svg-rendered.png')
  assert.equal(report.visual.length,2)
  assert.ok(report.visual.every(v=>v.overlaps.length===0),'Actual edge label bounds overlap a node: '+JSON.stringify(report.visual))
  await visualPage.close()
  for(const r of roles)assert.ok((await readFile(join(workspace,`outputs/${r}.svg`),'utf8')).includes(r))
  assert.deepEqual(await readFile(join(workspace,'outputs/actual.png')),png)
  assert.match(await readFile(join(workspace,'outputs/result.md'),'utf8'),/五位成员/)
  releaseFinal();await page.getByRole('button',{name:'停止生成',exact:true}).waitFor({state:'hidden',timeout:45000})
  const done=await wait(async()=>{const s=await readOffice();const c=s.conversations.find(c=>c.id===first);return c?.tasks.length>=6&&['code-agent','file-agent','app-agent','review-agent','data-agent'].every(id=>c.tasks.some(t=>t.assigneeId===id&&t.status==='completed'&&t.progress===100))&&c.tasks.every(t=>t.status==='completed'&&t.progress===100)?s:false});report.snapshots.push({phase:'completed',snapshot:done})
  await screenshot('02-completed-tasks.png')
  const log=page.getByRole('log',{name:'对话内容'});assert.match(await log.innerText(),/第二阶段自动执行完成/);assert.ok(!(await log.innerText()).includes('回复未完成'))
  await page.reload({waitUntil:'domcontentloaded'});await page.getByText(/正在加载办公室/).waitFor({state:'hidden',timeout:60000})
  const expand=page.getByRole('button',{name:'展开对话',exact:true});if(await expand.isVisible())await expand.click()
  await wait(async()=>(await page.getByRole('log',{name:'对话内容'}).innerText()).includes('第二阶段自动执行完成'));assert.equal(sends.length,1)
  await screenshot('03-refresh-recovered.png')
  for(const name of ['李研','周理','陈书','刘市','赵审']) {
    await page.locator('.runtime-person').filter({hasText:name}).click()
    const dialog=page.getByRole('dialog',{name:name+' · 成员详情',exact:true});await dialog.waitFor()
    const text=await dialog.innerText();assert.ok(text.includes('100%')&&text.includes('已核验完成'),text)
    await screenshot('member-'+name+'.png')
    await dialog.getByRole('button',{name:'文件产物',exact:true}).click()
    const files=page.getByRole('dialog',{name:'真实文件产物',exact:true});await files.waitFor()
    const role=roles[['李研','周理','陈书','刘市','赵审'].indexOf(name)]
    await wait(async()=>(await files.innerText()).includes(`outputs/${role}.svg`))
    await screenshot('artifact-'+role+'.png')
    // 文件详情关闭后可能恢复成员详情；按真实可见关闭按钮退出所有层，
    // 再点击下一成员，不能强制点击穿透modal来伪装用户操作。
    for(let layer=0;layer<3&&await page.getByRole('dialog').count();layer++) {
      await page.getByRole('button',{name:'关闭详情',exact:true}).click()
      await delay(200)
    }
    await wait(async()=>await page.getByRole('dialog').count()===0)
  }
  report.assertions.push('One UI send resumed original AgentLoop across EndTurn; five real child Providers concurrently executed; five SVGs, real image HTTP bytes and markdown saved; official .env provided independent credentials; all tasks completed and refresh restored checkpoint without another send')
  phase='pipeline';pipelineNext=0
  await send('请五位同事交错完成两篇短文，最多同时开放两篇；真实交接，图表嵌入正文。完成这两篇后停止。')
  const pipelineConversation=sends.at(-1).conversationId
  await page.getByRole('button',{name:'收起聊天',exact:true}).click()
  const assignments=new Map();let sampledOverlap=false;let observedActions=false;let observedCompleted=false
  const pipelineDeadline=Date.now()+180000
  while(Date.now()<pipelineDeadline){
    const snapshot=await readOffice();const conv=snapshot.conversations.find(c=>c.id===pipelineConversation)
    if(conv){
      report.snapshots.push({phase:'pipeline',at:Date.now(),snapshot})
      const working=conv.tasks.filter(t=>t.status==='running')
      if(working.some(t=>t.title.startsWith('A ·'))&&working.some(t=>t.title.startsWith('B ·')))sampledOverlap=true
      if(conv.tasks.some(t=>/正在读取|正在保存|正在绘制|刚完成/.test(t.summary??'')))observedActions=true
      assert.ok(conv.tasks.filter(t=>t.status==='completed').every(t=>t.progress===100),'Completed task must never display zero')
      observedCompleted ||= conv.tasks.some(t=>t.assigneeId!=='office-lead'&&t.status==='completed'&&t.progress===100)
      for(const event of conv.events.filter(e=>e.kind==='task.assigned')){
        if(!assignments.has(event.id)){assignments.set(event.id,event);await screenshot(`pipeline-handoff-${assignments.size}.png`)}
      }
      if(conv.tasks.length>=11&&conv.tasks.every(t=>t.status==='completed'&&t.progress===100))break
    }
    await delay(500)
  }
  const ps=await readOffice();const pc=ps.conversations.find(c=>c.id===pipelineConversation)
  assert.ok(pc?.tasks.length>=11&&pc.tasks.every(t=>t.status==='completed'&&t.progress===100),'Two-article final tasks did not converge')
  assert.equal(assignments.size,12);assert.ok(sampledOverlap,'Browser never saw two articles concurrently working');assert.ok(observedActions);assert.ok(observedCompleted)
  const pairs=[...assignments.values()].map(e=>[e.fromEmployeeId,e.employeeId])
  assert.ok(pairs.some(([a,b])=>a==='file-agent'&&b==='app-agent'))
  assert.ok(pairs.some(([a,b])=>a==='app-agent'&&b==='code-agent'))
  assert.ok(pairs.some(([a,b])=>a==='app-agent'&&b==='review-agent'))
  assert.ok(pairs.some(([a,b])=>a==='data-agent'&&b==='app-agent'))
  report.pipeline={assignments:[...assignments.values()],timeline:pipelineTimeline,sampledOverlap,observedActions,observedCompleted}
  for(const item of ['A','B']){
    const md=await readFile(join(workspace,`outputs/${item}/article.md`),'utf8');assert.match(md,/!\[协作流程\]\(diagram\.svg\)/)
    const svg=await readFile(join(workspace,`outputs/${item}/diagram.svg`),'utf8');assert.ok(svg.startsWith('<svg'))
    // Render the same actual Markdown text and image bytes for visual artifact QA.
    const artifact=await context.newPage();await artifact.setContent(`<h1>${item}图文合稿</h1><pre></pre>${svg}`);await artifact.locator('pre').textContent();await artifact.locator('pre').evaluate((el,text)=>el.textContent=text,md)
    await artifact.screenshot({path:join(output,`pipeline-${item}-combined.png`),fullPage:true});await artifact.close()
  }
  const download=page.waitForEvent('download');await page.getByRole('button',{name:'导出快照',exact:true}).click();const file=await download;await file.saveAs(join(output,'pipeline-scene.json'))
  const scene=JSON.parse(await readFile(join(output,'pipeline-scene.json'),'utf8'));report.pipeline.sceneVisits=scene.records.filter(r=>r.command.capability==='office.visit')
  report.pipeline.sceneVisitActors=[...new Set(report.pipeline.sceneVisits.flatMap(r=>r.command.participants?.filter(p=>p.role==='visitor').map(p=>p.entityId)??[]))]
  assert.ok(report.pipeline.sceneVisitActors.includes('file-agent')&&report.pipeline.sceneVisitActors.includes('app-agent'),'Actual Office runtime never received non-lead handoffs')
  await screenshot('pipeline-completed.png')
  assert.equal(sends.length,2)
  report.assertions.push('Two real work items overlapped; third item rejected; 12 actual child Runs across five roles; trusted source owners produced non-lead visit commands; real SVG embedded into both Markdown articles; all completed cards 100%; live actions sampled from actual tools')
  phase='queue'
  await send('给同一成员安排多项独立待办，同时让另一成员工作；逐项收取真实结果。')
  const queueConversation=sends.at(-1).conversationId
  const queueAssignments=new Map();let queuedObserved=false
  const queueDeadline=Date.now()+150000
  while(Date.now()<queueDeadline) {
    const snapshot=await readOffice(), c=snapshot.conversations.find(c=>c.id===queueConversation)
    if(c){
      report.snapshots.push({phase:'queue',at:Date.now(),snapshot})
      if(c.tasks.some(t=>t.status==='queued'))queuedObserved=true
      for(const e of c.events.filter(e=>e.kind==='task.assigned'))if(!queueAssignments.has(e.id)){queueAssignments.set(e.id,e);await screenshot(`queue-handoff-${queueAssignments.size}.png`)}
      if(queueAssignments.size===21&&c.tasks.length>=22&&c.tasks.every(t=>t.status==='completed'&&t.progress===100))break
    }
    await delay(350)
  }
  assert.equal(queueAssignments.size,21,'All 21 actual handoffs must be observed, not just ledger intents')
  assert.ok(queuedObserved,'Actual UI snapshot never exposed pending FIFO work')
  for(const role of ['draft','review']){
    assert.equal(queuePeak.get(role),1,'One person executed overlapping work')
    const actual=queueTimeline.filter(e=>e.kind==='start'&&e.role===role).map(e=>e.id)
    assert.deepEqual(actual,Array.from({length:21},(_,i)=>i).filter(i=>(i%3===0?'review':'draft')===role))
  }
  assert.ok(queueTimeline.find(e=>e.kind==='start'&&e.role==='draft').at<queueTimeline.find(e=>e.kind==='end'&&e.role==='review').at)
  report.queue={assignments:[...queueAssignments.values()],timeline:queueTimeline,peak:Object.fromEntries(queuePeak),queuedObserved}
  await screenshot('queue-all-completed.png')
  for(const name of ['陈书','刘市']){
    const side=page.getByRole('button',{name:'查看'+name+'的资料',exact:true})
    if(await side.count()){
      await side.click();await page.getByRole('button',{name:'查看完整档案',exact:true}).click()
    }else{await page.locator('.runtime-person').filter({hasText:name}).click()}
    const dialog=page.getByRole('dialog',{name:name+' · 成员详情',exact:true});await dialog.waitFor()
    assert.ok(!(await dialog.innerText()).includes('scope_denied'));await screenshot('queue-member-'+name+'.png');await dialog.getByRole('button',{name:/关闭/}).click()
  }
  report.assertions.push('21 real queued child Runs accepted without blocking lead; same member FIFO; other member parallel; all 21 actual Office assignment events observed; queued and completed100 shown; member details clicked')
  phase='network'
  await send('验证模型连接断开后能看到错误原因，并保留已完成事实。')
  const networkConversation=sends.at(-1).conversationId
  await wait(async()=>(await page.getByRole('log',{name:'对话内容'}).innerText()).includes('本轮未完成：连接模型服务失败'),45000)
  await screenshot('network-error-visible.png')
  const networkHistory=await page.getByRole('log',{name:'对话内容'}).innerText()
  assert.ok(networkCalls>1)
  await page.reload({waitUntil:'domcontentloaded'});await page.getByText(/正在加载办公室/).waitFor({state:'hidden',timeout:60000})
  const e=page.getByRole('button',{name:'展开对话',exact:true});if(await e.isVisible())await e.click()
  await wait(async()=>(await page.getByRole('log',{name:'对话内容'}).innerText()).includes('本轮未完成：连接模型服务失败'))
  await screenshot('network-error-after-refresh.png')
  report.network={conversationId:networkConversation,requests:networkCalls,chat:networkHistory,afterRefresh:await page.getByRole('log',{name:'对话内容'}).innerText()}
  report.assertions.push('Real upstream TCP break retried boundedly; actual Office chat shows a safe error in its body; reload retains same error; not a fabricated success')
  phase='quota'
  await send('验证模型账户余额不足时显示明确原因。')
  await wait(async()=>(await page.getByRole('log',{name:'对话内容'}).innerText()).includes('本轮未完成：模型服务账户余额或额度不足'))
  assert.equal(quotaCalls,1)
  const quotaChat=await page.getByRole('log',{name:'对话内容'}).innerText()
  assert.ok(!quotaChat.includes('NEVER_SHOW_QUOTA_SECRET'))
  await screenshot('quota-error-visible.png')
  await page.reload({waitUntil:'domcontentloaded'});await page.getByText(/正在加载办公室/).waitFor({state:'hidden',timeout:60000})
  const quotaExpand=page.getByRole('button',{name:'展开对话',exact:true});if(await quotaExpand.isVisible())await quotaExpand.click()
  await wait(async()=>(await page.getByRole('log',{name:'对话内容'}).innerText()).includes('本轮未完成：模型服务账户余额或额度不足'))
  assert.equal(quotaCalls,1)
  const restoredQuota=await page.getByRole('log',{name:'对话内容'}).innerText();assert.ok(!restoredQuota.includes('NEVER_SHOW_QUOTA_SECRET'))
  await screenshot('quota-error-after-refresh.png')
  report.quota={requests:quotaCalls,chat:quotaChat,afterRefresh:restoredQuota}
  report.assertions.push('Actual upstream HTTP402 appears as safe account balance/credit failure in Office chat and after reload; no raw upstream secret, retry or replay')
  phase='child-error'
  await send('请李研核查资料来源，刘市独立整理已知资料；若一位失败，另一位继续，不要重做已完成内容。')
  await wait(async()=>{const text=await page.getByRole('log',{name:'对话内容'}).innerText();return text.includes('子任务异常记录（主任务仍在运行）')&&text.includes('李研')&&text.includes('核查资料来源')&&text.includes('响应流读取失败')})
  assert.ok(await page.getByRole('button',{name:'停止生成',exact:true}).isVisible(),'子错误可见时主任务仍应运行')
  const childErrorActive=await page.getByRole('log',{name:'对话内容'}).innerText()
  await screenshot('child-error-visible-while-other-continues.png')
  releaseHealthyChild()
  await page.getByRole('button',{name:'停止生成',exact:true}).waitFor({state:'hidden',timeout:45000})
  await wait(async()=>(await page.getByRole('log',{name:'对话内容'}).innerText()).includes('刘市已完成整理'))
  const childErrorFinal=await page.getByRole('log',{name:'对话内容'}).innerText()
  assert.ok(childErrorFinal.includes('核查资料来源'))
  const childErrorRequests=report.model.length
  await page.reload({waitUntil:'domcontentloaded'});await page.getByText(/正在加载办公室/).waitFor({state:'hidden',timeout:60000})
  const childExpand=page.getByRole('button',{name:'展开对话',exact:true});if(await childExpand.isVisible())await childExpand.click()
  await wait(async()=>(await page.getByRole('log',{name:'对话内容'}).innerText()).includes('子任务异常记录'))
  assert.equal(report.model.length,childErrorRequests,'刷新不得重放失败同事')
  report.childError={active:childErrorActive,final:childErrorFinal,afterReload:await page.getByRole('log',{name:'对话内容'}).innerText()}
  await screenshot('child-error-retained-after-refresh.png')
  report.assertions.push('Actual child TCP stream failure reaches parent chat with member/task while another real child keeps streaming; parent then continues, refresh retains error without replay')
  const followupModelCount=report.model.length
  await page.getByRole('textbox',{name:'给办公室发送消息'}).fill('请说明刚才两位同事各自的结果与未完成事项，不重复操作。')
  await page.getByRole('button',{name:'发送消息',exact:true}).click()
  await wait(()=>report.model.length===followupModelCount+1,12000)
  await page.getByRole('button',{name:'停止生成',exact:true}).waitFor({state:'hidden',timeout:45000})
  await screenshot('same-conversation-followup.png')
  report.assertions.push('Same-conversation follow-up after a completed parent with child error is accepted exactly once, without replaying children or importing UI notices as model messages')
  phase='stop';leadCalls=0;peak=0
  await send('五位成员持续处理资料，直到我点击停止。')
  await wait(()=>childActive===5);assert.equal(peak,5)
  const unbounded=await wait(async()=>{const s=await readOffice();const c=s.conversations.find(c=>c.tasks.filter(t=>t.assigneeId!=='office-lead'&&t.status==='running').length===5);return c?{snapshot:s,conversation:c}:false})
  assert.ok(!unbounded.conversation.tasks.some(t=>t.title==='启动期固定步骤不应遮住后续持续事项'))
  assert.equal(unbounded.conversation.tasks.find(t=>t.id.startsWith('run:')).status,'running')
  assert.equal(await page.getByText('启动期固定步骤不应遮住后续持续事项',{exact:true}).count(),0)
  for(const task of unbounded.conversation.tasks.filter(t=>t.assigneeId!=='office-lead'&&t.status==='running')) {
    await wait(async()=>await page.getByText(task.title,{exact:true}).first().isVisible())
  }
  await screenshot('04-five-active-before-stop.png')
  report.uiPositiveStopTitles=unbounded.conversation.tasks.filter(t=>t.assigneeId!=='office-lead'&&t.status==='running').map(t=>t.title)
  assert.ok(retryVerified,'Rejected Office snapshot must be retried with the same seq and payload')
  report.assertions.push('Actual HTTP503 during snapshot delivery retries the exact persisted seq/payload before publishing newer facts')
  report.snapshots.push({phase:'unbounded-old-plan',snapshot:unbounded.snapshot})
  report.assertions.push('A real UI instruction started unbounded work with an obsolete fixed startup plan: five actual children remain visible and running, parent remains running, obsolete plan is not shown as the current workload; original plan evidence is retained')
  await page.getByRole('button',{name:'停止生成',exact:true}).click();await page.getByRole('button',{name:'停止生成',exact:true}).waitFor({state:'hidden',timeout:45000})
  await wait(()=>childActive===0);const count=report.model.length;await delay(300);assert.equal(report.model.length,count)
  report.snapshots.push({phase:'cancelled',snapshot:await readOffice()});assert.equal(sends.length,8)
  const health=await fetch(hostBase+'/health');assert.equal(health.status,200)
  await screenshot('05-group-cancelled.png');report.assertions.push('Second single UI instruction kept five real HTTP child streams active until actual UI stop, all streams closed and no further model request; Host remained healthy')
  report.peak=5;report.images=images;report.sends=sends;report.result='passed'
}catch(e){report.result='failed';report.failure=e.stack;if(page)try{await screenshot('failure.png')}catch{};process.exitCode=1}
finally {
  releaseFinal?.()
  releaseHealthyChild?.()
  if(context)await context.tracing.stop({path:join(output,'trace.zip')}).catch(()=>{})
  if(browser)await browser.close().catch(()=>{})
  for(const item of processes.reverse()){if(item.p.exitCode===null){item.p.kill('SIGTERM');await Promise.race([item.closed,delay(10000)]);if(item.p.exitCode===null)item.p.kill('SIGKILL')}await item.save()}
  for(const s of servers){s.closeAllConnections?.();await new Promise(r=>s.close(r))}
  report.finishedAt=new Date().toISOString();await writeFile(join(output,'report.json'),JSON.stringify(report,null,2));console.log(JSON.stringify({output,result:report.result,failure:report.failure,errors:report.errors}))
}
