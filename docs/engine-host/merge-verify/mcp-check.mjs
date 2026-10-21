// Smoke check for the merged sidecar image's MCP-over-HTTP endpoint.
// This is the surface JYZS compose wires for AI Office
// (`OPENPAW_URL: http://openpaw:8788/mcp`).
const BASE = process.env.SIDECAR_BASE || 'http://openpaw-sidecar-image:8788';

let pass = 0;
const failures = [];
function check(name, cond, detail) {
  if (cond) { pass++; console.log(`  PASS  ${name}`); }
  else { failures.push(detail ? `${name} :: ${detail}` : name); console.log(`  FAIL  ${name}${detail ? ' :: ' + detail : ''}`); }
}

function parseBody(text) {
  let json = null;
  for (const line of text.split('\n')) {
    if (line.startsWith('data:')) {
      try { json = JSON.parse(line.slice(5).trim()); } catch { /* partial */ }
    }
  }
  if (!json) { try { json = JSON.parse(text); } catch { /* empty */ } }
  return json;
}

async function rpc(method, params, id, sessionId) {
  const headers = { 'content-type': 'application/json', accept: 'application/json, text/event-stream' };
  if (sessionId) headers['mcp-session-id'] = sessionId;
  const res = await fetch(BASE + '/mcp', {
    method: 'POST',
    headers,
    body: JSON.stringify(id === undefined
      ? { jsonrpc: '2.0', method, params }
      : { jsonrpc: '2.0', id, method, params }),
  });
  const text = await res.text();
  return { status: res.status, sid: res.headers.get('mcp-session-id'), json: parseBody(text), text };
}

(async () => {
  console.log(`# merged sidecar MCP check — ${BASE}/mcp`);

  const init = await rpc('initialize', {
    protocolVersion: '2025-06-18',
    capabilities: {},
    clientInfo: { name: 'merge-verify', version: '1.0.0' },
  }, 1);
  check('POST /mcp initialize -> 2xx with serverInfo',
    init.status >= 200 && init.status < 300 && !!init.json?.result?.serverInfo,
    `${init.status} ${init.text.slice(0, 160)}`);
  const sid = init.sid;

  const notified = await rpc('notifications/initialized', {}, undefined, sid);
  check('notifications/initialized accepted', notified.status >= 200 && notified.status < 300, `got ${notified.status}`);

  const tools = await rpc('tools/list', {}, 2, sid);
  const names = (tools.json?.result?.tools || []).map((t) => t.name);
  check('tools/list -> non-empty tool set',
    tools.status >= 200 && tools.status < 300 && names.length > 0, `${tools.status} n=${names.length}`);
  console.log('  tools:', names.join(', ') || '(none)');

  if (failures.length) {
    console.log(`\n########## SIDECAR RESULT: ${pass} passed, ${failures.length} failed ##########`);
    for (const f of failures) console.log('  - ' + f);
    process.exit(1);
  }
  console.log(`\n########## SIDECAR RESULT: ${pass} passed, 0 failed ##########`);
  process.exit(0);
})().catch((e) => { console.error('harness error:', e); process.exit(2); });
