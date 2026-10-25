// 逐轮真实AgentLoop回归；可选冻结Office/Chromium，测试Provider不冒充真实模型。
import { createServer } from 'node:http'
import { spawn, execFileSync } from 'node:child_process'
import { once } from 'node:events'
import { mkdir, writeFile, readFile, mkdtemp, copyFile, unlink } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join, resolve, dirname } from 'node:path'
import { fileURLToPath, pathToFileURL } from 'node:url'
import { createHash } from 'node:crypto'
import assert from 'node:assert/strict'
import { DatabaseSync } from 'node:sqlite'

const root = resolve(dirname(fileURLToPath(import.meta.url)), '../..')
const out = await mkdtemp(join(process.env.OPENPAW_REPEAT_OUTPUT_PARENT || tmpdir(), 'openpaw-status-repeat-'))
const singleCount = Number(process.env.OPENPAW_REPEAT_SINGLE ?? 100)
const multiCount = Number(process.env.OPENPAW_REPEAT_MULTI ?? 100)
assert.ok([singleCount, multiCount].every(n => Number.isInteger(n) && n >= 0))
assert.ok(singleCount <= 100 && multiCount <= 200 && singleCount + Math.max(0, multiCount - 100) <= 100, '每个团队最多100个独立会话')
const runtime = process.env.OPENPAW_REPEAT_OFFICE_RUNTIME
const delay = ms => new Promise(r => setTimeout(r, ms))
const workspace = join(out, 'workspace'), workspace2 = join(out, 'workspace-2'), home = join(out, 'home')
for (const p of [workspace, workspace2, home]) await mkdir(p)
await writeFile(join(workspace, 'README.md'), 'repeat fixture\n')
await writeFile(join(workspace2, 'README.md'), 'repeat fixture 2\n')
const report = { out, startedAt: new Date().toISOString(), scriptSha256: createHash('sha256').update(await readFile(fileURLToPath(import.meta.url))).digest('hex'), commit: execFileSync('git', ['rev-parse', 'HEAD'], { cwd: root }).toString().trim(),
  dirty: Boolean(execFileSync('git', ['status', '--porcelain'], { cwd: root }).toString().trim()),
  provider: 'deterministic OpenAI fixture, real AgentLoop; no live weather claims', mode: runtime ? 'real Office + Chromium UI' : 'HTTP fixture, no browser',
  requested: { single: singleCount, multi: multiCount }, runs: [], failures: [], pageErrors: [], httpErrors: [], clicks: [], idleControls: [], transitions: [], status: 'running' }
const processes = []
let browser, officeProcess, host, provider, office, hostConfig, hostEnvironment, hostBinary
let activeTeam
const snapshots = new Map(), names = ['王明', '李研', '周理', '陈书', '刘市', '赵审']
const employeeIds = ['office-lead', 'code-agent', 'file-agent', 'app-agent', 'review-agent', 'data-agent']
async function persist() { report.updatedAt = new Date().toISOString(); await writeFile(join(out, 'report.json'), JSON.stringify(report, null, 2)) }
function start(file, args, cwd, env, label) {
  const p = spawn(file, args, { cwd, env, stdio: ['ignore', 'pipe', 'pipe'] }); let stdout = '', stderr = ''
  p.stdout.on('data', b => { stdout += b }); p.stderr.on('data', b => { stderr += b })
  const item = { p, close: once(p, 'close'), stdout: () => stdout, save: async () => { await writeFile(join(out, label + '.stdout.log'), stdout); await writeFile(join(out, label + '.stderr.log'), stderr) } }
  processes.push(item); return item
}
async function waitFor(fn, timeout = 60000) {
  const end = Date.now() + timeout
  while (Date.now() < end) { const v = await fn(); if (v) return v; await delay(100) }
  throw Error('Timed out waiting for real state')
}
async function listen(s) { s.listen(0, '127.0.0.1'); await once(s, 'listening'); return s.address().port }
function envelope(res, code, data) { res.writeHead(code, { 'content-type': 'application/json' }); res.end(JSON.stringify({ code, msg: '', data })) }
let officeBase, officePort, page, base, lastSend
const officeKey = 'deterministic-office-key'
async function readOffice(team) {
  if (!runtime) return snapshots.get(team)
  const r = await fetch(officeBase + '/api/pawoffice/messages', { method: 'POST', headers: { 'content-type': 'application/json', authorization: 'Bearer ' + officeKey }, body: JSON.stringify({ type: 'office.read', teamId: team }), signal: AbortSignal.timeout(15000) })
  const body = await r.json()
  if (r.status === 409 && body.data?.reasonCode === 'NO_RECEIVER') {
    report.httpErrors.push({ status: r.status, reasonCode: body.data.reasonCode, phase: 'page receiver reconnecting' })
    return undefined
  }
  assert.equal(r.status, 200, JSON.stringify(body)); return body.data.snapshot
}
async function disableIdleChatter(phase) {
  // 场景控件异步出现，isVisible的一次检查不能证明已经关闭。
  // 只操作公开按钮并核验其状态，不写入场景或假造人物点击。
  const control = page.getByRole('button', { name: /^(开启|关闭)空闲闲聊$/ })
  await control.waitFor({ state: 'visible' })
  await waitFor(() => control.isEnabled())
  const before = await control.getAttribute('aria-pressed')
  assert.ok(before === 'true' || before === 'false')
  if (before === 'true') await control.click()
  await waitFor(async () => await control.getAttribute('aria-pressed') === 'false')
  assert.equal(await control.getAttribute('aria-label'), '开启空闲闲聊')
  report.idleControls.push({ phase, checkedAt: new Date().toISOString(), before, after: 'false' })
  report.idleChatter = 'disabled and verified through public UI before each round and Pixi click; default-traffic R17 remains unresolved'
}
async function chooseOffice(team) {
  await page.getByText(/正在加载办公室/).waitFor({ state: 'hidden', timeout: 60000 })
  const collapse = page.getByRole('button', { name: '收起聊天', exact: true }); if (await collapse.isVisible()) await collapse.click()
  await page.getByRole('button', { name: /^团队空间，当前团队/ }).click()
  await page.getByRole('button', { name: '进入' + team, exact: true }).click()
  await page.getByRole('dialog').waitFor({ state: 'hidden' })
  await page.getByText(/正在加载办公室/).waitFor({ state: 'hidden', timeout: 60000 })
  await disableIdleChatter('choose-' + team)
}
async function sceneSnapshot(file) {
  const pending = page.waitForEvent('download')
  await page.getByRole('button', { name: '导出快照', exact: true }).click()
  const path = join(out, file)
  await (await pending).saveAs(path)
  return JSON.parse(await readFile(path, 'utf8'))
}
async function canvasPoint(x, y) {
  const canvas = await page.locator('canvas').first().boundingBox(); assert.ok(canvas)
  const scale = Math.min(canvas.width / 1100, canvas.height / 850)
  return [canvas.x + (canvas.width - 1100 * scale) / 2 + x * scale,
    canvas.y + (canvas.height - 850 * scale) / 2 + y * scale]
}
async function send(prompt, i, kind) {
  const startedAt = new Date().toISOString()
  let identity, events, earlySnapshots = [], replyText
  if (runtime) {
    const expand = page.getByRole('button', { name: '展开对话', exact: true }); if (await expand.isVisible()) await expand.click()
    await page.getByRole('button', { name: '新会话', exact: true }).first().click()
    const box = page.getByRole('textbox', { name: '给办公室发送消息' }); await box.waitFor()
    await waitFor(() => box.isEnabled()); await box.fill(prompt)
    await page.getByRole('button', { name: '发送消息', exact: true }).click()
    await waitFor(() => lastSend?.messages?.some(m => m.content === prompt))
    identity = structuredClone(lastSend)
    // 从发送时就采样。等待文字结束后才观察，会漏掉先于回复公开的交接帧。
    let sampling = true, sampleError, previousSeq
    const collector = (async () => {
      while (sampling) {
        const snapshot = await readOffice(identity.teamId)
        if (snapshot && snapshot.seq !== previousSeq) { earlySnapshots.push(snapshot); previousSeq = snapshot.seq }
        await delay(100)
      }
    })().catch(error => { sampleError = error })
    try {
      await waitFor(async () => (await page.getByRole('log', { name: '对话内容' }).innerText()).includes('已完成。'))
      await page.getByRole('button', { name: '停止生成', exact: true }).waitFor({ state: 'hidden', timeout: 60000 })
    } finally { sampling = false; await collector }
    if (sampleError) throw sampleError
    replyText = await page.getByRole('log', { name: '对话内容' }).innerText()
    events = []
  } else {
    identity = { version: '1.0', type: 'send', turnId: `turn-${kind}-${i}`, teamId: kind === 'multi' ? 'office-2' : 'office-1', conversationId: `${kind}-${i}`, messages: [{ role: 'user', content: prompt }] }
    const r = await fetch(base + '/office/chat', { signal: AbortSignal.timeout(60000), method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(identity) })
    assert.equal(r.status, 200); events = (await r.text()).trim().split('\n').filter(Boolean).map(JSON.parse)
    assert.ok(events.some(e => e.type === 'done'), 'Run must really finish')
  }
  return { i, kind, prompt, startedAt, replyText, conversation: identity.conversationId, turn: identity.turnId, team: identity.teamId, events: events.length, earlySnapshots }
}
async function checkSnapshot(row) {
  let last = '', previousChildren = 0
  const early = row.earlySnapshots; delete row.earlySnapshots
  const started = Date.now()
  const c = await waitFor(async () => {
    const s = early.shift() || await readOffice(row.team)
    const c = s?.conversations?.find(c => c.id === row.conversation)
    const main = c?.tasks?.find(t => row.parentId ? t.id === row.parentId : t.id?.startsWith('run:'))
    if (main && !row.parentId) row.parentId = main.id
    const children = c?.tasks?.filter(t => t.id?.startsWith('delegation:')) || []
    const signature = JSON.stringify({ status: main?.status, progress: main?.progress, children: children.length, whiteboard: c?.whiteboard?.status })
    if (signature !== last) {
      report.transitions.push({ i: row.i, kind: row.kind, elapsedMs: Date.now() - started, seq: s?.seq, ...JSON.parse(signature) })
      console.error('SNAP', row.kind, row.i, signature); last = signature
      if (row.kind === 'multi') {
        assert.ok(children.length >= previousChildren, '公开成员不能因重建消失')
        assert.ok(children.length - previousChildren <= 1, '逐人交接不能整组跳过')
        if (children.length < 5) assert.notEqual(main?.status, 'completed', '最后一位交接前不能报整体完成')
        previousChildren = children.length
      }
    }
    if (runtime && row.kind === 'multi' && row.i === 0 && children.length === 1 && !row.rebuiltDuringHandoff) {
      row.rebuiltDuringHandoff = true
      const followup = 'FOLLOWUP-交接期间直接回复已完成，不新增协作。'
      const box = page.getByRole('textbox', { name: '给办公室发送消息' })
      await box.fill(followup); await page.getByRole('button', { name: '发送消息', exact: true }).click()
      await waitFor(() => lastSend?.messages?.some(m => m.content === followup))
      await page.getByRole('button', { name: '停止生成', exact: true }).waitFor({ state: 'hidden' })
      await page.reload({ waitUntil: 'domcontentloaded' }); await chooseOffice(row.team)
      const expand = page.getByRole('button', { name: '展开对话', exact: true }); if (await expand.isVisible()) await expand.click()
      await box.waitFor(); await waitFor(() => box.isEnabled())
      row.followup = structuredClone(lastSend)
      // 重新取事实，避免把点击/刷新期间的旧帧当作后续结果。
      return false
    }
    assert.ok(!c?.tasks?.some(t => t.status === 'failed'), '成功fixture不得投影为失败')
    if (main?.status !== 'completed' || main.progress !== 100 || c?.whiteboard?.status !== 'completed') return false
    if (row.kind === 'multi') {
      assert.equal(children.length, 5); assert.deepEqual(new Set(children.map(t => t.assigneeId)), new Set(employeeIds.slice(1)))
      assert.equal(c.events.filter(e => e.kind === 'task.assigned').length, 5)
      assert.equal(c.events.filter(e => e.kind === 'task.completed').length, 5, '每位已核验成员必须有完成动态')
      assert.ok(c.whiteboard.description.includes('5/5'))
      if (row.followup) {
        const followupMain = c.tasks.find(t => t.id.startsWith('run:') && t.id !== row.parentId)
        assert.equal(followupMain?.status, 'completed'); assert.equal(followupMain.progress, 100)
      }
    }
    return c
  }, 90000)
  row.waitMs = Date.now() - started; row.final = c
  if (runtime && row.rebuiltDuringHandoff) {
    host.p.kill('SIGTERM'); await host.close; await host.save()
    host = start(hostBinary, ['engine-host', '--config', hostConfig, '--port', new URL(base).port], root, hostEnvironment, 'host-restarted')
    await waitFor(() => host.stdout().includes('openpaw engine host listening on'))
    await page.reload({ waitUntil: 'domcontentloaded' }); await chooseOffice(row.team)
    await waitFor(async () => {
      const restored = (await readOffice(row.team))?.conversations?.find(x => x.id === row.conversation)
      return restored?.tasks?.find(t => t.id === row.parentId)?.status === 'completed' && restored.events.filter(e => e.kind === 'task.completed').length === 5
    })
    const restored = (await readOffice(row.team)).conversations.find(x => x.id === row.conversation)
    assert.deepEqual(restored.events.filter(e => e.kind === 'task.assigned').map(e => e.id).sort(), c.events.filter(e => e.kind === 'task.assigned').map(e => e.id).sort())
    row.restartVerified = true
  }
  if (runtime) {
    // 核对浏览器收到的任务中心数据与真实快照，不能只凭后台HTTP判定UI通过。
    const collapse = page.getByRole('button', { name: '收起聊天', exact: true }); if (await collapse.isVisible()) await collapse.click()
    await page.getByRole('button', { name: /任务中心/ }).first().click()
    row.uiTasks = await waitFor(async () => {
      const visible = await page.locator('.runtime-task').evaluateAll(cards => cards.map(card => ({
        title: card.querySelector('strong')?.textContent,
        status: card.querySelector('.runtime-task-summary em')?.className,
        progress: card.querySelector('.runtime-task-progress')?.textContent,
      })))
      return visible.length === c.tasks.length && c.tasks.every(task => visible.some(card =>
        card.title === task.title && card.status === 'status-completed' && card.progress === '100%')) ? visible : false
    })
    await page.locator('.runtime-task-summary').first().click()
    await page.locator('.runtime-task-detail').first().waitFor()
    row.taskDetail = await page.locator('.runtime-task-detail').first().innerText()
    assert.ok(!/scope_denied|回复未完成|等待后端响应处理/.test(row.taskDetail))
    if (row.kind === 'multi') {
      for (const person of [0, 1, 2, 3, 4, 5]) {
      const name = names[person]
      const actor = page.locator('.runtime-person').filter({ hasText: name }).first()
      const start = Date.now(); await actor.click()
      const dialog = page.getByRole('dialog', { name: name + ' · 成员详情', exact: true }); await dialog.waitFor()
      const text = await dialog.innerText()
      assert.ok(!/scope_denied|未绑定|等待后端响应处理/.test(text)); assert.ok(text.includes('任务') && text.includes('完成'), text)
      assert.ok(text.includes('100%') && text.includes('已核验完成'), text)
      report.clicks.push({ i: row.i, name, entrance: 'sidebar', elapsedMs: Date.now() - start, text })
      await dialog.getByRole('button', { name: '关闭详情', exact: true }).click()
      }
      // 导出的是格点位置，渲染移动有插值。等待人物实际入座再点击，禁止改世界或伪造命中。
      await waitFor(async () => {
        const scene = await sceneSnapshot('scene-latest.json')
        return scene.world.actors.length === 6 && scene.world.actors.every(a => a.posture === 'seated')
      }, 90000)
      row.actors = []
      for (const actorId of employeeIds) {
        await disableIdleChatter(`multi-${row.i}-pixi-${actorId}`)
        // 真实Pixi位置可随闲聊移动；每次点击前导出当前世界，不复用旧坐标。
        const person = await waitFor(async () => {
          const scene = await sceneSnapshot('scene-latest.json')
          const actor = scene.world.actors.find(a => a.id === actorId); assert.ok(actor)
          return actor.posture === 'seated' && !actor.motion && !actor.step && !actor.seatTransition ? actor : false
        }, 90000)
        assert.equal(person.presentation.status, 'idle', '成功终态人物不得仍显示工作/失败')
        const [x, y] = await canvasPoint((person.position.x + .5) * 50 + (person.posture === 'seated' ? 25 : 0),
          (person.position.y + .5) * 50 - (person.posture === 'seated' ? 10 : 0) - 40)
        const pending = page.waitForResponse(r => r.url().endsWith('/api/pawoffice/ui') && r.request().postDataJSON()?.action === 'actor.inspect')
        const started = Date.now(); await page.mouse.click(x, y)
        const response = await pending; const body = await response.json()
        assert.equal(response.request().postDataJSON().actorId, actorId)
        assert.equal(body.data.target, 'dialog')
        assert.ok(body.data.content.fields.some(f => f.label === '已确认进度' && f.value === '100%'))
        assert.ok(body.data.content.fields.some(f => f.label === '业务状态' && f.value === '已核验完成'))
        const dialog = page.getByRole('dialog', { name: person.name + ' · 成员详情', exact: true }); await dialog.waitFor()
        const text = await dialog.innerText(); assert.ok(!/scope_denied|未绑定|等待后端响应处理/.test(text))
        const item = { i: row.i, name: person.name, entrance: 'pixi', elapsedMs: Date.now() - started, actor: person, text }
        report.clicks.push(item); row.actors.push(item)
        await dialog.getByRole('button', { name: '关闭详情', exact: true }).click()
      }
      const scene = await sceneSnapshot(`multi-${row.i}-scene.json`)
      const board = scene.world.props.find(p => p.templateId === 'office.wall-whiteboard'); assert.ok(board)
      assert.deepEqual(board.state.taskBoard, c.whiteboard, '实际场景白板须与当前会话事实一致')
      const [x, y] = await canvasPoint(board.position.x * 50 + 290, board.position.y * 50 - 70)
      await page.mouse.click(x, y)
      const reader = page.getByRole('dialog', { name: '协作白板', exact: true }); await reader.waitFor()
      row.whiteboardText = await reader.innerText()
      assert.ok(row.whiteboardText.includes(c.whiteboard.title) && row.whiteboardText.includes('5/5'))
      const values = await reader.getByRole('progressbar').evaluateAll(nodes => nodes.map(n => n.getAttribute('aria-valuenow')))
      assert.equal(values.length, c.whiteboard.steps.length); assert.ok(values.every(v => v === '100'))
      await page.screenshot({ path: join(out, `multi-${row.i}-whiteboard.png`), fullPage: true })
      await reader.getByRole('button', { name: '关闭白板', exact: true }).click()
    }
    if (row.i === 0 || row.i === (row.kind === 'multi' ? multiCount : singleCount) - 1) await page.screenshot({ path: join(out, `${row.kind}-${row.i}.png`), fullPage: true })
  }
}
try {
  if (runtime) {
    const frozen = JSON.parse(await readFile(join(root, 'docs/engine-host/contracts/sources.json'), 'utf8'))
    for (const item of frozen.office.files) {
      const bytes = await readFile(join(runtime, item.path)); assert.equal(createHash('sha256').update(bytes).digest('hex'), item.sha256)
    }
    report.officeCommit = frozen.office.commit
    const reserve = createServer(); officePort = await listen(reserve); await new Promise(r => reserve.close(r))
    officeBase = `http://127.0.0.1:${officePort}`
    officeProcess = start(process.execPath, ['server/dist/index.js'], runtime, { ...process.env, PORT: String(officePort), HOST: '127.0.0.1', PAWOFFICE_API_KEY: officeKey, PAWOFFICE_PUBLIC_ORIGIN: officeBase }, 'office')
    await waitFor(async () => { try { return (await fetch(officeBase)).ok } catch { return false } })
  } else {
    const actors = employeeIds.map((id, i) => ({ id, name: id, templateId: 'person', position: { x: i, y: 0 }, presentation: { status: 'idle', title: '', sourceRevision: 0 } }))
    const scene = { protocolVersion: '2.0', sceneId: 'scene-repeat', command: {}, batch: {}, actors, objects: [], capabilities: [] }
    const ui = { slots: [], actions: [], actionSchema: {}, presentationSchema: {}, whiteboardSchema: {} }
    const directory = () => ({ offices: [...snapshots].map(([id, s]) => ({ id, name: id, seq: s.seq || 0, runtimeReady: true, taskCount: 0 })), actors: actors.map(({ id, name }) => ({ id, name })), supportedMessages: ['offices.initialize', 'office.snapshot', 'office.read', 'office.capabilities'], sceneCommandProtocol: '2.0', ui })
    office = createServer(async (req, res) => { let body = ''; for await (const c of req) body += c; const x = JSON.parse(body || '{}')
      if (x.type === 'office.capabilities') return envelope(res, 200, { scene, ui, requests: {}, responses: { success: {}, error: {} } })
      if (x.type === 'office.read' && !x.teamId) return envelope(res, 200, directory())
      if (x.type === 'offices.initialize') { for (const s of x.offices || []) snapshots.set(s.workspace.id, s); return envelope(res, 200, directory()) }
      if (x.type === 'office.read') return envelope(res, 200, { snapshot: snapshots.get(x.teamId), scene, pendingDecisions: [] })
      if (x.type === 'office.snapshot') { snapshots.set(x.teamId, x.snapshot); return envelope(res, 200, { teamId: x.teamId, seq: x.snapshot.seq, status: 'applied' }) }
      envelope(res, 200, { actions: [] })
    })
    officePort = await listen(office); officeBase = `http://127.0.0.1:${officePort}`
  }
  provider = createServer(async (req, res) => {
    let body = ''; for await (const c of req) body += c
    const x = JSON.parse(body), prompt = [...x.messages].reverse().find(m => m.role === 'user')?.content || ''
    const role = ['lead', 'research', 'organize', 'draft', 'review', 'audit'].find(r => x.messages.some(m => m.role === 'system' && m.content.includes('（' + r + '）')))
    let content = '', tool_calls
    if (prompt.startsWith('MULTI-') && x.tools.some(t => t.function.name === 'delegate_task')) {
      const results = x.messages.filter(m => m.role === 'tool'), roles = ['research', 'organize', 'draft', 'review', 'audit']
      if (results.length < roles.length) { const r = roles[results.length]; tool_calls = [{ index: 0, id: `delegate-${r}-${prompt.slice(0,12)}`, type: 'function', function: { name: 'delegate_task', arguments: JSON.stringify({ role_id: r, title: `${r}完成资料核查`, instructions: '完成一个独立核查并返回结果', acceptance: '给出独立结果' }) } }] }
      else content = `${prompt} 五个角色均已完成，主角色已汇总。已完成。`
    } else if (role && role !== 'lead' && !x.messages.some(m => m.role === 'tool')) {
      tool_calls = [{ index: 0, id: `read-${role}`, type: 'function', function: { name: 'read_file', arguments: JSON.stringify({ path: 'README.md' }) } }]
    } else content = `${role || prompt} 独立任务已完成。`
    res.writeHead(200, { 'content-type': 'text/event-stream' })
    res.write('data: ' + JSON.stringify({ choices: [{ index: 0, delta: tool_calls ? { tool_calls } : { content }, finish_reason: null }] }) + '\n\n')
    res.write('data: ' + JSON.stringify({ choices: [{ index: 0, delta: {}, finish_reason: tool_calls ? 'tool_calls' : 'stop' }], usage: { prompt_tokens: 1, completion_tokens: 1 } }) + '\n\n'); res.end('data: [DONE]\n\n')
  })
  const providerPort = await listen(provider), config = join(out, 'host.toml')
  await writeFile(config, `home = "${home}"\nprovider = "openai"\nmodel = "repeat-fixture"\nbase_url = "http://127.0.0.1:${providerPort}/v1"\napi_key_env = "OPENAI_API_KEY"\ntools = ["read_file"]\n[roots]\nproject = "${workspace}"\nproject-2 = "${workspace2}"\n[slots]\noffice-1 = "project"\noffice-2 = "project-2"\n[office]\nbase_url = "${officeBase}"\napi_key_env = "PAWOFFICE_API_KEY"\nallowed_origins = ["${officeBase}"]\n[agents]\nenabled = true\n`)
  // 固定本轮可执行文件，避免并行工程检查重建target后让重启切换到另一版本。
  const binary = join(out, 'host-executable')
  await copyFile(process.env.OPENPAW_REPEAT_BINARY || join(root, 'target/debug/openpaw'), binary)
  report.binarySha256 = createHash('sha256').update(await readFile(binary)).digest('hex')
  hostBinary = binary; hostConfig = config; hostEnvironment = { ...process.env, OPENAI_API_KEY: 'repeat-provider', PAWOFFICE_API_KEY: officeKey }
  host = start(binary, ['engine-host', '--config', config, '--port', '0'], root, { ...process.env, OPENAI_API_KEY: 'repeat-provider', PAWOFFICE_API_KEY: officeKey }, 'host')
  base = await waitFor(() => host.stdout().match(/openpaw engine host listening on (http:\/\/127\.0\.0\.1:\d+)/)?.[1]); report.host = base; report.office = officeBase
  console.error('EVIDENCE', out, 'HOST', base, 'OFFICE', officeBase)
  if (runtime) {
    const { chromium } = await import(pathToFileURL(join(runtime, 'node_modules/playwright-core/index.mjs')))
    browser = await chromium.launch({ headless: true, args: ['--use-angle=swiftshader', '--enable-webgl', '--enable-unsafe-swiftshader'] })
    page = await browser.newPage({ viewport: { width: 1440, height: 1080 }, locale: 'zh-CN' }); page.setDefaultTimeout(20000)
    page.on('pageerror', e => report.pageErrors.push(e.message)); page.on('response', r => { if (r.status() >= 400) report.httpErrors.push({ url: r.url(), status: r.status() }) })
    page.on('request', r => { if (r.url().includes('/api/pawoffice/chat') && r.method() === 'POST') { try { const x = r.postDataJSON(); if (x.type === 'send') lastSend = x } catch {} } })
    await page.goto(officeBase + '/office/', { waitUntil: 'domcontentloaded' })
  }
  for (const [kind, count, team] of [['single', singleCount, 'office-1'], ['multi', multiCount, 'office-2']]) {
    if (runtime && count) { await chooseOffice(team); activeTeam = team }
    for (let i = 0; i < count; i++) {
      const roundTeam = kind === 'multi' && i >= 100 ? 'office-1' : team
      if (runtime && activeTeam !== roundTeam) { await chooseOffice(roundTeam); activeTeam = roundTeam }
      if (runtime) await disableIdleChatter(`${kind}-${i}-before-send`)
      try { const row = await send(kind === 'multi' ? `MULTI-${i} 请王明统筹五位同事，分别核查路线、美食、住宿、行程和资料。` : `SINGLE-${i} 请直接回复已完成。`, i, kind); await checkSnapshot(row); row.finishedAt = new Date().toISOString(); report.runs.push(row); console.error('PASS', kind, i + 1, '/', count) }
      catch (e) { report.failures.push({ kind, i, error: e.stack }); if (page) await page.screenshot({ path: join(out, `failure-${kind}-${i}.png`), fullPage: true }).catch(() => {}); throw e }
      await persist()
    }
  }
  const db = new DatabaseSync(join(home, 'engine-host/host.db'), { readOnly: true })
  try {
    for (const row of report.runs.filter(r => r.kind === 'multi')) {
      const parent = db.prepare('SELECT run FROM host_requests WHERE key=?').get(row.turn).run
      const children = db.prepare("SELECT reference FROM host_events WHERE kind='agent_delegation' AND json_extract(reference,'$.run_id')=? ORDER BY revision").all(parent).map(r => JSON.parse(r.reference))
      assert.equal(children.length, 5)
      for (const child of children) {
        const ledger = (await readFile(join(home, 'sessions', child.child_session + '.jsonl'), 'utf8')).trim().split('\n').map(JSON.parse)
        assert.ok(ledger.some(e => e.type === 'tool_call_complete' && e.tool_name === 'read_file'))
        assert.ok(ledger.some(e => e.type === 'run_finished' && e.status === 'completed'))
      }
      row.execution = { parent, children }
    }
  } finally { db.close() }
  assert.equal(report.pageErrors.length, 0); assert.equal(report.httpErrors.length, 0); report.finishedAt = new Date().toISOString(); report.status = 'passed'
} catch (e) { report.status = 'failed'; report.error = e.stack; process.exitCode = 1 }
finally {
  await persist(); if (browser) await browser.close()
  for (const item of processes.reverse()) { item.p.kill('SIGTERM'); await Promise.race([item.close, delay(5000)]); if (item.p.exitCode === null) { item.p.kill('SIGKILL'); await item.close } await item.save() }
  if (office) await new Promise(r => office.close(r)); if (provider) await new Promise(r => provider.close(r))
  if (hostBinary) await unlink(hostBinary)
  console.log(JSON.stringify({ out, status: report.status, runs: report.runs.length, failures: report.failures.length, error: report.error }))
}
