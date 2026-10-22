// Product acceptance only: real Host + frozen Office + independent Chromium.
// The local OpenAI fixture is a deterministic Provider, never a chat bridge.
import assert from 'node:assert/strict'
import { createHash, randomBytes } from 'node:crypto'
import { spawn, execFileSync } from 'node:child_process'
import { createServer } from 'node:http'
import { createServer as tcpServer } from 'node:net'
import { mkdir, readFile, writeFile, access, readdir, realpath } from 'node:fs/promises'
import { dirname, join, resolve, relative, delimiter } from 'node:path'
import { fileURLToPath, pathToFileURL } from 'node:url'
import { tmpdir } from 'node:os'
import { once } from 'node:events'
import { officeRequestSchema, officeResultSchema, officeSnapshotSchema } from './upstream-office/packages/protocol/src/office-integration.ts'
import { chatEventSchema, chatOperationSchema, chatHistoryPageSchema, chatCancelResultSchema } from './upstream-office/packages/protocol/src/chat.ts'
import { assertProtocolValue, responseEnvelopeSchema } from './upstream-office/packages/protocol/src/schema.ts'

const root = resolve(dirname(fileURLToPath(import.meta.url)), '../..')
const manifest = JSON.parse(await readFile(join(root, 'docs/engine-host/contracts/sources.json'), 'utf8'))
const source = process.env.OPENPAW_OFFICE_SOURCE
if (!source) throw new Error('OPENPAW_OFFICE_SOURCE must name the read-only Office checkout')
const output = process.env.OPENPAW_OFFICE_PRODUCT_OUTPUT
  ? resolve(process.env.OPENPAW_OFFICE_PRODUCT_OUTPUT)
  : join(tmpdir(), 'openpaw-office-product-' + Date.now() + '-' + randomBytes(3).toString('hex'))
if (!relative(root, output).startsWith('..')) throw new Error('Evidence must be outside the repository')
await mkdir(output, { recursive: false })
const report = { startedAt: new Date().toISOString(), platform: process.platform, commit: execFileSync('git', ['rev-parse', 'HEAD'], { cwd: root }).toString().trim(), officeCommit: manifest.office.commit,
  sourceDirty: Boolean(execFileSync('git', ['status', '--porcelain'], { cwd: root }).toString().trim()),
  provider: 'deterministic OpenAI protocol fixture; actual AgentLoop and tools', commands: [], browserRequests: [], browserErrors: [], modelRequests: [], messages: [], assertions: [], result: 'running' }
const processes = []
let browser, context, providerServer, page
const delay = ms => new Promise(r => setTimeout(r, ms))
async function waitFor(fn, timeout = 30000) {
  const deadline = Date.now() + timeout
  while (Date.now() < deadline) { const value = await fn(); if (value) return value; await delay(100) }
  throw new Error('Condition was not met before timeout')
}
function start(command, args, cwd, env, label) {
  const log = { label, command, args, cwd, startedAt: new Date().toISOString() }
  report.commands.push(log)
  const p = spawn(command, args, { cwd, env, stdio: ['ignore', 'pipe', 'pipe'], windowsHide: true })
  let stdout = '', stderr = ''
  p.stdout.on('data', b => { stdout += b.toString() })
  p.stderr.on('data', b => { stderr += b.toString() })
  p.on('error', e => { log.error = e.message })
  p.on('exit', (code, signal) => { log.exitCode = code; log.signal = signal })
  const close = once(p, 'close')
  const item = { p, label, close, stdout: () => stdout, save: async () => { await writeFile(join(output, label + '.stdout.log'), stdout); await writeFile(join(output, label + '.stderr.log'), stderr) } }
  processes.push(item)
  return item
}
async function run(command, args, cwd, env, label) {
  const item = start(command, args, cwd, env, label)
  // Bound installation/build waits and retain a real failed exit rather than hanging the suite.
  const timer = setTimeout(() => item.p.kill('SIGKILL'), 300000)
  try { await item.close } finally { clearTimeout(timer); await item.save() }
  if (item.p.exitCode !== 0) throw new Error(label + ' failed; see its log')
}
async function npmCli() {
  const pathNpm = []
  for (const directory of (process.env.PATH ?? '').split(delimiter)) { try { const file = await realpath(join(directory, 'npm')); if (file.endsWith('.js')) pathNpm.push(file) } catch { /* Windows uses npm-cli.js layout below */ } }
  const candidates = [process.env.OPENPAW_NPM_CLI, process.env.npm_execpath, ...pathNpm,
    join(dirname(process.execPath), 'node_modules/npm/bin/npm-cli.js'),
    join(dirname(process.execPath), '../lib/node_modules/npm/bin/npm-cli.js'),
    '/usr/share/nodejs/npm/bin/npm-cli.js'].filter(Boolean)
  for (const candidate of candidates) { try { await access(candidate); return resolve(candidate) } catch { /* try next platform layout */ } }
  throw new Error('Set OPENPAW_NPM_CLI to the installed npm-cli.js (no shell shim required)')
}
async function freePort() { const s = tcpServer(); s.listen(0, '127.0.0.1'); await once(s, 'listening'); const p = s.address().port; await new Promise(r => s.close(r)); return p }
const childEnv = { ...process.env, OPENPAW_HOME: join(output, 'model-home'), OPENAI_API_KEY: 'deterministic-provider-key', PAWOFFICE_API_KEY: 'deterministic-office-key' }
async function officePost(base, request) {
  assertProtocolValue(request); officeRequestSchema.parse(request)
  assert.ok(Buffer.byteLength(JSON.stringify(request)) <= 65536)
  const r = await fetch(base + '/api/pawoffice/messages', { method: 'POST', headers: { 'content-type': 'application/json', Authorization: 'Bearer deterministic-office-key' }, body: JSON.stringify(request), signal: AbortSignal.timeout(20000) })
  const data = await r.json(); assertProtocolValue(data); responseEnvelopeSchema.parse(data); assert.equal(data.code, r.status)
  if (r.ok) officeResultSchema(request.type, Boolean(request.teamId)).parse(data.data)
  report.messages.push({ at: new Date().toISOString(), request, http: r.status, response: data })
  return data
}
function provider() {
  return createServer(async (req, res) => {
    try {
      let body = ''; for await (const b of req) { body += b; if (body.length > 4 * 1024 * 1024) throw new Error('fixture input too large') }
      const data = JSON.parse(body)
      assert.equal(req.url, '/v1/chat/completions'); assert.equal(data.model, 'office-product-fixture')
      assert.equal(req.headers.authorization, 'Bearer deterministic-provider-key')
      report.modelRequests.push({ at: new Date().toISOString(), path: req.url, model: data.model, messages: data.messages, tools: data.tools.map(t => t.function.name), authorizationMatched: true })
      const lastUser = data.messages.findLastIndex(m => m.role === 'user')
      const prompt = data.messages[lastUser]?.content ?? ''
      const results = data.messages.slice(lastUser + 1).filter(m => m.role === 'tool')
      if (prompt.includes('慢任务')) {
        const timer = setTimeout(() => { if (!res.destroyed) finish({ content: '慢任务完成' }) }, 60000)
        res.once('close', () => clearTimeout(timer)); return
      }
      if (prompt.includes('读取') && results.length === 0) {
        finish({ tool_calls: ['README.md', 'notes.txt'].map((path, i) => ({ index: i, id: 'real-read-' + i, type: 'function', function: { name: 'read_file', arguments: JSON.stringify({ path }) } })) }, 'tool_calls'); return
      }
      let text = '网页协议对话正常。'
      if (results.length) text = '实际工具读取结果：' + results.map(m => m.content).join('\n')
      if (prompt.includes('追问')) text = '已保留历史：' + data.messages.filter(m => m.role === 'tool').map(m => m.content).join('\n')
      finish({ content: text })
      function finish(delta, reason = 'stop') {
        res.writeHead(200, { 'content-type': 'text/event-stream' })
        res.write('data: ' + JSON.stringify({ choices: [{ index: 0, delta, finish_reason: null }] }) + '\n\n')
        res.write('data: ' + JSON.stringify({ choices: [{ index: 0, delta: {}, finish_reason: reason }], usage: { prompt_tokens: 100, completion_tokens: 30 } }) + '\n\n')
        res.end('data: [DONE]\n\n')
      }
    } catch (e) { report.modelError = e.stack; res.writeHead(500); res.end('fixture failure') }
  })
}
try {
  const office = join(output, 'office')
  await run('git', ['clone', '--no-hardlinks', '--no-checkout', '--', resolve(source), office], root, childEnv, 'office-clone')
  await run('git', ['checkout', '--detach', manifest.office.commit], office, childEnv, 'office-checkout')
  for (const item of manifest.office.files) {
    const bytes = await readFile(join(office, item.path)); assert.equal(createHash('sha256').update(bytes).digest('hex'), item.sha256)
  }
  const npm = await npmCli()
  await run(process.execPath, [npm, 'ci', '--prefer-offline', '--ignore-scripts', '--no-audit', '--no-fund'], office, childEnv, 'office-install')
  const officePort = await freePort(); const officeBase = 'http://127.0.0.1:' + officePort
  providerServer = provider(); providerServer.listen(0, '127.0.0.1'); await once(providerServer, 'listening')
  const modelBase = 'http://127.0.0.1:' + providerServer.address().port + '/v1'
  const workspace = join(output, 'workspace 中文'); await mkdir(workspace); await mkdir(childEnv.OPENPAW_HOME)
  const markerA = 'README-' + randomBytes(6).toString('hex'), markerB = 'NOTES-' + randomBytes(6).toString('hex')
  await writeFile(join(workspace, 'README.md'), markerA); await writeFile(join(workspace, 'notes.txt'), markerB)
  const config = join(output, 'host.toml')
  await writeFile(config, `home = ${JSON.stringify(join(output, 'host-data'))}\nprovider = "openai"\nmodel = "office-product-fixture"\nbase_url = ${JSON.stringify(modelBase)}\napi_key_env = "OPENAI_API_KEY"\n[roots]\nproject = ${JSON.stringify(workspace)}\n[slots]\noffice-1 = "project"\noffice-2 = "project"\n[office]\nbase_url = ${JSON.stringify(officeBase)}\napi_key_env = "PAWOFFICE_API_KEY"\nallowed_origins = [${JSON.stringify(officeBase)}]\n[office.names]\noffice-1 = "真实运行验收"\noffice-2 = "独立办公室"\n`)
  if (!process.env.OPENPAW_HOST_BINARY) await run('cargo', ['build', '--locked', '-p', 'openpaw-cli', '--features', 'engine-host,sidecar'], root, childEnv, 'host-build')
  const binary = process.env.OPENPAW_HOST_BINARY ?? join(process.env.CARGO_TARGET_DIR ?? join(root, 'target'), 'debug', process.platform === 'win32' ? 'openpaw.exe' : 'openpaw')
  let host = start(resolve(binary), ['engine-host', '--config', config, '--port', '0'], root, childEnv, 'host')
  const hostBase = await waitFor(() => host.stdout().match(/openpaw engine host listening on (http:\/\/127\.0\.0\.1:\d+)/)?.[1])
  report.hostBinarySha256 = createHash('sha256').update(await readFile(resolve(binary))).digest('hex')
  const buildEnv = { ...childEnv } // chat.url由Host运行时注册，不再注入VITE配置
  await run(process.execPath, [npm, 'run', 'build:full'], office, buildEnv, 'office-build')
  let officeProcess = start(process.execPath, ['server/dist/index.js'], office, { ...childEnv, PORT: String(officePort), HOST: '127.0.0.1', PAWOFFICE_PUBLIC_ORIGIN: officeBase }, 'office-server')
  await waitFor(async () => { try { return (await fetch(officeBase + '/office/')).ok } catch { return false } })
  const { chromium } = await import(pathToFileURL(join(office, 'node_modules/playwright-core/index.mjs')))
  browser = await chromium.launch({ headless: true, ...(process.env.OPENPAW_CHROMIUM ? { executablePath: process.env.OPENPAW_CHROMIUM } : {}), args: ['--use-angle=swiftshader', '--enable-webgl', '--enable-unsafe-swiftshader'] })
  context = await browser.newContext({ viewport: { width: 1440, height: 1050 }, locale: 'zh-CN', recordVideo: { dir: join(output, 'video') } })
  await context.tracing.start({ screenshots: true, snapshots: true, sources: false })
  page = await context.newPage()
  // Passive CDP capture survives HttpChatSource reader.cancel() after done.
  // Cloning/teeing fetch would keep a connection alive and invalidate cancellation testing.
  const cdp = await context.newCDPSession(page)
  await cdp.send('Network.enable')
  const streams = new Map(), captureJobs = []
  page.on('pageerror', e => report.browserErrors.push(e.message))
  page.on('response', async response => {
    if(response.url()!==officeBase+'/api/pawoffice/chat') return
    const request=response.request().postDataJSON()
    if(!['history','cancel'].includes(request.type))return
    const item=report.browserRequests.findLast(r=>JSON.stringify(r.request)===JSON.stringify(request))
    if(!item)return
    try {item.http=response.status();item.json=await response.json();responseEnvelopeSchema.parse(item.json);assertProtocolValue(item.json)
      if(response.ok())(request.type==='history'?chatHistoryPageSchema:chatCancelResultSchema).parse(item.json.data)
    }catch(e){report.browserErrors.push(String(e))}
  })
  cdp.on('Network.requestWillBeSent', event => {
    if (event.request.url === officeBase + '/api/pawoffice/chat' && event.request.method === 'POST') {
      const item = { at: new Date().toISOString(), url: event.request.url, method: 'POST', request: JSON.parse(event.request.postData) }
      chatOperationSchema.parse(item.request)
      report.browserRequests.push(item)
      if (['send','resume'].includes(item.request.type)) streams.set(event.requestId, { item, chunks: [] })
    }
  })
  cdp.on('Network.responseReceived', event => {
    const stream = streams.get(event.requestId)
    if (!stream) return
    stream.item.http = event.response.status
    captureJobs.push(cdp.send('Network.streamResourceContent', { requestId: event.requestId }).then(result => {
      if (result.bufferedData) stream.chunks.unshift(Buffer.from(result.bufferedData, 'base64'))
    }).catch(error => { stream.item.captureError = String(error) }))
  })
  cdp.on('Network.dataReceived', event => { const stream = streams.get(event.requestId); if (stream && event.data) stream.chunks.push(Buffer.from(event.data, 'base64')) })
  function capturedEvents() {
    for (const { item, chunks } of streams.values()) {
      item.body = Buffer.concat(chunks).toString('utf8')
      assert.ok(!item.captureError, item.captureError)
      assert.equal(item.request.teamId, 'office-1', 'browser journey must use selected office')
      item.events = item.body.split('\n').filter(Boolean).map(line => { assert.ok(line.length <= 350000); const event = JSON.parse(line); chatEventSchema.parse(event); assertProtocolValue(event); return event })
    }
  }
  const input = page.getByRole('textbox', { name: '给办公室发送消息' })
  const log = page.getByRole('log', {name:'对话内容'})
  async function chooseOffice(p=page){
    await p.getByText(/正在加载办公室/).waitFor({state:'hidden',timeout:60000})
    await p.getByRole('button',{name:/^团队空间，当前团队/}).click()
    await p.getByRole('button',{name:'进入真实运行验收',exact:true}).click()
    await p.getByRole('dialog').waitFor({state:'hidden'})
  }
  async function newConversation(){
    await page.getByRole('button',{name:'新会话',exact:true}).first().click()
    await input.waitFor();await waitFor(()=>input.isEnabled())
  }
  async function send(text,expected){
    await waitFor(async()=>{if(!await input.isEnabled())return false;await input.fill(text);return await page.getByRole('button',{name:'发送消息',exact:true}).isEnabled()})
    await page.getByRole('button',{name:'发送消息',exact:true}).click()
    if(expected){await waitFor(async()=>(await log.innerText()).includes(expected),45000);await page.getByRole('button',{name:'停止生成',exact:true}).waitFor({state:'hidden',timeout:45000})}
  }
  const readOffice=()=>officePost(officeBase,{type:'office.read',teamId:'office-1'})
  const conv=(result,id)=>result.data.snapshot.conversations.find(c=>c.id===id)
  const latestSend=()=>report.browserRequests.findLast(r=>r.request.type==='send')
  const terminal=e=>e.type==='done'||e.type==='snapshot'&&e.state==='complete'
  await page.goto(officeBase + '/office/', { waitUntil: 'domcontentloaded' })
  await chooseOffice()
  await newConversation()
  await page.screenshot({path:join(output,'01-connected.png'),fullPage:true})
  await send('A读取README.md和notes.txt并总结',markerA)
  const aRequest=structuredClone(latestSend().request), a=aRequest.conversationId
  const transcript=await log.innerText();assert.ok(transcript.includes(markerB))
  const state=await waitFor(async()=>{const r=await readOffice();return r.code===200&&conv(r,a)?.tasks.filter(t=>t.id.startsWith('tool:')).length===2?r:false})
  const current=conv(state,a)
  assert.ok(current.tasks.filter(t=>t.title.includes('read_file')).every(t=>t.status==='completed'))
  assert.ok(current.events.some(e=>e.kind==='task.assigned'));assert.ok(current.whiteboard.steps.length<=3)
  report.correlations={conversationA:a,firstTurn:aRequest.turnId,toolTasks:current.tasks.filter(t=>t.id.startsWith('tool:')).map(t=>t.id),runTask:current.tasks[0].id}
  await page.screenshot({path:join(output,'02-real-tools.png'),fullPage:true})
  const downloadPromise=page.waitForEvent('download')
  await page.getByRole('button',{name:'导出快照',exact:true}).click()
  const download=await downloadPromise;await download.saveAs(join(output,'02-scene-export.json'))
  // Same real request twice: original Run, terminal snapshot and no new model/tool request.
  const modelCount=report.modelRequests.length
  const replay=await fetch(hostBase+'/office/chat',{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify(aRequest)})
  assert.equal(replay.status,200)
  const replayEvents=(await replay.text()).trim().split('\n').map(JSON.parse)
  for(const e of replayEvents)chatEventSchema.parse(e)
  assert.equal(replayEvents[0].type,'snapshot');assert.equal(replayEvents[0].state,'complete')
  assert.equal(report.modelRequests.length,modelCount)
  const changed=structuredClone(aRequest);changed.messages.at(-1).content='同turn不同内容'
  const conflictTurn=await fetch(hostBase+'/office/chat',{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify(changed)})
  assert.equal(conflictTurn.status,409)
  report.idempotency={request:aRequest,replayEvents,conflictHttp:conflictTurn.status,modelCountBefore:modelCount,modelCountAfter:report.modelRequests.length}
  await send('追问：刚才两个文件分别包含什么？','已保留历史')
  await send('慢任务A：保持后台运行')
  const active=structuredClone(latestSend().request)
  await waitFor(async()=>{const r=await readOffice();return conv(r,a)?.tasks.some(t=>t.status==='running')})
  await newConversation()
  await send('B会话你好','网页协议对话正常')
  const b=latestSend().request.conversationId
  assert.notEqual(a,b);assert.ok(!(await log.innerText()).includes(markerA))
  const both=await waitFor(async()=>{const r=await readOffice();return conv(r,a)?.tasks.some(t=>t.status==='running')&&conv(r,b)?.tasks.some(t=>t.status==='completed')?r:false})
  report.correlations.conversationB=b
  await page.screenshot({path:join(output,'03-B-independent-A-running.png'),fullPage:true})
  await page.getByRole('tab',{name:/A读取README/}).click()
  await waitFor(async()=>(await log.innerText()).includes(markerA))
  await page.screenshot({path:join(output,'04-A-working-before-refresh.png'),fullPage:true})
  const runsBeforeRefresh=report.modelRequests.filter(r=>r.messages.some(m=>m.role==='user'&&m.content==='慢任务A：保持后台运行')).length
  await page.reload({waitUntil:'domcontentloaded'})
  await chooseOffice()
  await input.waitFor({timeout:60000})
  await waitFor(()=>report.browserRequests.some(r=>r.request.type==='resume'&&r.request.turnId===active.turnId),45000)
  assert.equal(report.modelRequests.filter(r=>r.messages.some(m=>m.role==='user'&&m.content==='慢任务A：保持后台运行')).length,runsBeforeRefresh)
  if(await page.getByRole('button',{name:'展开对话',exact:true}).isVisible())await page.getByRole('button',{name:'展开对话',exact:true}).click()
  await page.getByRole('tab',{name:/A读取README/}).click()
  await page.screenshot({path:join(output,'05-refresh-resume-same-run.png'),fullPage:true})
  await page.getByRole('button',{name:'停止生成',exact:true}).click()
  await waitFor(async()=>(await log.innerText()).includes('已停止生成'))
  await waitFor(()=>report.browserRequests.some(r=>r.request.type==='cancel'&&r.request.turnId===active.turnId&&r.json?.data?.stopped===true))
  await waitFor(async()=>{const r=await readOffice();return conv(r,a)?.tasks.some(t=>t.summary.includes('用户取消，清理已完成'))})
  await page.screenshot({path:join(output,'06-explicit-cancel-confirmed.png'),fullPage:true})
  // Fresh browser storage discovers the exact conversation IDs from the live catalogue.
  const secondContext=await browser.newContext({viewport:{width:1440,height:1050},locale:'zh-CN'})
  const second=await secondContext.newPage();await second.goto(officeBase+'/office/',{waitUntil:'domcontentloaded'});await chooseOffice(second)
  await second.getByRole('button',{name:'展开对话',exact:true}).click()
  await second.getByRole('tab',{name:/B会话你好/}).click()
  await waitFor(async()=>(await second.getByRole('log',{name:'对话内容'}).innerText()).includes('网页协议对话正常'))
  await second.screenshot({path:join(output,'07-new-browser-discovers-conversations.png'),fullPage:true});await secondContext.close()
  async function control(request){
    chatOperationSchema.parse(request)
    const r=await fetch(hostBase+'/office/chat',{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify(request)})
    assert.equal(r.status,200);const json=await r.json();responseEnvelopeSchema.parse(json);assertProtocolValue(json);return json.data
  }
  const identity={version:'1.0',teamId:'office-1',conversationId:a}
  const latestPage=await control({...identity,type:'history',limit:1});chatHistoryPageSchema.parse(latestPage);assert.equal(latestPage.turns[0].id,active.turnId);assert.equal(latestPage.turns[0].state,'stopped');assert.ok(latestPage.hasMore)
  const olderPage=await control({...identity,type:'history',limit:1,cursor:latestPage.nextCursor});chatHistoryPageSchema.parse(olderPage);assert.notEqual(olderPage.turns[0].id,active.turnId)
  report.historyPages=[latestPage,olderPage]
  const saved=structuredClone((await readOffice()).data.snapshot)
  const same=await officePost(officeBase,{type:'office.snapshot',teamId:'office-1',snapshot:saved});assert.ok([200,202].includes(same.code))
  const differing=structuredClone(saved);differing.workspace.name='同版本不同内容'
  const conflict=await officePost(officeBase,{type:'office.snapshot',teamId:'office-1',snapshot:differing});assert.equal(conflict.code,409)
  if(saved.seq>1){const old=structuredClone(saved);old.seq--;assert.equal((await officePost(officeBase,{type:'office.snapshot',teamId:'office-1',snapshot:old})).data.status,'ignored')}
  officeProcess.p.kill('SIGTERM');await officeProcess.close
  officeProcess=start(process.execPath,['server/dist/index.js'],office,{...childEnv,PORT:String(officePort),HOST:'127.0.0.1',PAWOFFICE_PUBLIC_ORIGIN:officeBase},'office-restarted')
  await waitFor(async()=>{try{const r=await readOffice();return r.code===200&&conv(r,a)&&conv(r,b)}catch{return false}},45000)
  chatHistoryPageSchema.parse(await control({...identity,type:'history'}))
  // Verify registered chat.url after real Fastify restart through its public route.
  const forwarded=await fetch(officeBase+'/api/pawoffice/chat',{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify({...identity,type:'history'})})
  assert.equal(forwarded.status,200);chatHistoryPageSchema.parse((await forwarded.json()).data)
  const crashId='host-crash-'+Date.now(),crashTurn='crash-turn-'+Date.now()
  const crashRequest=fetch(hostBase+'/office/chat',{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify({version:'1.0',type:'send',turnId:crashTurn,teamId:'office-1',conversationId:crashId,messages:[{role:'user',content:'慢任务：模拟Host崩溃'}]})}).catch(()=>null)
  await waitFor(()=>report.modelRequests.some(r=>r.messages.some(m=>m.role==='user'&&m.content==='慢任务：模拟Host崩溃')))
  const requestsBeforeCrash=report.modelRequests.length
  host.p.kill('SIGKILL');await host.close
  host=start(resolve(binary),['engine-host','--config',config,'--port',new URL(hostBase).port],root,childEnv,'host-restarted')
  await waitFor(()=>host.stdout().includes('openpaw engine host listening on'))
  await waitFor(async()=>{const r=await readOffice();return conv(r,crashId)?.tasks.some(t=>t.summary.includes('Host重启中断'))},45000)
  const crashHistory=await control({version:'1.0',type:'history',teamId:'office-1',conversationId:crashId});chatHistoryPageSchema.parse(crashHistory);assert.equal(crashHistory.turns[0].state,'error');assert.equal(crashHistory.turns[0].id,crashTurn)
  assert.equal(report.modelRequests.length,requestsBeforeCrash);await crashRequest
  const recovered=await readOffice()
  await page.close();officeProcess.p.kill('SIGTERM');await officeProcess.close
  officeProcess=start(process.execPath,['server/dist/index.js'],office,{...childEnv,PORT:String(officePort),HOST:'127.0.0.1',PAWOFFICE_PUBLIC_ORIGIN:officeBase},'office-offline-restarted')
  await waitFor(async()=>{try{const r=await officePost(officeBase,{type:'office.snapshot',teamId:'office-1',snapshot:recovered.data.snapshot});return r.code===202&&r.data.status==='cached'?r:false}catch{return false}},45000)
  report.transcript=transcript
  await Promise.all(captureJobs);capturedEvents()
  assert.ok(report.browserRequests.some(r=>r.http===200&&r.events?.some(terminal)))
  assert.equal(report.browserErrors.length,0)
  report.assertions.push('Real browser same-origin four-operation chat through unmodified latest Office','A/B conversations independent tasks and whiteboards; A running while B completes','Original read_file outputs correlated with Run/call/scene snapshot','Same turn retry reuses Run; different content conflicts','Refresh history/resume detaches without duplicate model/tool execution','Explicit cancel waits for cleanup','New browser discovers original conversation IDs and stored history','Stable paginated history and stopped state','Same seq retry/conflict/stale and offline 202 from real service','Fastify restart re-registers chat.url','Host crash interrupted without replay; stable turn survives restart')
  const sessionDir = join(output, 'host-data/sessions')
  report.sessions = await readdir(sessionDir)
  for (const file of report.sessions) {
    const ledger = (await readFile(join(sessionDir, file), 'utf8')).trim().split('\n').map(JSON.parse)
    assert.ok(ledger.filter(e => e.type === 'tool_call_complete').length <= 2, 'no tool replay after restart')
    report.assertions.push('Session ' + file + ': ' + ledger.filter(e => e.type === 'run_finished').map(e => e.status).join(','))
  }
  report.result = 'passed'
} catch (e) { report.result = 'failed'; report.error = e.stack; if (page) await page.screenshot({ path: join(output, 'failure.png'), fullPage: true }).catch(() => {}); process.exitCode = 1 }
finally {
  if (context) { await context.tracing.stop({ path: join(output, 'trace.zip') }).catch(() => {}); await context.close() }
  if (browser) await browser.close()
  for (const item of [...processes].reverse()) {
    if (item.p.exitCode === null && item.p.signalCode === null) { item.p.kill('SIGTERM'); await Promise.race([item.close, delay(8000)]); if (item.p.exitCode === null && item.p.signalCode === null) { item.p.kill('SIGKILL'); await item.close } }
    await item.save()
  }
  if (providerServer) { providerServer.closeAllConnections(); await new Promise(r => providerServer.close(r)) }
  report.finishedAt = new Date().toISOString(); await writeFile(join(output, 'report.json'), JSON.stringify(report, null, 2))
  console.log(JSON.stringify({ result: report.result, output, error: report.error }, null, 2))
}
