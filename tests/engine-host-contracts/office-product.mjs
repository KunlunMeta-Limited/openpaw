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
import { chatEventSchema } from './upstream-office/packages/protocol/src/chat.ts'
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
        const timer = setTimeout(() => { if (!res.destroyed) finish({ content: '慢任务完成' }) }, 30000)
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
  const buildEnv = { ...childEnv, VITE_PAWOFFICE_CHAT_URL: hostBase + '/office/chat' }
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
  cdp.on('Network.requestWillBeSent', event => {
    if (event.request.url === hostBase + '/office/chat' && event.request.method === 'POST') {
      const item = { at: new Date().toISOString(), url: event.request.url, method: 'POST', request: JSON.parse(event.request.postData) }
      report.browserRequests.push(item); streams.set(event.requestId, { item, chunks: [] })
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
      item.events = item.body.split('\n').filter(Boolean).map(line => { assert.ok(line.length <= 32000); const event = JSON.parse(line); chatEventSchema.parse(event); assertProtocolValue(event); return event })
    }
  }
  await page.goto(officeBase + '/office/', { waitUntil: 'domcontentloaded' })
  const input = page.getByRole('textbox', { name: '给办公室发送消息' }); await input.waitFor({ timeout: 60000 })
  await page.getByText(/正在加载办公室/).waitFor({ state: 'hidden', timeout: 60000 })
  await waitFor(async () => { const directory = await officePost(officeBase, { type: 'office.read' }); return directory.code === 200 && ['office-1', 'office-2'].every(id => directory.data.offices.some(o => o.id === id && o.runtimeReady)) })
  // Workers initialize independently; never assume which office the UI initially selects.
  await page.getByRole('button', { name: /^团队空间，当前团队/ }).click()
  await page.getByRole('button', { name: '进入真实运行验收', exact: true }).click()
  await page.getByRole('dialog').waitFor({ state: 'hidden' })
  await page.getByRole('button', { name: /^团队空间，当前团队：真实运行验收/ }).waitFor()
  await page.screenshot({ path: join(output, '01-connected.png'), fullPage: true })
  async function send(text, expected) {
    // Switching offices restores its IndexedDB chat asynchronously; fill only after ready,
    // and tolerate the draft being replaced once by that restoration before submitting.
    await waitFor(async () => { if (!await input.isEnabled()) return false; await input.fill(text); return await page.getByRole('button', { name: '发送消息', exact: true }).isEnabled() })
    await page.getByRole('button', { name: '发送消息', exact: true }).click()
    await waitFor(async () => (await page.getByRole('log', { name: '对话内容' }).innerText()).includes(expected), 30000)
    await page.getByRole('button', { name: '停止生成', exact: true }).waitFor({ state: 'hidden', timeout: 30000 })
  }
  await send('你好', '网页协议对话正常')
  assert.equal(await page.getByText('交互预览', { exact: true }).isVisible(), false)
  await send('读取README.md和notes.txt并总结', markerA)
  const transcript = await page.getByRole('log', { name: '对话内容' }).innerText(); assert.ok(transcript.includes(markerB))
  await page.screenshot({ path: join(output, '02-real-tools.png'), fullPage: true })
  await send('追问：刚才两个文件分别包含什么？', '已保留历史')
  const state = await waitFor(async () => { const result = await officePost(officeBase, { type: 'office.read', teamId: 'office-1' }); return result.code === 200 && result.data.snapshot.tasks.filter(t => t.id.startsWith('tool:')).length === 2 ? result : false })
  assert.ok(state.data.snapshot.tasks.some(t => t.title.includes('read_file') && t.status === 'completed'))
  assert.ok(state.data.snapshot.events.some(e => e.kind === 'task.assigned'))
  assert.ok(state.data.snapshot.whiteboard.steps.length <= 3)
  await page.getByRole('button', { name: '收起对话', exact: true }).first().click(); await delay(300)
  await page.getByRole('button', { name: /任务中心/ }).click()
  await page.screenshot({ path: join(output, '03-tasks-and-whiteboard.png'), fullPage: true })
  // Validate the real server's version semantics; no fixture can replace this journey.
  const saved = structuredClone(state.data.snapshot)
  const same = await officePost(officeBase, { type: 'office.snapshot', teamId: 'office-1', snapshot: saved }); assert.ok([200, 202].includes(same.code))
  const changed = structuredClone(saved); changed.workspace.name = '同版本不同内容'
  const conflict = await officePost(officeBase, { type: 'office.snapshot', teamId: 'office-1', snapshot: changed }); assert.equal(conflict.code, 409); assert.equal(conflict.data.reasonCode, 'SEQ_CONFLICT')
  if (saved.seq > 1) { const old = structuredClone(saved); old.seq--; const stale = await officePost(officeBase, { type: 'office.snapshot', teamId: 'office-1', snapshot: old }); assert.equal(stale.data.status, 'ignored') }
  await input.fill('慢任务：请等待'); await page.getByRole('button', { name: '发送消息', exact: true }).click()
  await page.getByRole('button', { name: '停止生成', exact: true }).waitFor()
  await delay(200); await page.getByRole('button', { name: '停止生成', exact: true }).click()
  await waitFor(async () => (await page.getByRole('log', { name: '对话内容' }).innerText()).includes('已停止生成'))
  await waitFor(async () => { const r = await officePost(officeBase, { type: 'office.read', teamId: 'office-1' }); return r.code === 200 && r.data.snapshot.tasks.some(t => t.summary.includes('用户取消，清理已完成')) })
  await page.screenshot({ path: join(output, '04-stop-confirmed.png'), fullPage: true })
  // Fastify restart clears its memory directory. Host must reinitialize from its durable version.
  officeProcess.p.kill('SIGTERM'); await officeProcess.close
  officeProcess = start(process.execPath, ['server/dist/index.js'], office, { ...childEnv, PORT: String(officePort), HOST: '127.0.0.1', PAWOFFICE_PUBLIC_ORIGIN: officeBase }, 'office-restarted')
  await waitFor(async () => { try { const r = await officePost(officeBase, { type: 'office.read' }); return r.code === 200 && r.data.offices.some(o => o.id === 'office-1') } catch { return false } })
  await page.reload({ waitUntil: 'domcontentloaded' }); await input.waitFor({ timeout: 60000 })
  await waitFor(async () => { const r = await officePost(officeBase, { type: 'office.read', teamId: 'office-1' }); return r.code === 200 && r.data.snapshot.tasks.some(t => t.title.includes('read_file')) }, 45000)
  await page.getByRole('button', { name: /^团队空间，当前团队/ }).click()
  await page.getByRole('button', { name: '进入真实运行验收', exact: true }).click()
  await page.getByRole('dialog').waitFor({ state: 'hidden' })
  await page.screenshot({ path: join(output, '05-office-restart-recovered.png'), fullPage: true })
  // Crash an accepted, still-waiting run. Restart the same Host port/data;
  // v1 has no resume, but interrupted metadata and original tools must survive.
  const crashRequest = fetch(hostBase + '/office/chat', { method: 'POST', headers: { 'content-type': 'application/json', Origin: officeBase }, body: JSON.stringify({ version: '1.0', teamId: 'office-1', conversationId: 'host-crash-' + Date.now(), messages: [{ role: 'user', content: '慢任务：模拟Host崩溃' }] }) }).catch(() => null)
  await waitFor(() => report.modelRequests.some(r => r.messages.some(m => m.role === 'user' && m.content === '慢任务：模拟Host崩溃')))
  const requestsBeforeCrash = report.modelRequests.length
  host.p.kill('SIGKILL'); await host.close
  host = start(resolve(binary), ['engine-host', '--config', config, '--port', new URL(hostBase).port], root, childEnv, 'host-restarted')
  await waitFor(() => host.stdout().includes('openpaw engine host listening on'))
  await waitFor(async () => { const r = await officePost(officeBase, { type: 'office.read', teamId: 'office-1' }); return r.code === 200 && r.data.snapshot.tasks.some(t => t.summary.includes('Host重启中断')) }, 45000)
  await page.screenshot({ path: join(output, '06-host-crash-interrupted.png'), fullPage: true })
  assert.equal(report.modelRequests.length, requestsBeforeCrash, 'restart must not replay Provider/tool work')
  await crashRequest
  const recovered = await officePost(officeBase, { type: 'office.read', teamId: 'office-1' })
  await page.close()
  // Restart Office after the Host crash. During its startup the official API accepts
  // the snapshot into its durable cache and returns 202; the coordinator applies it
  // after capabilities/initialization, so this is observed through the real API.
  officeProcess.p.kill('SIGTERM'); await officeProcess.close
  officeProcess = start(process.execPath, ['server/dist/index.js'], office, { ...childEnv, PORT: String(officePort), HOST: '127.0.0.1', PAWOFFICE_PUBLIC_ORIGIN: officeBase }, 'office-offline-restarted')
  const offline = await waitFor(async () => { try {
    const r = await officePost(officeBase, { type: 'office.snapshot', teamId: 'office-1', snapshot: recovered.data.snapshot })
    return r.code === 202 && r.data.status === 'cached' ? r : false
  } catch { return false } }, 45000)
  report.assertions.push('Host crash marks accepted run interrupted without replay', 'No browser: real Office returns 202 cached rather than applied')
  report.transcript = transcript // Captured before closing the page for offline-cache validation
  await Promise.all(captureJobs); capturedEvents()
  assert.ok(report.browserRequests.some(r => r.http === 200 && r.events?.some(e => e.type === 'done')))
  assert.equal(report.browserErrors.length, 0)
  report.assertions.push('Real browser POST /office/chat and strict upstream events', 'Actual original read_file outputs contain two unpredictable file markers', 'Follow-up uses original session tool history', 'Run/tool tasks, task.assigned and whiteboard read from real Office', 'Same-version duplicate/conflict and stale-version behavior', 'Browser stop cancels Host run', 'Office service restart reinitialized without replaying tools')
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
