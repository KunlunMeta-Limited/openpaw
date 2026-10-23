// Product acceptance only: real Host + frozen Office + independent Chromium.
// The local OpenAI fixture is a deterministic Provider, never a chat bridge.
import assert from 'node:assert/strict'
import { teamProduct } from './office-resilience.mjs'
import { createHash, randomBytes } from 'node:crypto'
import { spawn, execFileSync } from 'node:child_process'
import { createServer } from 'node:http'
import { createServer as tcpServer } from 'node:net'
import { mkdir, readFile, writeFile, access, readdir, realpath, copyFile } from 'node:fs/promises'
import { dirname, join, resolve, relative, delimiter } from 'node:path'
import { fileURLToPath, pathToFileURL } from 'node:url'
import { tmpdir } from 'node:os'
import { once } from 'node:events'
import { DatabaseSync } from 'node:sqlite'
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
let browser, context, providerServer, page, webFixtureServer, webBase
const webFixture = { requests: [], active: 0, cancelled: 0 }
const teamGates = new Map()
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
      const role=['lead','research','organize','draft','review','audit'].find(role=>data.messages.some(m=>m.role==='system'&&m.content.includes('（'+role+'）')))
      if(role&&role!=='lead') {
        if(!results.length) {
          await new Promise(resolve=>teamGates.set(role,resolve))
          finish({content:'正在核查本角色资料。',tool_calls:[{index:0,id:'team-read-'+role,type:'function',function:{name:'read_file',arguments:JSON.stringify({path:'README.md'})}}]},'tool_calls');return
        }
        assert.ok(results[0].content.includes('README-'))
        finish({content:role+'：已根据真实README读取完成独立核查。'});return
      }
      if(prompt==='T6真实五人协作') {
        assert.equal(role,'lead');assert.ok(data.tools.some(t=>t.function.name==='delegate_task'))
        const roles=['research','organize','draft','review','audit'];const names=['李研','周理','陈书','刘市','赵审']
        if(results.length<roles.length) {const n=results.length;finish({content:'',tool_calls:[{index:0,id:'delegate-'+roles[n],type:'function',function:{name:'delegate_task',arguments:JSON.stringify({role_id:roles[n],title:names[n]+'核对项目说明',instructions:'读取README.md，核对项目说明并报告本角色的真实依据和限制',acceptance:'必须有本子会话的真实读取与独立答复'})}}]},'tool_calls');return}
        assert.ok(results.every(r=>JSON.parse(r.content.split('\n\n[Host执行回执]')[0]).state==='completed'))
        finish({content:'T6五个独立角色核查完成，均有真实独立Run和文件读取。'});return
      }
      if (prompt==='今天是几号') {
        assert.equal(data.messages[lastUser-1]?.role,'system')
        const current=data.messages[lastUser-1].content.match(/Host local time: ([^\n]+)/)?.[1]
        assert.ok(current&&!Number.isNaN(Date.parse(current)))
        finish({content:'HOST-DATE: '+current});return
      }
      if (prompt==='旧能力会话'||(prompt==='长沙天气'&&!data.tools.some(t=>t.function.name==='web_fetch'))) {
        assert.ok(!data.tools.some(t=>['web_search','web_fetch'].includes(t.function.name)))
        assert.equal(data.messages[lastUser-1]?.role,'system')
        assert.ok(data.messages[lastUser-1].content.includes('本轮未启用联网工具'))
        finish({content:'这一轮我没有联网工具，也没有系统时钟。'});return
      }
      const legacyFollowUp=prompt==='长沙天气'&&data.messages.some(m=>m.role==='assistant'&&m.content?.includes('这一轮我没有联网工具'))
      if (prompt.includes('WEB-')||legacyFollowUp) {
        assert.ok(data.tools.some(t=>t.function.name==='web_search'));assert.ok(data.tools.some(t=>t.function.name==='web_fetch'))
        assert.ok(data.messages.some(m=>m.role==='system'&&m.content.includes('Current UTC time:')&&m.content.includes('fresh=true')))
        if(legacyFollowUp){
          // Check actual original Provider wire, not just the leading system or echoed answer.
          assert.equal(data.messages[lastUser-1]?.role,'system')
          assert.ok(data.messages[lastUser-1].content.includes('历史回复不代表本轮能力')&&data.messages[lastUser-1].content.includes('本轮已启用联网工具'))
          assert.equal(data.messages.filter(m=>m.role==='system').length,2)
        }
        if (!results.length) {
          finish({content:'正在读取真实HTTP页面。',tool_calls:[{index:0,id:'actual-web-fetch',type:'function',function:{name:'web_fetch',arguments:JSON.stringify({url:webBase+(prompt.includes('WEB-SLOW')?'/slow':'/fresh'),fresh:true})}}]},'tool_calls');return
        }
        assert.ok(results[0].content.includes('WEB-HTTP-MARKER'))
        finish({content:'# 网页读取结果\n\n'+results[0].content});return
      }
      if (prompt==='帮我查询资料并记录到reports/天气.md') {
        assert.ok(data.tools.some(t=>t.function.name==='report_progress'));
        const fetchResult=results.find(r=>r.tool_call_id==='progress-2')?.content;
        const fetchReceipt=fetchResult?.match(/\[Host执行回执\]\n(\{[^\n]+\})\n/);
        const actualFetch=fetchReceipt?JSON.parse(fetchReceipt[1]):undefined;
        if(fetchResult){
          assert.equal(actualFetch?.callId,'progress-2');assert.equal(actualFetch?.tool,'web_fetch');
          assert.ok(fetchResult.includes('WEB-HTTP-MARKER'));
        }
        const savedResult=results.find(r=>r.tool_call_id==='progress-5')?.content;
        if(savedResult){
          const receipt=JSON.parse(savedResult.match(/\[Host执行回执\]\n(\{[^\n]+\})\n/)?.[1]??'null');
          assert.equal(receipt?.callId,'progress-5');assert.equal(receipt?.tool,'write_file');
          assert.ok(savedResult.startsWith('wrote '));
        }
        const recovery=results.find(r=>r.tool_call_id==='progress-6')?.content;
        const candidates=recovery?JSON.parse(recovery.match(/可用证据：(\[.*?\])。evidenceCallIds/s)?.[1]??'[]'):[];
        const recoveredWrite=candidates.find(c=>c.tool==='write_file')?.callId;
        if(recovery)assert.equal(recoveredWrite,'progress-5','恢复提示必须来自本轮实际成功写入');
        const calls=[
          ['report_progress',{type:'create',steps:[{id:'lookup',title:'查询当天资料'},{id:'save',title:'保存 reports/天气.md'},{id:'answer',title:'整理答复'}]}],
          ['report_progress',{type:'update',stepId:'lookup',state:'in_progress'}],
          ['web_fetch',{url:webBase+'/fresh',fresh:true}],
          ['report_progress',{type:'update',stepId:'lookup',state:'completed',evidenceCallIds:[actualFetch?.callId??'unavailable']}],
          ['report_progress',{type:'update',stepId:'save',state:'in_progress'}],
          ['write_file',{path:'reports/天气.md',content:'# 确定性资料验收\n\n'+(results.find(r=>r.tool_call_id==='progress-2')?.content??'')}],
          ['report_progress',{type:'update',stepId:'save',state:'completed',evidenceCallIds:['write_file:reports/天气.md']}],
          ['report_progress',{type:'update',stepId:'save',state:'completed',evidenceCallIds:[recoveredWrite??'unavailable']}],
          ['report_progress',{type:'update',stepId:'answer',state:'in_progress'}],
        ];
        if(results.length<calls.length){
          if(results.length>0&&calls[results.length-1][0]==='report_progress'&&results.length!==7) assert.ok(results.at(-1).content.includes('业务步骤已记录')&&results.at(-1).content.includes('当前步骤：'));
          if(results.length===7)assert.ok(results.at(-1).content.includes('invalid_progress')&&recoveredWrite);
          const [name,input]=calls[results.length];
          finish({content:'',tool_calls:[{index:0,id:'progress-'+results.length,type:'function',function:{name,arguments:JSON.stringify(input)}}]},'tool_calls');return;
        }
        assert.ok(results.find(r=>r.tool_call_id==='progress-2').content.includes('WEB-HTTP-MARKER'));
        finish({content:'资料已通过真实HTTP取回，并经确认保存到 reports/天气.md。此为确定性协议验收数据，非实时天气。'});return;
      }
      if (prompt==='核对已保存的记录文件位置') {
        const calls=[
          ['report_progress',{type:'create',steps:[{id:'check',title:'核对本地日期与已保存的资料记录文件位置'}]}],
          ['glob',{pattern:'reports/天气.md'}],
          ['report_progress',{type:'update',stepId:'check',state:'completed',evidenceCallIds:['location-1']}],
        ];
        if(results.length<calls.length){
          if(results.length===2)assert.ok(results.at(-1).content.includes('reports/天气.md')&&results.at(-1).content.includes('[Host执行回执]'));
          const [name,input]=calls[results.length];
          finish({content:'',tool_calls:[{index:0,id:'location-'+results.length,type:'function',function:{name,arguments:JSON.stringify(input)}}]},'tool_calls');return;
        }
        assert.ok(results.at(-1).content.includes('业务步骤已记录'));
        finish({content:'已通过真实文件查询核对位置：reports/天气.md，无需再次写入。'});return;
      }
      if (prompt.startsWith('T5-')) {
        if (!results.length && !prompt.includes('命令')) {
          finish({content:'读取两文件后申请写入。',tool_calls:['README.md','notes.txt'].map((path,i)=>({index:i,id:'t5-read-'+i,type:'function',function:{name:'read_file',arguments:JSON.stringify({path})}}))},'tool_calls');return
        }
        const writeResult=results.find(m=>m.tool_call_id==='t5-write')
        if (!writeResult) {
          const command=JSON.stringify(process.execPath)+' managed-parent.cjs'
          const input=prompt.includes('命令')?{command}:{path:prompt.includes('拒绝')?'reports/refused.txt':'reports/T5报告.md',content:'# T5真实报告\n\n'+results.map(m=>m.content).join('\n')}
          finish({content:'拟执行，等待你的确认。',tool_calls:[{index:0,id:'t5-write',type:'function',function:{name:prompt.includes('命令')?'bash':'write_file',arguments:JSON.stringify(input)}}]},'tool_calls');return
        }
        finish({content:'T5工具结果：'+writeResult.content});return
      }
      if (prompt.includes('MODEL-ERROR')) { res.writeHead(401); res.end('controlled authentication failure'); return }
      if (prompt==='长连接180秒验收') {
        res.writeHead(200,{'content-type':'text/event-stream'});
        const chunk=text=>res.write('data: '+JSON.stringify({choices:[{index:0,delta:{content:text},finish_reason:null}]})+'\n\n');
        chunk('长连接开始。');const pulse=setInterval(()=>chunk('继续接收。'),30000);
        const ending=setTimeout(()=>{clearInterval(pulse);chunk('长连接真实完成。');res.write('data: '+JSON.stringify({choices:[{index:0,delta:{},finish_reason:'stop'}]})+'\n\n');res.end('data: [DONE]\n\n');},190000);
        res.once('close',()=>{clearInterval(pulse);clearTimeout(ending)});return;
      }
      if (prompt.includes('慢任务')) {
        const timer = setTimeout(() => { if (!res.destroyed) finish({ content: '慢任务完成' }) }, 60000)
        res.once('close', () => clearTimeout(timer)); return
      }
      if (prompt.includes('读取') && results.length === 0) {
        finish({ content: '我先读取这两个文件。', tool_calls: ['README.md', 'notes.txt'].map((path, i) => ({ index: i, id: 'real-read-' + i, type: 'function', function: { name: 'read_file', arguments: JSON.stringify({ path }) } })) }, 'tool_calls'); return
      }
      let text = '网页协议对话正常。已处理：' + prompt
      if (results.length) text = '# 文件读取结果\n\n| 文件 | 结果 |\n| --- | --- |\n' + results.map((m,i) => '| '+['README.md','notes.txt'][i]+' | '+m.content.replaceAll('\n',' ').replaceAll('|','\\|')+' |').join('\n') + '\n\n```text\n'+results.map(m=>m.content).join('\n')+'\n```'
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
  // 仅隔离产品子进程放行受控环回网页；真实演示绝不设置该门控。
  childEnv.OPENPAW_WEB_TEST_ALLOW_LOOPBACK = '1'
  webFixtureServer = createServer((req,res)=>{
    webFixture.requests.push({url:req.url,at:new Date().toISOString()})
    if(req.url==='/slow'){
      webFixture.active++;res.writeHead(200,{'content-type':'text/html'});res.write('<h1>waiting for body</h1>')
      res.once('close',()=>{webFixture.active--;webFixture.cancelled++})
    }else{res.writeHead(200,{'content-type':'text/html'});res.end('<h1>WEB-HTTP-MARKER</h1><p>Deterministic HTTP page, not live weather.</p>')}
  })
  webFixtureServer.listen(0,'127.0.0.1');await once(webFixtureServer,'listening');webBase='http://127.0.0.1:'+webFixtureServer.address().port
  providerServer = provider(); providerServer.listen(0, '127.0.0.1'); await once(providerServer, 'listening')
  const modelBase = 'http://127.0.0.1:' + providerServer.address().port + '/v1'
  const workspace = join(output, 'workspace 中文'); await mkdir(workspace); await mkdir(childEnv.OPENPAW_HOME)
  const markerA = 'README-' + randomBytes(6).toString('hex'), markerB = 'NOTES-' + randomBytes(6).toString('hex')
  await writeFile(join(workspace, 'README.md'), markerA); await writeFile(join(workspace, 'notes.txt'), markerB)
  // 原安全策略禁止解释器内联代码；使用隔离工作区脚本，不放宽工具策略。
  await writeFile(join(workspace,'managed-parent.cjs'),"require('fs').writeFileSync('managed-parent.pid',String(process.pid));require('child_process').spawn(process.execPath,['managed-child.cjs'],{stdio:'inherit'});setInterval(()=>console.log('parent-tick'),50)")
  await writeFile(join(workspace,'managed-child.cjs'),"const fs=require('fs');fs.writeFileSync('managed-child.pid',String(process.pid));setInterval(()=>console.error('child-tick'),50);setTimeout(()=>fs.writeFileSync('late-side-effect','bad'),8000)")
  const config = join(output, 'host.toml')
  await writeFile(config, `home = ${JSON.stringify(join(output, 'host-data'))}\nprovider = "openai"\nmodel = "office-product-fixture"\nbase_url = ${JSON.stringify(modelBase)}\napi_key_env = "OPENAI_API_KEY"\ntools = ["read_file", "glob", "grep", "todo_list", "think", "web_search", "web_fetch", "write_file", "edit_file", "todo_add", "todo_update", "bash"]\n[roots]\nproject = ${JSON.stringify(workspace)}\n[slots]\noffice-1 = "project"\noffice-2 = "project"\noffice-reserved = "project"\n[office]\nbase_url = ${JSON.stringify(officeBase)}\napi_key_env = "PAWOFFICE_API_KEY"\nallowed_origins = [${JSON.stringify(officeBase)}]\ncreation_slots = ["office-reserved"]\n[office.names]\noffice-1 = "真实运行验收"\noffice-2 = "独立办公室"\n`)
  if (!process.env.OPENPAW_HOST_BINARY) await run('cargo', ['build', '--locked', '-p', 'openpaw-cli', '--features', 'engine-host,sidecar'], root, childEnv, 'host-build')
  const buildBinary = process.env.OPENPAW_HOST_BINARY ?? join(process.env.CARGO_TARGET_DIR ?? join(root, 'target'), 'debug', process.platform === 'win32' ? 'openpaw.exe' : 'openpaw')
  // 每轮固定自己的已构建可执行文件；后续其他feature构建不能改变重启所用程序。
  const binary=join(output,process.platform==='win32'?'openpaw-product.exe':'openpaw-product');await copyFile(resolve(buildBinary),binary)
  let host = start(resolve(binary), ['engine-host', '--config', config, '--port', '0'], root, childEnv, 'host')
  const hostBase = await waitFor(() => host.stdout().match(/openpaw engine host listening on (http:\/\/127\.0\.0\.1:\d+)/)?.[1])
  report.hostBinarySha256 = createHash('sha256').update(await readFile(resolve(binary))).digest('hex')
  const buildEnv = { ...childEnv } // chat.url由Host运行时注册，不再注入VITE配置
  await run(process.execPath, [npm, 'run', 'build:full'], office, buildEnv, 'office-build')
  let officeProcess = start(process.execPath, ['server/dist/index.js'], office, { ...childEnv, PORT: String(officePort), HOST: '127.0.0.1', PAWOFFICE_PUBLIC_ORIGIN: officeBase }, 'office-server')
  await waitFor(async () => { try { return (await fetch(officeBase + '/office/')).ok } catch { return false } })
  const { chromium } = await import(pathToFileURL(join(office, 'node_modules/playwright-core/index.mjs')))
  browser = await chromium.launch({ headless: true, ...(process.env.OPENPAW_CHROMIUM ? { executablePath: process.env.OPENPAW_CHROMIUM } : {}), args: ['--use-angle=swiftshader', '--enable-webgl', '--enable-unsafe-swiftshader'] })
  context = await browser.newContext({ viewport: { width: 1440, height: 1050 }, locale: 'zh-CN', permissions: ['clipboard-read', 'clipboard-write'], recordVideo: { dir: join(output, 'video') } })
  await context.tracing.start({ screenshots: true, snapshots: true, sources: false })
  page = await context.newPage()
  // Passive CDP capture survives HttpChatSource reader.cancel() after done.
  // Cloning/teeing fetch would keep a connection alive and invalidate cancellation testing.
  const allStreams = [], captureJobs = []
  async function observe(p, ctx) {
    const cdp = await ctx.newCDPSession(p)
    await cdp.send('Network.enable')
    const streams = new Map()
    p.on('pageerror', e => report.browserErrors.push(e.message))
    p.on('response', async response => {
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
      if (['send','resume'].includes(item.request.type)) { const stream = { item, chunks: [] }; streams.set(event.requestId, stream); allStreams.push(stream) }
    }
    })
    cdp.on('Network.responseReceived', event => {
    const stream = streams.get(event.requestId)
    if (!stream) return
    stream.item.http = event.response.status
    captureJobs.push(cdp.send('Network.streamResourceContent', { requestId: event.requestId }).then(result => {
      if (result.bufferedData) stream.chunks.unshift(Buffer.from(result.bufferedData, 'base64'))
    }).catch(async error => {
      // Small terminal/error responses can finish before streaming capture attaches.
      // CDP reads the completed resource without cloning fetch or prolonging its lifetime.
      try { const body=await cdp.send('Network.getResponseBody',{requestId:event.requestId});stream.chunks=[Buffer.from(body.body,body.base64Encoded?'base64':'utf8')] }
      catch(fallback){stream.item.captureError=String(error)+'; completed resource: '+String(fallback)}
    }))
    })
    cdp.on('Network.dataReceived', event => { const stream = streams.get(event.requestId); if (stream && event.data) stream.chunks.push(Buffer.from(event.data, 'base64')) })
  }
  await observe(page, context)
  function capturedEvents() {
    for (const { item, chunks } of allStreams) {
      item.body = Buffer.concat(chunks).toString('utf8')
      assert.ok(!item.captureError, item.captureError)
      assert.ok(['office-1','office-2','office-reserved'].includes(item.request.teamId), 'browser journey must use an allowed office')
      if(item.http!==200){item.errorEnvelope=JSON.parse(item.body);responseEnvelopeSchema.parse(item.errorEnvelope);assertProtocolValue(item.errorEnvelope);continue}
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
  await log.getByRole('heading',{name:'文件读取结果',exact:true}).waitFor()
  assert.equal(await log.locator('table').count(),1)
  assert.ok((await log.locator('pre code').innerText()).includes(markerA))
  await log.getByRole('button',{name:'复制回复',exact:true}).last().click()
  await log.getByRole('button',{name:'已复制',exact:true}).waitFor()
  report.clipboard=await page.evaluate(()=>navigator.clipboard.readText())
  assert.ok(report.clipboard.startsWith('我先读取这两个文件。\n\n# 文件读取结果'))
  const state=await waitFor(async()=>{const r=await readOffice();return r.code===200&&conv(r,a)?.tasks[0]?.status==='completed'?r:false})
  const current=conv(state,a)
  assert.equal(current.tasks.length,1,'无业务计划时仅显示真实用户目标'); assert.ok(!current.tasks.some(t=>t.id.startsWith('tool:')||/call_|read_file/.test(t.title)))
  assert.ok(current.events.some(e=>e.kind==='task.assigned'));assert.ok(current.whiteboard.steps.length<=3)
  report.correlations={conversationA:a,firstTurn:aRequest.turnId,businessTasks:current.tasks.map(t=>t.id),runTask:current.tasks[0].id}
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
  assert.equal(replayEvents[0].text,report.clipboard)
  const changed=structuredClone(aRequest);changed.messages.at(-1).content='同turn不同内容'
  const conflictTurn=await fetch(hostBase+'/office/chat',{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify(changed)})
  assert.equal(conflictTurn.status,200)
  const conflictEvents=(await conflictTurn.text()).trim().split('\n').map(JSON.parse)
  assert.deepEqual(conflictEvents,[{type:'error',message:'本轮标识已用于其他内容，请重新同步会话后重试'}])
  assert.equal(report.modelRequests.length,modelCount)
  report.idempotency={request:aRequest,replayEvents,conflictHttp:conflictTurn.status,conflictEvents,modelCountBefore:modelCount,modelCountAfter:report.modelRequests.length}
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
  await waitFor(async()=>{const r=await readOffice();return conv(r,a)?.tasks.some(t=>t.summary.includes('已停止，实际操作已清理'))})
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
  const originalHistory=await control({...identity,type:'history'})
  assert.equal(originalHistory.turns.find(t=>t.id===aRequest.turnId).assistant,report.clipboard)
  // T4.2: extend the same frozen-Office journey, never patch application responses.
  const box=p=>p.getByRole('textbox',{name:'给办公室发送消息'})
  const transcriptOf=p=>p.getByRole('log',{name:'对话内容'})
  async function expand(p){const b=p.getByRole('button',{name:'展开对话',exact:true});if(await b.isVisible())await b.click()}
  async function select(p,title){await expand(p);await p.getByRole('tab').filter({has:p.getByText(title,{exact:true})}).click();await waitFor(()=>box(p).isEnabled())}
  async function fresh(p){await expand(p);await p.getByRole('button',{name:'新会话',exact:true}).first().click();await expand(p);await waitFor(()=>box(p).isEnabled())}
  async function uiSend(p,text){await waitFor(async()=>{if(!await box(p).isEnabled())return false;await box(p).fill(text);return p.getByRole('button',{name:'发送消息',exact:true}).isEnabled()});await p.getByRole('button',{name:'发送消息',exact:true}).click();await waitFor(async()=>(await transcriptOf(p).innerText()).includes('已处理：'+text),45000);await p.getByRole('button',{name:'停止生成',exact:true}).waitFor({state:'hidden',timeout:45000});return structuredClone(latestSend().request)}
  async function archived(p){return p.evaluate(async()=>{const db=await new Promise((resolve,reject)=>{const r=indexedDB.open('pawoffice-chat',2);r.onsuccess=()=>resolve(r.result);r.onerror=()=>reject(r.error)});return new Promise((resolve,reject)=>{const tx=db.transaction(['conversations','chat-pages'],'readonly'),a=tx.objectStore('conversations').getAll(),b=tx.objectStore('chat-pages').getAll();tx.oncomplete=()=>{db.close();resolve({directories:a.result,pages:b.result})};tx.onerror=()=>reject(tx.error)})})}
  async function switchOffice(p,name){const c=p.getByRole('button',{name:'收起聊天',exact:true});if(await c.isVisible())await c.click();await p.getByRole('button',{name:/^团队空间，当前团队/}).click();await p.getByRole('button',{name:'进入'+name,exact:true}).click();await p.getByRole('dialog').waitFor({state:'hidden'})}
  let restart=0
  async function stopHost(){host.p.kill('SIGTERM');await host.close}
  async function startHost(){host=start(resolve(binary),['engine-host','--config',config,'--port',new URL(hostBase).port],root,childEnv,'host-ui-restart-'+restart++);await waitFor(()=>host.stdout().includes('openpaw engine host listening on'))}
  await fresh(page)
  const modelBeforeDraft=report.modelRequests.length
  await box(page).fill('未发送草稿不建Run')
  assert.equal(report.modelRequests.length,modelBeforeDraft)
  const uiInitial=await uiSend(page,'UI-分页与草稿A')
  const uiA=uiInitial.conversationId
  await fresh(page);await uiSend(page,'UI-草稿B');await box(page).fill('B-DRAFT')
  await select(page,'UI-分页与草稿A');await box(page).fill('A-DRAFT')
  await select(page,'UI-草稿B');assert.equal(await box(page).inputValue(),'B-DRAFT')
  await page.getByRole('button',{name:/^切换会话：/}).click()
  await page.getByRole('textbox',{name:'搜索会话'}).fill('UI-分页')
  const search=page.getByRole('dialog',{name:'全部会话',exact:true})
  assert.equal(await search.locator('.office-conversation-select').count(),1)
  await page.screenshot({path:join(output,'08-search.png'),fullPage:true})
  await search.getByRole('button',{name:'UI-分页与草稿A',exact:true}).click()
  assert.equal(await box(page).inputValue(),'A-DRAFT')
  await page.getByRole('tab',{name:'UI-分页与草稿A',exact:true}).press('End');assert.equal(await box(page).inputValue(),'B-DRAFT')
  await page.getByRole('tab',{name:'UI-草稿B',exact:true}).press('Home');assert.notEqual(await box(page).inputValue(),'B-DRAFT')
  await select(page,'UI-分页与草稿A');await page.getByRole('tab',{name:'UI-分页与草稿A',exact:true}).press('ArrowRight');assert.equal(await box(page).inputValue(),'B-DRAFT')
  await page.getByRole('tab',{name:'UI-草稿B',exact:true}).press('ArrowLeft');assert.equal(await box(page).inputValue(),'A-DRAFT')
  await switchOffice(page,'独立办公室');await fresh(page);await uiSend(page,'UI-办公室C');await box(page).fill('C-DRAFT')
  await waitFor(async()=>JSON.stringify(await archived(page)).includes('C-DRAFT'))
  await switchOffice(page,'真实运行验收');await select(page,'UI-分页与草稿A');assert.equal(await box(page).inputValue(),'A-DRAFT')
  await page.reload({waitUntil:'domcontentloaded'});await chooseOffice();await select(page,'UI-分页与草稿A');assert.equal(await box(page).inputValue(),'A-DRAFT')
  await select(page,'UI-草稿B');assert.equal(await box(page).inputValue(),'B-DRAFT')
  await switchOffice(page,'独立办公室');await expand(page);await select(page,'UI-办公室C');assert.equal(await box(page).inputValue(),'C-DRAFT')
  await page.screenshot({path:join(output,'09-drafts-refresh.png'),fullPage:true})
  await switchOffice(page,'真实运行验收');await select(page,'UI-分页与草稿A')
  for(let i=1;i<=24;i++)await uiSend(page,'UI-PAGE-'+String(i).padStart(2,'0'))
  await waitFor(async()=>JSON.stringify(await archived(page)).includes('UI-PAGE-24'))
  // Independent browser storage tests real server pages (20+5), not a cleared user archive.
  const pagingContext=await browser.newContext({viewport:{width:1440,height:1050},locale:'zh-CN',recordVideo:{dir:join(output,'video')}})
  await pagingContext.tracing.start({screenshots:true,snapshots:true,sources:false})
  try {
    const p=await pagingContext.newPage();await observe(p,pagingContext)
    await p.goto(officeBase+'/office/',{waitUntil:'domcontentloaded'});await chooseOffice(p);await select(p,'UI-分页与草稿A')
    await waitFor(()=>report.browserRequests.some(r=>r.request.type==='history'&&r.request.conversationId===uiA&&r.json?.data?.hasMore))
    await stopHost();await transcriptOf(p).evaluate(el=>el.scrollTop=0)
    await p.getByRole('button',{name:'加载更早消息',exact:true}).click()
    await p.locator('.office-chat-history-page [role=alert]').waitFor()
    await p.screenshot({path:join(output,'10-older-offline.png'),fullPage:true})
    const failedCursor=report.browserRequests.findLast(r=>r.request.type==='history'&&r.request.cursor).request.cursor
    await startHost();await p.getByRole('button',{name:'加载更早消息',exact:true}).click()
    await waitFor(()=>report.browserRequests.some(r=>r.request.type==='history'&&r.request.cursor===failedCursor&&r.json?.data?.turns?.length))
    await transcriptOf(p).evaluate(el=>el.scrollTop=0);await waitFor(async()=>(await transcriptOf(p).innerText()).includes('UI-分页与草稿A'))
    const pages=report.browserRequests.filter(r=>r.request.type==='history'&&r.request.conversationId===uiA&&r.http===200&&r.json)
    const turns=new Map();for(const p of pages)for(const t of p.json.data.turns)turns.set(t.id,t)
    assert.equal(turns.size,25);assert.equal(new Set([...turns.values()].map(t=>t.user)).size,25)
    report.uiPagination={conversationId:uiA,turnCount:turns.size,failedCursor,pages};report.drafts=await archived(p)
    await p.screenshot({path:join(output,'11-older-restored.png'),fullPage:true})
    // The original browser now holds an old suffix; rejection must be actionable through Office.
    await uiSend(p,'UI-第二浏览器更新')
    const beforeReject=report.modelRequests.length
    await box(page).fill('UI-旧历史提交');await page.getByRole('button',{name:'发送消息',exact:true}).click()
    await waitFor(async()=>(await transcriptOf(page).innerText()).includes('会话历史已更新，请点击重新同步后重试'))
    assert.equal(report.modelRequests.length,beforeReject)
    report.staleHistory=structuredClone(latestSend())
    await page.screenshot({path:join(output,'12-actionable-history-conflict.png'),fullPage:true})
    await page.getByRole('button',{name:'重新同步',exact:true}).click()
    await waitFor(async()=>(await transcriptOf(page).innerText()).includes('已处理：UI-第二浏览器更新'))
    await uiSend(page,'UI-同步后继续')
    await page.screenshot({path:join(output,'13-history-resynced.png'),fullPage:true})
  } finally {await pagingContext.tracing.stop({path:join(output,'trace-pagination.zip')});await pagingContext.close()}
  await fresh(page);await box(page).fill('MODEL-ERROR-认证失败验收');await page.getByRole('button',{name:'发送消息',exact:true}).click()
  await waitFor(async()=>(await transcriptOf(page).innerText()).includes('回复未完成'),45000)
  await page.screenshot({path:join(output,'14-provider-auth-error.png'),fullPage:true})
  await page.getByRole('button',{name:'重新同步',exact:true}).click();await waitFor(async()=>!await page.getByText('正在同步聊天记录…',{exact:true}).isVisible())
  await uiSend(page,'UI-模型失败后恢复')
  await fresh(page);await stopHost();const beforeOffline=report.modelRequests.length
  await box(page).fill('UI-首次发送连接失败');await page.getByRole('button',{name:'发送消息',exact:true}).click()
  await waitFor(async()=>(await transcriptOf(page).innerText()).includes('回复未完成'))
  const offline=structuredClone(latestSend());assert.equal(offline.http,502);assert.equal(report.modelRequests.length,beforeOffline)
  assert.ok(!(await readOffice()).data.snapshot.conversations.some(c=>c.id===offline.request.conversationId))
  report.firstSendOffline=offline
  await page.screenshot({path:join(output,'15-first-send-offline.png'),fullPage:true})
  await startHost();await page.getByRole('button',{name:'重新同步',exact:true}).click();await waitFor(async()=>!await page.getByText('正在同步聊天记录…',{exact:true}).isVisible());await uiSend(page,'UI-首次失败后恢复')
  report.assertions.push('Markdown heading/table/code and clipboard equal streamed/replayed history','Real UI search, keyboard selection, per-conversation and per-office drafts survive refresh','25 real UI turns recovered as server pages 20+5; failed older-page retry keeps cursor','Dual browsers: safe in-band history conflict creates no Run; resync then send succeeds','Real provider HTTP401 and Host-offline failures remain visible and recoverable')
  // O11—O15: original Web tool, real browser, actual HTTP and reliable stop.
  await fresh(page);await send('WEB-READ','WEB-HTTP-MARKER')
  const webRequest=structuredClone(latestSend().request)
  const webState=await waitFor(async()=>{const r=await readOffice();return conv(r,webRequest.conversationId)?.tasks[0]?.status==='completed'?r:false})
  const webConversation=conv(webState,webRequest.conversationId)
  assert.ok(webConversation.whiteboard.steps.length>0)
  // T5 maps real Web calls to the existing search role; actor handoff stays in Office.
  assert.ok(webConversation.events.some(e=>e.kind==='task.assigned'&&e.employeeId==='search'))
  assert.ok(!webConversation.tasks.some(t=>t.title.includes('web_fetch')))
  assert.ok(webConversation.employees.some(e=>e.id==='search'&&e.sceneActorId==='data-agent'))
  assert.ok((await log.innerText()).includes('source_url:'));assert.ok((await log.innerText()).includes('retrieved_at_unix_ms:'))
  await page.screenshot({path:join(output,'16-real-web-fetch.png'),fullPage:true})
  await fresh(page);await send('WEB-SLOW')
  const webSlow=structuredClone(latestSend().request)
  await waitFor(()=>webFixture.active===1)
  const beforeWebRefresh=webFixture.requests.length
  await page.screenshot({path:join(output,'17-web-running.png'),fullPage:true})
  await page.reload({waitUntil:'domcontentloaded'});await chooseOffice();await expand(page)
  await page.getByRole('tab',{name:'WEB-SLOW',exact:true}).click()
  await waitFor(()=>report.browserRequests.some(r=>r.request.type==='resume'&&r.request.turnId===webSlow.turnId),45000)
  assert.equal(webFixture.requests.length,beforeWebRefresh,'refresh must not replay Web request')
  await fresh(page);await send('网络并行会话','已处理：网络并行会话')
  assert.equal(webFixture.active,1,'other conversation must complete while Web tool is waiting')
  await page.getByRole('tab',{name:'WEB-SLOW',exact:true}).click()
  await page.getByRole('button',{name:'停止生成',exact:true}).click()
  await waitFor(()=>webFixture.active===0&&webFixture.cancelled===1)
  await waitFor(()=>report.browserRequests.some(r=>r.request.type==='cancel'&&r.request.turnId===webSlow.turnId&&r.json?.data?.stopped===true))
  await waitFor(async()=>(await log.innerText()).includes('已停止生成'))
  const afterWebStop=await waitFor(async()=>{const r=await readOffice();return conv(r,webSlow.conversationId)?.tasks.some(t=>t.summary.includes('已停止，实际操作已清理'))?r:false})
  report.web={request:webRequest,snapshot:webConversation,slow:webSlow,stopped:conv(afterWebStop,webSlow.conversationId),fixture:webFixture}
  assert.equal(webFixture.requests.length,2)
  await page.screenshot({path:join(output,'18-web-cancelled-clean.png'),fullPage:true})
  report.assertions.push('Original web_fetch/fresh HTTP result correlated with browser, Run/call/tasks/whiteboard','Current time and explicitly enabled Web tools reach actual Provider request','Refresh detaches without repeating Web request; another conversation completes during Web wait','Explicit stop closes actual HTTP body wait before confirming cancelled')
  // D43: update capabilities while keeping the same real browser conversation and ledger.
  const enabledConfig=await readFile(config,'utf8')
  assert.ok(enabledConfig.includes(', "web_search", "web_fetch"'))
  await stopHost();await writeFile(config,enabledConfig.replace(', "web_search", "web_fetch"',''));await startHost()
  await fresh(page)
  for(let i=0;i<3;i++)await send('旧能力会话','这一轮我没有联网工具')
  const oldCapabilityRequest=structuredClone(latestSend().request)
  await page.screenshot({path:join(output,'19-existing-no-web-history.png'),fullPage:true})
  await stopHost();await writeFile(config,enabledConfig);await startHost()
  const beforeCapabilityFollow=webFixture.requests.length
  await page.reload({waitUntil:'domcontentloaded'});await chooseOffice();await expand(page)
  await page.getByRole('tab',{name:'旧能力会话',exact:true}).click()
  await send('长沙天气','WEB-HTTP-MARKER')
  const currentCapabilityRequest=structuredClone(latestSend().request)
  assert.equal(currentCapabilityRequest.conversationId,oldCapabilityRequest.conversationId)
  assert.equal(currentCapabilityRequest.messages.filter(m=>m.role==='assistant'&&m.content.includes('这一轮我没有联网工具')).length,3)
  assert.equal(webFixture.requests.length,beforeCapabilityFollow+1)
  const capabilityState=await waitFor(async()=>{const r=await readOffice();return conv(r,currentCapabilityRequest.conversationId)?.tasks.every(t=>t.status==='completed')?r:false})
  report.capabilityUpgrade={old:oldCapabilityRequest,current:currentCapabilityRequest,snapshot:conv(capabilityState,currentCapabilityRequest.conversationId)}
  await page.screenshot({path:join(output,'20-existing-session-web-enabled.png'),fullPage:true})
  report.assertions.push('D43: three old denials remain in the same conversation after enabling Web and restarting; ordinary short follow-up performs real HTTP without rewriting history')
  await send('今天是几号','HOST-DATE:')
  assert.equal(latestSend().request.conversationId,oldCapabilityRequest.conversationId)
  assert.equal(webFixture.requests.length,beforeCapabilityFollow+1)
  await stopHost();await writeFile(config,enabledConfig.replace(', "web_search", "web_fetch"',''));await startHost()
  await page.reload({waitUntil:'domcontentloaded'});await chooseOffice();await expand(page)
  await page.getByRole('tab',{name:'旧能力会话',exact:true}).click()
  await send('长沙天气','这一轮我没有联网工具')
  assert.equal(latestSend().request.conversationId,oldCapabilityRequest.conversationId)
  assert.equal(webFixture.requests.length,beforeCapabilityFollow+1)
  report.capabilityUpgrade.disabledAgain=structuredClone(latestSend().request)
  await page.screenshot({path:join(output,'21-existing-session-web-disabled-again.png'),fullPage:true})
  report.assertions.push('D43: same-session date uses actual current server context without unnecessary Web; disabling Web again grants no stale capability and performs zero HTTP')
  await stopHost();await writeFile(config,enabledConfig);await startHost()
  // T5: actual browser approval, original tools, official UI navigation and deletion.
  report.t5={};
  await fresh(page);await send('T5-拒绝报告');await page.getByRole('button',{name:'收起聊天',exact:true}).click();
  await page.getByRole('button',{name:'拒绝',exact:true}).waitFor();
  report.t5.rejectRequest=structuredClone(latestSend().request);
  assert.equal(await access(join(workspace,'reports/refused.txt')).then(()=>true,()=>false),false);
  await page.screenshot({path:join(output,'22-t5-pending-no-side-effects.png'),fullPage:true});
  await page.getByRole('button',{name:'拒绝',exact:true}).click();await expand(page);
  await page.getByRole('button',{name:'停止生成',exact:true}).waitFor({state:'hidden',timeout:45000});
  assert.equal(await access(join(workspace,'reports/refused.txt')).then(()=>true,()=>false),false);
  const deniedSnapshot=await waitFor(async()=>{const r=await readOffice();const c=conv(r,report.t5.rejectRequest.conversationId);return c?.tasks[0]?.status==='completed'?c:false});
  assert.ok(!deniedSnapshot.events.some(e=>e.summary.includes('write_file')),'拒绝写入不能触发执行交接动画');
  await fresh(page);await send('T5-允许报告');await page.getByRole('button',{name:'收起聊天',exact:true}).click();
  await page.getByRole('button',{name:'允许本次',exact:true}).waitFor();
  report.t5.approveRequest=structuredClone(latestSend().request);
  assert.equal(await access(join(workspace,'reports/T5报告.md')).then(()=>true,()=>false),false);
  const pending=await waitFor(async()=>{const r=await readOffice();const c=conv(r,report.t5.approveRequest.conversationId);return c?.tasks.some(t=>t.status==='blocked')?r:false});
  report.t5.pendingSnapshot=pending;
  await page.screenshot({path:join(output,'23-t5-approval-blocked.png'),fullPage:true});
  // 实际reload应撤旧展示再重开；只读核对原业务审批、输入和期限，不能直接HTTP代批。
  const approvalRun=conv(pending,report.t5.approveRequest.conversationId).tasks[0].id.slice('run:'.length);
  function approvalRecord(){
    const db=new DatabaseSync(join(output,'host-data/engine-host/host.db'),{readOnly:true});
    try {
      const approval=JSON.parse(db.prepare('SELECT data FROM host_approvals WHERE run=?').get(approvalRun).data);
      const display=db.prepare('SELECT * FROM host_office_decisions WHERE approval_id=?').get(approval.approval_id);
      return {approval,display};
    } finally {db.close()}
  }
  report.t5.beforeReload=approvalRecord();
  await page.reload({waitUntil:'domcontentloaded'});await chooseOffice();await select(page,'T5-允许报告');
  await page.getByRole('button',{name:'收起聊天',exact:true}).click();
  await page.getByRole('button',{name:'允许本次',exact:true}).waitFor();
  report.t5.afterReload=await waitFor(()=>{const r=approvalRecord();return r.display.state==='active'&&r.display.display_id!==report.t5.beforeReload.display.display_id?r:false});
  assert.deepEqual(report.t5.afterReload.approval,report.t5.beforeReload.approval,'重开不得替换业务审批/输入/期限');
  assert.equal(report.t5.afterReload.display.expires_at,report.t5.beforeReload.display.expires_at);
  assert.equal(await access(join(workspace,'reports/T5报告.md')).then(()=>true,()=>false),false);
  await page.screenshot({path:join(output,'23b-t5-reloaded-approval-same-binding.png'),fullPage:true});
  report.assertions.push('D47: real browser reload reopens a new display, preserves exact business approval/input/expiry and performs no write before browser approval');
  await fresh(page);await send('审批等待期间读取README.md和notes.txt并总结',markerA);
  report.t5.readWhilePending=structuredClone(latestSend().request);
  assert.notEqual(report.t5.readWhilePending.conversationId,report.t5.approveRequest.conversationId);
  const simultaneous=await readOffice();
  assert.equal(conv(simultaneous,report.t5.approveRequest.conversationId).tasks[0].status,'blocked');
  await page.screenshot({path:join(output,'28-t5-other-session-reads-during-approval.png'),fullPage:true});
  await switchOffice(page,'独立办公室');await fresh(page);
  await send('后台办公室读取README.md和notes.txt并总结',markerA);
  report.t5.readOtherOffice=structuredClone(latestSend().request);
  assert.equal(report.t5.readOtherOffice.teamId,'office-2');
  await page.getByRole('button',{name:'收起聊天',exact:true}).click();
  const notice=page.getByRole('region',{name:'真实运行验收待决定提示',exact:true});
  await notice.getByRole('button',{name:'允许本次',exact:true}).waitFor();
  await page.screenshot({path:join(output,'29-t5-background-office-direct-approval.png'),fullPage:true});
  await notice.getByRole('button',{name:'允许本次',exact:true}).click();
  await switchOffice(page,'真实运行验收');await select(page,'T5-允许报告');
  await page.getByRole('button',{name:'停止生成',exact:true}).waitFor({state:'hidden',timeout:45000});
  const artifact=await readFile(join(workspace,'reports/T5报告.md'),'utf8');assert.ok(artifact.includes(markerA)&&artifact.includes(markerB));
  report.t5.artifact={path:'reports/T5报告.md',sha256:createHash('sha256').update(artifact).digest('hex'),content:artifact};
  await page.locator('.runtime-person').filter({hasText:'王明'}).click();
  await page.getByRole('button',{name:'文件产物',exact:true}).click();
  await page.getByRole('button',{name:'打开 reports/T5报告.md',exact:true}).click();
  await page.getByText(/T5真实报告/).last().waitFor();
  await page.screenshot({path:join(output,'24-t5-file-detail-from-real-disk.png'),fullPage:true});
  await page.getByRole('button',{name:'返回目录',exact:true}).click();
  for(const name of ['历史','工具结果 / 拟执行','Skill','Goal','Todo']) {
    await page.getByRole('button',{name,exact:true}).click();
    await page.screenshot({path:join(output,'t5-detail-'+name.replaceAll('/','-')+'.png'),fullPage:true});
    await page.getByRole('button',{name:'返回目录',exact:true}).click();
  }
  // 等待最后一次导航真正返回目录，再用Office原关闭按钮，避免响应晚到后重开对话框。
  await page.getByRole('dialog',{name:'王明 · 成员详情',exact:true}).waitFor();
  await page.getByRole('button',{name:'关闭详情',exact:true}).click();
  await page.locator('dialog.office-ui-dialog').waitFor({state:'hidden'});
  await fresh(page);await send('T5-命令慢任务');await page.getByRole('button',{name:'收起聊天',exact:true}).click();await page.getByRole('button',{name:'允许本次',exact:true}).waitFor();
  report.t5.shellRequest=structuredClone(latestSend().request);
  await page.getByRole('button',{name:'允许本次',exact:true}).click();
  report.t5.shellRunning=await waitFor(async()=>{const r=await readOffice();const c=conv(r,report.t5.shellRequest.conversationId);return c?.tasks.some(t=>t.assigneeId==='command'&&t.status==='running')?r:false});
  report.t5.processes=await waitFor(async()=>{try{return await Promise.all(['parent','child'].map(async role=>({role,pid:Number(await readFile(join(workspace,'managed-'+role+'.pid'),'utf8'))})))}catch{return false}});
  for(const item of report.t5.processes){assert.ok(item.pid>0);if(process.platform==='linux')item.before=await readFile('/proc/'+item.pid+'/stat','utf8')}
  await page.screenshot({path:join(output,'25-t5-real-shell-running.png'),fullPage:true});
  await delay(2500);
  await page.screenshot({path:join(output,'25b-t5-live-scene-handoff.png'),fullPage:true});
  await expand(page);await page.getByRole('button',{name:'停止生成',exact:true}).click();
  await page.getByRole('button',{name:'停止生成',exact:true}).waitFor({state:'hidden',timeout:15000});
  // Office先清除本地生成标记；以真实cancel业务回执作为Host完成清理的界限。
  report.t5.cancelAcknowledged=await waitFor(()=>report.browserRequests.findLast(r=>r.request.type==='cancel'&&r.request.turnId===report.t5.shellRequest.turnId)?.json?.data?.stopped===true);
  for(const item of report.t5.processes){
    if(process.platform==='linux'){
      try{item.after=await readFile('/proc/'+item.pid+'/stat','utf8');assert.ok(['Z','X','x'].includes(item.after.slice(item.after.lastIndexOf(') ')+2).split(' ')[0]),'清理返回后仍有可运行后代')}
      catch(e){if(!['ENOENT','ESRCH'].includes(e.code))throw e;item.after='exited: '+e.code}
    }else{let alive=false;try{process.kill(item.pid,0);alive=true}catch(e){if(e.code!=='ESRCH')throw e}assert.equal(alive,false)}
  }
  await delay(8500);assert.equal(await access(join(workspace,'late-side-effect')).then(()=>true,()=>false),false);
  await page.screenshot({path:join(output,'26-t5-shell-cancelled-clean.png'),fullPage:true});
  await select(page,'T5-允许报告');
  await page.getByRole('button',{name:'删除会话：T5-允许报告',exact:true}).click();
  await page.getByRole('alertdialog',{name:'删除会话'}).getByRole('button',{name:'删除',exact:true}).click();
  await waitFor(async()=>!conv(await readOffice(),report.t5.approveRequest.conversationId));
  await page.getByRole('tab',{name:'T5-允许报告',exact:true}).waitFor({state:'hidden',timeout:15000});
  assert.equal(await readFile(join(workspace,'reports/T5报告.md'),'utf8'),artifact);
  report.t5.deletedHistory=await fetch(hostBase+'/office/chat',{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify({version:'1.0',teamId:'office-1',conversationId:report.t5.approveRequest.conversationId,type:'history'})}).then(r=>({status:r.status}));
  assert.notEqual(report.t5.deletedHistory.status,200);
  const modelCountAfterDelete=report.modelRequests.length;
  report.t5.deletedRetries=[];
  for(const type of ['send','resume','cancel']) {
    const request=type==='send'?report.t5.approveRequest:{version:'1.0',teamId:'office-1',conversationId:report.t5.approveRequest.conversationId,type,turnId:report.t5.approveRequest.turnId};
    const r=await fetch(hostBase+'/office/chat',{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify(request)});
    const body=await r.text();assert.ok(!body.includes('"type":"done"'));
    assert.ok(r.status!==200||body.includes('"type":"error"'),'已删会话不能假成功');
    report.t5.deletedRetries.push({type,http:r.status,body});
  }
  assert.equal(report.modelRequests.length,modelCountAfterDelete,'旧请求不能复活会话或调用模型');
  await page.screenshot({path:join(output,'27-t5-deleted-conversation-artifact-preserved.png'),fullPage:true});
  report.assertions.push('T5: real browser reject leaves no artifact; confirm executes exact original write_file once','T5: blocked approval snapshot and six original read-only detail categories; generated file matches actual disk hash','T5: actual bash working/assignment, explicit stop prevents delayed descendant side effect','T5: official conversation.delete removes snapshot/history and keeps workspace artifact');
  // r2业务目标：同一原引擎计划、HTTP、人工审批和真实磁盘推动进度。
  await fresh(page);await send('帮我查询资料并记录到reports/天气.md');
  const progressRequest=structuredClone(latestSend().request);
  await page.getByRole('button',{name:'收起聊天',exact:true}).click();
  await page.getByRole('button',{name:'允许本次',exact:true}).waitFor();
  const progressPending=await waitFor(async()=>{const r=await readOffice();const c=conv(r,progressRequest.conversationId);return c?.tasks.length===4&&c.tasks[0].status==='blocked'?c:false});
  assert.equal(progressPending.tasks[0].progress,25);
  assert.equal(progressPending.tasks[1].status,'completed');
  assert.ok(!progressPending.tasks.some(t=>/call_|web_fetch|write_file|report_progress/.test(t.title)));
  assert.equal(await access(join(workspace,'reports/天气.md')).then(()=>true,()=>false),false);
  await page.screenshot({path:join(output,'30-business-plan-awaiting-save.png'),fullPage:true});
  await page.getByRole('button',{name:'允许本次',exact:true}).click();await expand(page);
  await waitFor(async()=>(await log.innerText()).includes('资料已通过真实HTTP取回'),45000);
  const progressComplete=await waitFor(async()=>{const c=conv(await readOffice(),progressRequest.conversationId);return c?.tasks[0]?.progress===100?c:false});
  assert.ok(progressComplete.tasks.every(t=>t.status==='completed'));
  assert.ok(progressComplete.whiteboard.description.includes('3/3'));
  assert.equal(progressComplete.whiteboard.note,'');assert.equal(progressComplete.whiteboard.tagline,'');
  const recorded=await readFile(join(workspace,'reports/天气.md'),'utf8');assert.ok(recorded.includes('WEB-HTTP-MARKER'));
  await page.screenshot({path:join(output,'31-business-plan-completed.png'),fullPage:true});
  report.t5.progress={request:progressRequest,pending:progressPending,completed:progressComplete,file:{path:'reports/天气.md',sha256:createHash('sha256').update(recorded).digest('hex')}};
  report.assertions.push('D48: original HTTP Provider receives strict rejection of a fabricated evidence alias, reads exact current-run evidence from the diagnostic, then completes the plan using the real approved write call');
  report.assertions.push('D48: successful original fetch/write results preserve their bodies and expose exact Host execution receipts; the HTTP Provider reads the fetch call ID from the receipt and sees the current plan after accepted reports');
  await page.reload({waitUntil:'domcontentloaded'});await chooseOffice();await select(page,'帮我查询资料并记录到reports/天气.md');
  const restored=conv(await readOffice(),progressRequest.conversationId);assert.deepEqual(restored.tasks,progressComplete.tasks);
  assert.deepEqual(restored.events,progressComplete.events,'刷新不改变实际开始时的步骤关联');
  await send('核对已保存的记录文件位置');
  const locationRequest=structuredClone(latestSend().request);
  const locationComplete=await waitFor(async()=>{const c=conv(await readOffice(),locationRequest.conversationId);return c?.tasks[0]?.id!==progressComplete.tasks[0].id&&c?.tasks[0]?.progress===100?c:false});
  assert.ok(locationComplete.tasks.every(t=>t.status==='completed'));
  assert.equal(await readFile(join(workspace,'reports/天气.md'),'utf8'),recorded,'确认文件位置不能再次写入');
  report.t5.savedLocation={request:locationRequest,completed:locationComplete};
  await page.screenshot({path:join(output,'31b-saved-location-query-completed.png'),fullPage:true});
  report.assertions.push('D49: original browser follows up about an already saved record location; real glob evidence completes the query plan without another write or approval');
  // r3官方新建入口：真实空作用域action经过唯一poller和原Store。
  await page.getByRole('button',{name:'收起聊天',exact:true}).click();
  await page.getByRole('button',{name:/^团队空间，当前团队/}).click();
  await page.getByRole('button',{name:'新建办公室',exact:true}).first().click();
  await page.getByRole('textbox',{name:'办公室名称',exact:true}).fill('新建验收办公室');
  await page.getByRole('button',{name:'创建办公室',exact:true}).click();
  await page.getByRole('dialog').waitFor({state:'hidden',timeout:30000});
  await waitFor(async()=>{const r=await officePost(officeBase,{type:'office.read'});return r.data.offices.some(o=>o.id==='office-reserved'&&o.name==='新建验收办公室')});
  await fresh(page);await send('新办公室协议问答','网页协议对话正常');
  const createdRequest=structuredClone(latestSend().request);assert.equal(createdRequest.teamId,'office-reserved');
  report.t5.creation={request:createdRequest,snapshot:(await officePost(officeBase,{type:'office.read',teamId:'office-reserved'})).data.snapshot};
  await page.screenshot({path:join(output,'32-real-browser-created-office-chat.png'),fullPage:true});
  await switchOffice(page,'真实运行验收');
  report.assertions.push('r2: browser goal + three business steps; 25% before approval, 100% only after exact real file write and reliable answer; stable refresh/events','r3: official office.create creates configured real workspace slot and conversation through untouched latest Office UI');
  await fresh(page);const longStarted=Date.now();await send('长连接180秒验收');
  await waitFor(async()=>(await log.innerText()).includes('长连接开始。'));
  const longRequest=structuredClone(latestSend().request);
  await waitFor(async()=>(await log.innerText()).includes('长连接真实完成。'),210000);
  await page.getByRole('button',{name:'停止生成',exact:true}).waitFor({state:'hidden',timeout:20000});
  report.t5.longConnection={request:longRequest,milliseconds:Date.now()-longStarted};assert.ok(report.t5.longConnection.milliseconds>180000);
  assert.equal(report.browserRequests.filter(r=>r.request.type==='send'&&r.request.turnId===longRequest.turnId).length,1);
  await page.screenshot({path:join(output,'33-official-chat-over-180-seconds.png'),fullPage:true});
  report.assertions.push('r3: untouched latest Office browser receives one original stream over 180 seconds; Host idle and Run budgets stay enabled');
  // Opt-in collaboration only after every original single-Agent journey has run unchanged.
  host.p.kill('SIGTERM');await host.close
  await writeFile(config,(await readFile(config,'utf8'))+'\n[agents]\nenabled = true\n')
  host=start(resolve(binary),['engine-host','--config',config,'--port',new URL(hostBase).port],root,childEnv,'host-team')
  await waitFor(()=>host.stdout().includes('openpaw engine host listening on'))
  await teamProduct({page,report,output,readOffice,conv,latestSend,fresh,send,waitFor,release:role=>{assert.ok(teamGates.has(role));teamGates.get(role)();teamGates.delete(role)}})
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
  // 比较真实工具事实，不把新增合法查询误当重放；全部T5会话同样受此保护。
  const sessionDir = join(output, 'host-data/sessions')
  const toolFacts = async()=>Object.fromEntries(await Promise.all((await readdir(sessionDir)).sort().map(async file=>[
    file,(await readFile(join(sessionDir,file),'utf8')).trim().split('\n').map(JSON.parse).filter(e=>['tool_call_start','tool_call_complete'].includes(e.type)),
  ])))
  report.toolFactsBeforeRestart=await toolFacts()
  assert.ok(Object.values(report.toolFactsBeforeRestart).some(events=>events.length>0),'比较必须包含真实工具事实')
  host=start(resolve(binary),['engine-host','--config',config,'--port',new URL(hostBase).port],root,childEnv,'host-restarted')
  await waitFor(()=>host.stdout().includes('openpaw engine host listening on'))
  await waitFor(async()=>{const r=await readOffice();return conv(r,crashId)?.tasks.some(t=>t.summary.includes('处理被中断'))},45000)
  const crashHistory=await control({version:'1.0',type:'history',teamId:'office-1',conversationId:crashId});chatHistoryPageSchema.parse(crashHistory);assert.equal(crashHistory.turns[0].state,'error');assert.equal(crashHistory.turns[0].id,crashTurn)
  assert.equal(report.modelRequests.length,requestsBeforeCrash);await crashRequest
  const recovered=await readOffice()
  await page.close();officeProcess.p.kill('SIGTERM');await officeProcess.close
  officeProcess=start(process.execPath,['server/dist/index.js'],office,{...childEnv,PORT:String(officePort),HOST:'127.0.0.1',PAWOFFICE_PUBLIC_ORIGIN:officeBase},'office-offline-restarted')
  await waitFor(async()=>{try{const r=await officePost(officeBase,{type:'office.snapshot',teamId:'office-1',snapshot:recovered.data.snapshot});return r.code===202&&r.data.status==='cached'?r:false}catch{return false}},45000)
  report.transcript=transcript
  await Promise.all(captureJobs);capturedEvents()
  assert.ok(report.browserRequests.some(r=>r.http===200&&r.events?.some(terminal)))
  const initialStream=report.browserRequests.find(r=>r.request.type==='send'&&r.request.turnId===aRequest.turnId)
  assert.equal(initialStream.events.filter(e=>e.type==='delta').map(e=>e.text).join(''),report.clipboard)
  const rejected=report.browserRequests.find(r=>r.request.type==='send'&&r.request.turnId===report.staleHistory.request.turnId)
  assert.equal(rejected.http,200);assert.deepEqual(rejected.events,[{type:'error',message:'会话历史已更新，请点击重新同步后重试'}])
  assert.equal((await control({...identity,type:'history'})).turns.find(t=>t.id===aRequest.turnId).assistant,report.clipboard)
  assert.equal(report.browserErrors.length,0)
  report.assertions.push('Real browser same-origin four-operation chat through unmodified latest Office','A/B conversations independent tasks and whiteboards; A running while B completes','Original read_file outputs correlated with Run/call/scene snapshot','Same turn retry reuses Run; different content conflicts','Refresh history/resume detaches without duplicate model/tool execution','Explicit cancel waits for cleanup','New browser discovers original conversation IDs and stored history','Stable paginated history and stopped state','Same seq retry/conflict/stale and offline 202 from real service','Fastify restart re-registers chat.url','Host crash interrupted without replay; stable turn survives restart')
  report.sessions = await readdir(sessionDir)
  report.toolFactsAfterRestart=await toolFacts()
  assert.deepEqual(report.toolFactsAfterRestart,report.toolFactsBeforeRestart,'所有会话重启后工具事实必须精确相等，不允许重放')
  for (const file of report.sessions) {
    const ledger = (await readFile(join(sessionDir, file), 'utf8')).trim().split('\n').map(JSON.parse)
    let activeRun='';const calls=new Set();for(const e of ledger){if(e.type==='run_started')activeRun=e.run_id;if(e.type==='tool_call_complete'){const key=activeRun+':'+e.call_id;assert.ok(!calls.has(key),'same Run/call executed twice');calls.add(key)}}
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
  if (webFixtureServer) { webFixtureServer.closeAllConnections(); await new Promise(r=>webFixtureServer.close(r)) }
  if (providerServer) { providerServer.closeAllConnections(); await new Promise(r => providerServer.close(r)) }
  report.finishedAt = new Date().toISOString(); await writeFile(join(output, 'report.json'), JSON.stringify(report, null, 2))
  console.log(JSON.stringify({ result: report.result, output, error: report.error }, null, 2))
}
