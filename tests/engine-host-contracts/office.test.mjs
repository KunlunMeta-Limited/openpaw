import test from 'node:test'
import assert from 'node:assert/strict'
import { readFile } from 'node:fs/promises'
import { createHash } from 'node:crypto'
import { officeRequestSchema, officeSnapshotSchema, officeErrorResultSchema, officeResultSchema, officeUiActionSchema } from './upstream-office/packages/protocol/src/office-integration.ts'
import { chatRequestSchema, chatEventSchema } from './upstream-office/packages/protocol/src/chat.ts'
import { assertProtocolValue, responseEnvelopeSchema } from './upstream-office/packages/protocol/src/schema.ts'
import { relayMessageSchema } from './upstream-office/packages/protocol/src/relay.ts'

const f = JSON.parse(await readFile(new URL('./fixtures/office.json', import.meta.url)))
const manifest = JSON.parse(await readFile(new URL('../../docs/engine-host/contracts/sources.json', import.meta.url)))

test('every cached upstream source matches its pinned commit hash', async () => {
  for (const item of manifest.office.files.filter(item => item.testSnapshot)) {
    const raw = await readFile(new URL('./upstream-office/' + item.path, import.meta.url))
    assert.equal(createHash('sha256').update(raw).digest('hex'), item.sha256, item.path)
  }
})

test('all seven public business request types pass real upstream schemas', () => {
  const requests = [
    { type: 'offices.initialize', offices: [f.snapshot] },
    { type: 'office.snapshot', teamId: 'office-1', snapshot: f.snapshot },
    { type: 'office.read', teamId: 'office-1' }, { type: 'office.read' },
    { type: 'office.capabilities' }, { type: 'ui.next', timeoutMs: 30000 },
    f.decision, f.dialog,
  ]
  for (const request of requests) {
    assertProtocolValue(request)
    officeRequestSchema.parse(request)
    assert.ok(Buffer.byteLength(JSON.stringify(request)) <= 65536)
  }
  officeUiActionSchema.parse(f.uiAction)
  relayMessageSchema.parse(f.sceneCommand)
})

test('snapshot default normalization is stable and full replacement clears omissions', () => {
  const once = officeSnapshotSchema.parse(f.snapshot)
  assert.deepEqual(officeSnapshotSchema.parse(once), once)
  assert.equal(once.system.cpuPercent, -1)
  assert.equal(once.whiteboard.title, '')
  const minimal = officeSnapshotSchema.parse({ schemaVersion: '1.0', seq: 2, workspace: f.snapshot.workspace })
  assert.deepEqual(minimal.tasks, [])
  assert.deepEqual(minimal.employees, [])
  assertProtocolValue(minimal)
})

test('upstream rejects invalid seq, mismatched team, dangling assignee and unknown fields', () => {
  for (const seq of [0, -1, 1.5, Number.MAX_SAFE_INTEGER + 1]) {
    assert.equal(officeSnapshotSchema.safeParse({ ...f.snapshot, seq }).success, false)
  }
  assert.equal(officeRequestSchema.safeParse({ type: 'office.snapshot', teamId: 'other', snapshot: f.snapshot }).success, false)
  assert.equal(officeSnapshotSchema.safeParse({ ...f.snapshot, tasks: [{ ...f.snapshot.tasks[0], assigneeId: 'missing' }] }).success, false)
  assert.equal(officeRequestSchema.safeParse({ type: 'office.read', teamId: 'office-1', invented: true }).success, false)
})

test('upstream enforces no-null, reserved keys and depth independently of DTO parsing', () => {
  assert.throws(() => assertProtocolValue({ metadata: { nested: null } }))
  assert.throws(() => assertProtocolValue(JSON.parse('{"__proto__":{}}')))
  let deep = {}
  for (let i = 0; i < 66; i++) deep = { child: deep }
  assert.throws(() => assertProtocolValue(deep))
})

test('chat requests/events and limits use real schemas; v1 cannot carry invented run IDs', () => {
  chatRequestSchema.parse(f.chatRequest)
  for (const event of [...f.chatEvents, f.chatError]) {
    assertProtocolValue(event)
    chatEventSchema.parse(event)
    assert.ok(JSON.stringify(event).length <= 32000)
  }
  assert.equal(chatRequestSchema.safeParse({ ...f.chatRequest, runId: 'invented' }).success, false)
  assert.equal(chatEventSchema.safeParse({ type: 'delta', text: 'x'.repeat(16001) }).success, false)
  // Zod counts code points; the browser's line and reply limits use JS UTF-16.
  assert.equal(chatEventSchema.safeParse({ type: 'delta', text: '😀'.repeat(8001) }).success, true)
  assert.equal(chatEventSchema.safeParse({ type: 'error', message: '' }).success, false)
  assert.equal(chatEventSchema.safeParse({ type: 'done', runId: 'invented' }).success, false)
})

test('response envelopes and result payloads retain applied/cached/error distinctions', () => {
  for (const response of Object.values(f.responses)) {
    assertProtocolValue(response)
    responseEnvelopeSchema.parse(response)
  }
  for (const key of ['applied', 'cached', 'ignored']) officeResultSchema('office.snapshot').parse(f.responses[key].data)
  officeResultSchema('ui.next').parse(f.responses.emptyUi.data)
  officeResultSchema('decision.request').parse(f.responses.decision.data)
  officeErrorResultSchema.parse(f.responses.conflict.data)
  officeErrorResultSchema.parse(f.responses.expired.data)
  assert.equal(f.responses.cached.code, 202)
  assert.equal(f.responses.cached.data.status, 'cached')
})

test('upstream bounds decision wait, UI wait, detail actions and scene team binding', () => {
  assert.equal(officeRequestSchema.safeParse({ ...f.decision, timeoutMs: 300001 }).success, false)
  assert.equal(officeRequestSchema.safeParse({ type: 'ui.next', timeoutMs: 30001 }).success, false)
  const tooMany = structuredClone(f.dialog)
  tooMany.presentation.content.actions = Array.from({ length: 9 }, (_, i) => ({ id: 'action-' + i, label: 'action' }))
  assert.equal(officeRequestSchema.safeParse(tooMany).success, false)
  assert.equal(relayMessageSchema.safeParse({ ...f.sceneCommand, teamId: 'other' }).success, false)
})
