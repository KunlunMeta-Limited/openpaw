import test from 'node:test'
import assert from 'node:assert/strict'
import { readFile } from 'node:fs/promises'
import { createServer } from 'node:http'
import { createOpencodeClient } from '@opencode-ai/sdk'
import * as f from './fixtures/opencode.ts'

async function checkSse(path: '/event' | '/global/event', frames: unknown[]) {
  const seen: string[] = []
  const server = createServer((request, response) => {
    seen.push(request.url ?? '')
    response.writeHead(200, { 'Content-Type': 'text/event-stream' })
    response.end(frames.map(frame => 'data: ' + JSON.stringify(frame) + '\n\n').join(''))
  })
  await new Promise<void>(resolve => server.listen(0, '127.0.0.1', resolve))
  const address = server.address()
  if (!address || typeof address === 'string') throw new Error('Missing fixture port')
  // SDK v1 SSE uses global fetch rather than config.fetch: use a real fixture
  // server, not a transport mock that never reaches the event parser.
  const client = createOpencodeClient({ baseUrl: 'http://127.0.0.1:' + address.port })
  try {
    const subscription = path === '/event' ? await client.event.subscribe() : await client.global.event()
    const actual = []
    for await (const event of subscription.stream) actual.push(event)
    assert.deepEqual(actual, frames)
    assert.deepEqual(seen, [path])
  } finally {
    await new Promise<void>((resolve, reject) => server.close(error => error ? reject(error) : resolve()))
  }
}

test('SDK version and installed integrity remain pinned', async () => {
  const manifest = JSON.parse(await readFile(new URL('../../docs/engine-host/contracts/sources.json', import.meta.url), 'utf8'))
  const lock = JSON.parse(await readFile(new URL('./package-lock.json', import.meta.url), 'utf8'))
  const sdk = lock.packages['node_modules/@opencode-ai/sdk']
  assert.equal(sdk.version, manifest.sdk.version)
  assert.equal(sdk.integrity, manifest.sdk.integrity)
})

test('real v1 SDK builds required HTTP methods, routes and permission body', async () => {
  const seen: { method: string; url: URL; body: unknown }[] = []
  const replies = [f.session, [f.session], f.session, f.messages, f.todos,
    { [f.session.id]: { type: 'busy' } }, undefined, true, f.providers, true]
  const client = createOpencodeClient({
    baseUrl: 'http://127.0.0.1:8766',
    fetch: async input => {
      const request = new Request(input)
      const raw = await request.text()
      seen.push({ method: request.method, url: new URL(request.url), body: raw ? JSON.parse(raw) : undefined })
      const response = replies.shift()
      return response === undefined ? new Response(null, { status: 204 })
        : Response.json(response)
    },
  })
  assert.deepEqual((await client.session.create({ body: { title: f.session.title } })).data, f.session)
  await client.session.list()
  await client.session.get({ path: { id: f.session.id } })
  assert.deepEqual((await client.session.messages({ path: { id: f.session.id } })).data, f.messages)
  await client.session.todo({ path: { id: f.session.id } })
  await client.session.status()
  await client.session.promptAsync(f.prompt)
  assert.equal((await client.session.abort({ path: { id: f.session.id } })).data, true)
  assert.deepEqual((await client.provider.list()).data, f.providers)
  assert.equal((await client.postSessionIdPermissionsPermissionId(f.reply)).data, true)
  assert.deepEqual(seen.map(r => [r.method, r.url.pathname]), [
    ['POST', '/session'], ['GET', '/session'], ['GET', '/session/ses_fixture'],
    ['GET', '/session/ses_fixture/message'], ['GET', '/session/ses_fixture/todo'],
    ['GET', '/session/status'], ['POST', '/session/ses_fixture/prompt_async'],
    ['POST', '/session/ses_fixture/abort'], ['GET', '/provider'],
    ['POST', '/session/ses_fixture/permissions/prm_fixture'],
  ])
  assert.deepEqual(seen.at(-1)?.body, { response: 'once' })
  assert.equal(replies.length, 0)
})

test('SDK encodes directory: GET query, POST header; fixture transport handles Basic', async () => {
  const seen: Request[] = []
  const directory = '/workspace/中文 项目'
  const auth = 'Basic ' + Buffer.from('fixture-user:fixture-password').toString('base64')
  const client = createOpencodeClient({
    baseUrl: 'http://127.0.0.1:8766', directory, headers: { Authorization: auth },
    fetch: async input => {
      const request = new Request(input); seen.push(request)
      return Response.json(f.session)
    },
  })
  await client.session.get({ path: { id: f.session.id } })
  await client.session.create({})
  assert.equal(new URL(seen[0].url).searchParams.get('directory'), directory)
  assert.equal(decodeURIComponent(seen[1].headers.get('x-opencode-directory') ?? ''), directory)
  for (const request of seen) assert.equal(request.headers.get('authorization'), auth)
})

test('real SDK parses workspace SSE with v1 permission/part events', async () => {
  await checkSse('/event', f.events)
})

test('global SSE has directory/payload wrapper distinct from workspace events', async () => {
  await checkSse('/global/event', [f.globalEvent])
})

test('unsupported writes are errors through SDK, never fake successful navigation', async () => {
  const client = createOpencodeClient({
    baseUrl: 'http://127.0.0.1:8766',
    fetch: async () => Response.json({ name: 'UnknownError', data: { message: 'Unsupported operation' } }, { status: 501 }),
  })
  await assert.rejects(client.session.fork({ path: { id: f.session.id }, throwOnError: true }))
})
