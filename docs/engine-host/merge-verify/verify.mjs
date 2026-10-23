// Verification client for the merged OpenPaw engine-host.
//
// Exercises the exact surfaces the two front-ends consume:
//   A. OpenWork  -> OpenCode v1 subset (docs/engine-host/master.md §3.2)
//   B. AI Office -> office action projection (§4.1 / §4.2), drained from the
//                   real ai-office action gateway.
//
// Usage: node verify.mjs <scenario>   scenario = tools | stream
const BASE = process.env.OPENPAW_BASE || 'http://openpaw-engine-host:8788';
const GW = process.env.OFFICE_GATEWAY || 'http://office-gateway:8765';
const USER = process.env.OPENCODE_SERVER_USERNAME || 'dev';
const PASS = process.env.OPENCODE_SERVER_PASSWORD || 'dev';
const SCENARIO = process.argv[2] || 'tools';

const AUTH = 'Basic ' + Buffer.from(`${USER}:${PASS}`).toString('base64');

let pass = 0;
const failures = [];
function check(name, cond, detail) {
  if (cond) {
    pass++;
    console.log(`  PASS  ${name}`);
  } else {
    failures.push(detail ? `${name} :: ${detail}` : name);
    console.log(`  FAIL  ${name}${detail ? ' :: ' + detail : ''}`);
  }
}
function section(t) { console.log(`\n=== ${t} ===`); }

async function req(method, path, body, opts = {}) {
  const headers = { ...(opts.headers || {}) };
  if (!opts.noAuth) headers.Authorization = AUTH;
  if (body !== undefined) headers['Content-Type'] = 'application/json';
  const res = await fetch(BASE + path, {
    method,
    headers,
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const text = await res.text();
  let json;
  try { json = JSON.parse(text); } catch { /* non-JSON (SSE / empty) */ }
  return { status: res.status, headers: res.headers, text, json };
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// ── A. OpenWork: OpenCode v1 surface (§3.2) ──────────────────────────────
async function openworkSuite() {
  section('A. OpenWork — OpenCode v1 surface (§3.2)');

  const health = await req('GET', '/global/health');
  check('GET /global/health -> 200 {"healthy":true}',
    health.status === 200 && health.json?.healthy === true, JSON.stringify(health.json));

  const noauth = await req('GET', '/global/health', undefined, { noAuth: true });
  check('missing Basic auth -> 401', noauth.status === 401, `got ${noauth.status}`);

  const bad = await req('GET', '/global/health', undefined, {
    headers: { Authorization: 'Basic ' + Buffer.from('dev:wrong').toString('base64') }, noAuth: true,
  });
  check('wrong Basic auth -> 401', bad.status === 401, `got ${bad.status}`);

  const created = await req('POST', '/session', { title: 'verify-' + SCENARIO });
  const sid = created.json?.id;
  // OpenWork 的 attached 代理验收断言 POST /session 返回 200（不是 201）。
  check('POST /session -> 200 session object',
    created.status === 200 && !!sid && !!created.json?.slug && !!created.json?.directory,
    `${created.status} ${created.text.slice(0, 160)}`);

  // openwork eval: 未知 permission 的回复必须 404（body 用 decision 字段）。
  const unknownPerm = await req('POST', '/permission/perm_missing/reply', { decision: 'reject' });
  check('POST /permission/perm_missing/reply -> 404',
    unknownPerm.status === 404, `got ${unknownPerm.status} ${unknownPerm.text.slice(0, 120)}`);

  const list = await req('GET', '/session');
  check('GET /session -> array containing the session',
    list.status === 200 && Array.isArray(list.json) && list.json.some((s) => s.id === sid));
  const summary = list.json?.find((s) => s.id === sid);
  check('session list -> OpenCode time/slug and compatible summary fields',
    summary?.slug === sid && Number.isFinite(summary?.time?.created)
      && summary.time.created === summary.createdAt && summary.time.updated === summary.updatedAt
      && summary.directory === created.json.directory && typeof summary.active === 'boolean');

  const one = await req('GET', `/session/${sid}`);
  check('GET /session/:id -> session object', one.status === 200 && one.json?.id === sid);

  const prov = await req('GET', '/provider');
  // openwork eval 对 /provider 的深层形状断言（第 104-144 行）：all/connected 为数组，
  // 每个 provider 有 id/name/models，且 models 的每个 key 等于该 model 的 id。
  const all = Array.isArray(prov.json?.all) ? prov.json.all : [];
  const conn = Array.isArray(prov.json?.connected) ? prov.json.connected : [];
  const byId = new Map(all.map((p) => [p.id, p]));
  const deepOk = ['mock', 'echo', 'anthropic', 'openai'].every((pid) => {
    const p = byId.get(pid);
    if (!p || typeof p.name !== 'string' || typeof p.models !== 'object' || p.models === null) return false;
    const entries = Object.entries(p.models);
    return entries.length > 0 && entries.every(([mid, m]) => m && m.id === mid && typeof m.name === 'string');
  });
  check('GET /provider -> all/connected + deep model shape (§3.2)',
    prov.status === 200 && deepOk && conn.includes('mock') && conn.includes('echo')
      && conn.every((cid) => byId.has(cid)),
    JSON.stringify({ ids: all.map((p) => p.id), connected: conn }));

  const emptyCreate = await req('POST', '/session', {});
  check('POST /session with {} body -> 200',
    emptyCreate.status === 200 && typeof emptyCreate.json?.id === 'string', `got ${emptyCreate.status}`);

  const todo = await req('GET', `/session/${sid}/todo`);
  check('GET /session/:id/todo -> []', todo.status === 200 && Array.isArray(todo.json));

  const status0 = await req('GET', '/session/status');
  check('GET /session/status -> object', status0.status === 200 && typeof status0.json === 'object');

  // SSE must be served on the three agreed paths only.
  for (const p of ['/event', '/global/event', '/api/event']) {
    const ctrl = new AbortController();
    const t = setTimeout(() => ctrl.abort(), 1200);
    try {
      const r = await fetch(BASE + p, { headers: { Authorization: AUTH }, signal: ctrl.signal });
      const ct = r.headers.get('content-type') || '';
      check(`GET ${p} -> SSE`, r.status === 200 && ct.includes('text/event-stream'), `${r.status} ${ct}`);
      ctrl.abort();
    } catch (e) {
      check(`GET ${p} -> SSE`, false, String(e));
    } finally { clearTimeout(t); }
  }
  const fourth = await req('GET', `/session/${sid}/event`);
  check('GET /session/:id/event -> 404 (not a 4th SSE path)', fourth.status === 404, `got ${fourth.status}`);

  // Degraded v1 capabilities.
  check('GET /config -> 200 {}', (await req('GET', '/config')).json && (await req('GET', '/config')).status === 200);
  check('GET /mcp -> 200 {}', (await req('GET', '/mcp')).status === 200);
  for (const p of ['/command', '/project']) {
    const r = await req('GET', p);
    check(`GET ${p} -> 200 []`, r.status === 200 && Array.isArray(r.json));
  }
  for (const p of ['command', 'shell', 'fork', 'revert', 'summarize']) {
    const r = await req('POST', `/session/${sid}/${p}`, {});
    if (p === 'fork' || p === 'revert') {
      check(`POST /session/:id/${p} -> current session no-op`, r.status === 200 && r.json.id === sid && !r.json.revert, `got ${r.status}`);
    } else {
      check(`POST /session/:id/${p} -> 204`, r.status === 204, `got ${r.status}`);
    }
  }
  for (const p of ['/api/health', '/session/execution']) {
    const r = await req('GET', p);
    check(`GET ${p} (v2) -> 404`, r.status === 404, `got ${r.status}`);
  }
  for (const p of ['/shell', '/fork', '/revert', '/summarize']) {
    const r = await req('POST', p, {});
    check(`POST ${p} -> 404 after deduplication`, r.status === 404, `got ${r.status}`);
  }
  check('POST /command -> 405 (GET only)', (await req('POST', '/command', {})).status === 405);

  // ── drive a turn while collecting SSE ──
  const events = [];
  const ctrl = new AbortController();
  const sse = (async () => {
    try {
      const r = await fetch(BASE + '/event', { headers: { Authorization: AUTH }, signal: ctrl.signal });
      const reader = r.body.getReader();
      const dec = new TextDecoder();
      let buf = '';
      for (;;) {
        const { done, value } = await reader.read();
        if (done) break;
        buf += dec.decode(value, { stream: true });
        const lines = buf.split('\n');
        buf = lines.pop();
        for (const line of lines) {
          if (!line.startsWith('data:')) continue;
          try { events.push(JSON.parse(line.slice(5).trim())); } catch { /* partial */ }
        }
      }
    } catch { /* aborted */ }
  })();

  await sleep(300);
  const prompt = await req('POST', `/session/${sid}/prompt_async`, {
    model: { providerID: 'mock', modelID: 'mock-1' },
    parts: [{ type: 'text', text: 'please read the readme' }],
  });
  check('POST /session/:id/prompt_async -> 204', prompt.status === 204, `got ${prompt.status}`);

  // Reply to any permission request (manual approval path).
  let replied = 0;
  const replyLoop = (async () => {
    for (let i = 0; i < 60; i++) {
      await sleep(150);
      const perms = await req('GET', '/permission');
      if (Array.isArray(perms.json) && perms.json.length) {
        for (const p of perms.json) {
          const r = await req('POST', `/permission/${p.id}/reply`, { reply: 'once' });
          if (r.status === 204) replied++;
        }
      }
      if (events.some((e) => e.type === 'session.idle')) return;
    }
  })();
  await replyLoop;
  await sleep(500);
  ctrl.abort();
  await sse;

  const types = events.map((e) => e.type);
  const has = (t) => types.includes(t);
  check('SSE: session.status emitted', has('session.status'), types.join(','));
  check('SSE: message.updated emitted', has('message.updated'));
  check('SSE: message.part.updated emitted', has('message.part.updated'));
  check('SSE: message.part.delta emitted', has('message.part.delta'));
  check('SSE: session.idle emitted', has('session.idle'));
  check('SSE: every frame is {type, properties}',
    events.length > 0 && events.every((e) => typeof e.type === 'string' && e.properties && typeof e.properties === 'object'),
    JSON.stringify(events[0] || {}));

  const msgs = await req('GET', `/session/${sid}/message`);
  const flat = JSON.stringify(msgs.json || []);
  const expectedText = SCENARIO === 'tools' ? '读取完成' : 'streamed response';
  check('GET /session/:id/message -> assistant text present',
    msgs.status === 200 && Array.isArray(msgs.json) && msgs.json.length >= 2 && flat.includes(expectedText),
    `n=${msgs.json?.length} expected=${expectedText}`);
  if (SCENARIO === 'tools') {
    check('assistant tool part reached terminal state',
      flat.includes('"tool"') && (flat.includes('completed') || flat.includes('error')));
  } else {
    const deltas = events.filter((e) => e.type === 'message.part.delta');
    check('streaming produced multiple message.part.delta frames', deltas.length >= 2, `deltas=${deltas.length}`);
  }

  const statusIdle = await req('GET', '/session/status');
  check('GET /session/status -> session no longer busy',
    !statusIdle.json?.[sid] || statusIdle.json[sid].type !== 'busy', JSON.stringify(statusIdle.json));

  const permsAfter = await req('GET', '/permission');
  check('GET /permission -> drained after reply',
    Array.isArray(permsAfter.json) && permsAfter.json.length === 0, JSON.stringify(permsAfter.json));

  if (SCENARIO === 'tools') {
    check('manual approval path produced a permission request', replied > 0, `replied=${replied}`);
    check('SSE: permission.asked emitted', has('permission.asked'));
    check('SSE: permission.replied emitted', has('permission.replied'));
  }
  return sid;
}

// ── B. AI Office: office action projection (§4.1 / §4.2) ─────────────────
async function officeSuite(sid) {
  section('B. AI Office — office action projection (§4.1 / §4.2)');

  const drain = await fetch(GW + '/actions');
  const drained = await drain.json().catch(() => ({}));
  const actions = drained.actions || [];
  check('GET /actions on the ai-office gateway -> {"actions":[...]}',
    drain.status === 200 && Array.isArray(drained.actions), JSON.stringify(drained).slice(0, 200));

  const visits = actions.filter((a) => a.type === 'desk_visit');
  const states = actions.filter((a) => a.type === 'set_state');

  check('office: at most one desk_visit per turn (§4.2)', visits.length <= 1, `visits=${visits.length}`);
  if (visits.length) {
    check('office: desk_visit visitor is roster 1 (王明)', visits[0].visitor === 1, JSON.stringify(visits[0]));
    check('office: desk_visit host is 3 for read_file (周理)', visits[0].host === 3, JSON.stringify(visits[0]));
  }
  check('office: set_state actions emitted', states.length > 0, `n=${states.length}`);
  check('office: set_state rosterNo 1 -> thinking on turn start',
    states.some((s) => s.rosterNo === 1 && s.state === 'thinking'), JSON.stringify(states));
  const allowed = ['idle', 'walking', 'working', 'talking', 'thinking'];
  check('office: every state is in the allowed vocabulary',
    states.every((s) => allowed.includes(s.state)), JSON.stringify(states.map((s) => s.state)));
  check('office: every action carries a type field',
    actions.every((a) => typeof a.type === 'string'), JSON.stringify(actions.slice(0, 3)));

  console.log('  --- drained actions ---');
  for (const a of actions) console.log('   ', JSON.stringify(a));
  console.log(`  --- office session scope: ${sid} ---`);
}

(async () => {
  console.log(`# OpenPaw merged engine-host verification — scenario="${SCENARIO}"`);
  console.log(`# engine-host=${BASE}  office-gateway=${GW}`);
  const sid = await openworkSuite();
  await officeSuite(sid);
  console.log(`\n########## RESULT: ${pass} passed, ${failures.length} failed ##########`);
  if (failures.length) {
    console.log('FAILURES:');
    for (const f of failures) console.log('  - ' + f);
    process.exit(1);
  }
  process.exit(0);
})().catch((e) => { console.error('harness error:', e); process.exit(2); });
