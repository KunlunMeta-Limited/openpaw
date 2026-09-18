#!/usr/bin/env bash
# T3 curl acceptance for the OpenPaw Engine Host (run inside the rust container).
# Covers: ready line, required routes, degraded routes, v2 404, auth,
# mock/echo/stream experiments, JSONL and the 409 turn-admission rule.
set -u

BIN=/workspace/target/debug/openpaw
HOME_DIR=/tmp/t3home
LOG=/tmp/t3-host.log
BODY=/tmp/t3-body
HOST_PID=""

pass=0
fail=0

check() { # name expected actual
  if [ "$2" = "$3" ]; then
    echo "PASS  $1 = $2"
    pass=$((pass + 1))
  else
    echo "FAIL  $1 expected=$2 actual=$3"
    fail=$((fail + 1))
  fi
}

check_true() { # name 0-or-1 (0 = pass)
  if [ "$2" = "0" ]; then
    echo "PASS  $1"
    pass=$((pass + 1))
  else
    echo "FAIL  $1"
    fail=$((fail + 1))
  fi
}

ok() { if "$@" >/dev/null 2>&1; then echo 0; else echo 1; fi; }
code() { curl -s -o "$BODY" -w "%{http_code}" "$@"; }
body() { cat "$BODY"; }
json_id() { sed -n 's/.*"id":"\([^"]*\)".*/\1/p' | head -1; }
port_of() { sed -n 's/.*:\([0-9][0-9]*\)$/\1/p' "$LOG" | head -1; }

start_host() { # $1 = extra env assignments (may be empty), $2 = extra args
  rm -f "$LOG"
  if [ -n "$1" ]; then
    env $1 "$BIN" --engine-host --engine-host-hostname 127.0.0.1 --engine-host-port 0 $2 >"$LOG" 2>&1 &
  else
    "$BIN" --engine-host --engine-host-hostname 127.0.0.1 --engine-host-port 0 $2 >"$LOG" 2>&1 &
  fi
  HOST_PID=$!
  local i=0
  while [ $i -lt 100 ]; do
    if grep -q "listening on" "$LOG" 2>/dev/null; then return 0; fi
    sleep 0.1
    i=$((i + 1))
  done
  return 1
}

stop_host() {
  if [ -n "$HOST_PID" ]; then
    kill "$HOST_PID" 2>/dev/null
    wait "$HOST_PID" 2>/dev/null
    HOST_PID=""
  fi
}

new_session() { curl -s -X POST -H 'content-type: application/json' -d "${1:-{\}}" "$H/session" | json_id; }
messages() { curl -s "$H/session/$1/message"; }

wait_assistant() { # session, tenths-of-second budget
  local i=0
  while [ $i -lt "$2" ]; do
    if messages "$1" | grep -q '"role":"assistant"'; then return 0; fi
    sleep 0.1
    i=$((i + 1))
  done
  return 1
}

echo "### 0. build"
cd /workspace
/usr/local/cargo/bin/cargo build -q -p openpaw-cli --features engine-host 2>&1 | tail -3
export OPENPAW_HOME="$HOME_DIR"
rm -rf "$HOME_DIR"
mkdir -p "$HOME_DIR"

echo
echo "### 1. mock provider: ready line, required routes, degraded routes, v2, JSONL"
start_host "" "--provider mock"
check_true "host started" $?
READY=$(cat "$LOG")
echo "READY LINE: $READY"
check "stdout ready line count" 1 "$(wc -l <"$LOG" | tr -d ' ')"
check_true "ready line has the agreed prefix" "$(echo "$READY" | grep -q '^opencode server listening on http://127.0.0.1:' && echo 0 || echo 1)"
PORT=$(port_of)
H="http://127.0.0.1:$PORT"
echo "PORT: $PORT"

check "GET /global/health" 200 "$(code "$H/global/health")"
check "health body" '{"healthy":true}' "$(body)"
check "POST /session" 200 "$(code -X POST -H 'content-type: application/json' -d '{"title":"curl"}' "$H/session")"
SID=$(json_id <"$BODY")
echo "SESSION: $SID"
check_true "session id present" "$(echo "$SID" | grep -qE '^[0-9a-f]{16,}$' && echo 0 || echo 1)"
check "GET /session" 200 "$(code "$H/session")"
check "GET /session/:id" 200 "$(code "$H/session/$SID")"
check "GET /session/:id/message" 200 "$(code "$H/session/$SID/message")"
check "GET /session/:id/todo" 200 "$(code "$H/session/$SID/todo")"
check "todo body" '[]' "$(body)"
check "GET /session/status" 200 "$(code "$H/session/status")"
check "GET /provider" 200 "$(code "$H/provider")"
check "GET /permission" 200 "$(code "$H/permission")"
check "GET /event status" 200 "$(curl -s -o /dev/null -w '%{http_code}' -m 2 "$H/event")"
for SSE in /event /global/event /api/event; do
  check_true "GET $SSE is SSE" "$(curl -s -o /dev/null -w '%{content_type}' -m 2 "$H$SSE" | grep -q 'text/event-stream' && echo 0 || echo 1)"
done

check "POST /session/:id/prompt_async" 204 "$(code -X POST -H 'content-type: application/json' -d '{"parts":[{"type":"text","text":"hello-from-curl"}]}' "$H/session/$SID/prompt_async")"
check_true "prompt_async body is empty" "$([ -s "$BODY" ] && echo 1 || echo 0)"
wait_assistant "$SID" 100
check_true "mock assistant text present" $?
check_true "mock assistant echoes the prompt" "$(messages "$SID" | grep -q 'hello-from-curl' && echo 0 || echo 1)"
check "POST /session/:id/abort" 200 "$(code -X POST "$H/session/$SID/abort")"
check "abort body" 'true' "$(body)"

check "degraded GET /config" 200 "$(code "$H/config")"
check "config body" '{}' "$(body)"
check "degraded GET /mcp" 200 "$(code "$H/mcp")"
check "mcp body" '{}' "$(body)"
check "degraded GET /command" 200 "$(code "$H/command")"
check "command body" '[]' "$(body)"
check "degraded GET /project" 200 "$(code "$H/project")"
check "project body" '[]' "$(body)"
for ACTION in command shell fork revert summarize; do
  check "degraded POST /session/:id/$ACTION" 204 "$(code -X POST "$H/session/$SID/$ACTION")"
done

check "v2 GET /api/health" 404 "$(code "$H/api/health")"
check "v2 GET /api/session" 404 "$(code "$H/api/session")"

check_true "JSONL written to \$OPENPAW_HOME/sessions/<id>.jsonl" "$([ -f "$HOME_DIR/sessions/$SID.jsonl" ] && echo 0 || echo 1)"
check_true "JSONL carries meta + message records" "$(grep -q '"type"' "$HOME_DIR/sessions/$SID.jsonl" && echo 0 || echo 1)"

echo
echo "### 2. echo provider: create -> prompt -> message"
stop_host
start_host "" "--provider echo"
check_true "echo host started" $?
H="http://127.0.0.1:$(port_of)"
ESID=$(new_session '{}')
check_true "echo session created" "$(echo "$ESID" | grep -qE '^[0-9a-f]{16,}$' && echo 0 || echo 1)"
check "echo prompt_async" 204 "$(code -X POST -H 'content-type: application/json' -d '{"parts":[{"type":"text","text":"echo-probe"}]}' "$H/session/$ESID/prompt_async")"
wait_assistant "$ESID" 100
check_true "echo assistant text present" $?
check_true "echo assistant echoes the prompt" "$(messages "$ESID" | grep -q 'echo-probe' && echo 0 || echo 1)"
check "echo abort" 200 "$(code -X POST "$H/session/$ESID/abort")"

echo
echo "### 3. OPENPAW_ENGINE_MOCK=stream: abort clears busy and stops new text"
stop_host
start_host "OPENPAW_ENGINE_MOCK=stream" "--provider mock"
check_true "stream host started" $?
H="http://127.0.0.1:$(port_of)"
SSID=$(new_session '{}')
check "stream prompt_async" 204 "$(code -X POST -H 'content-type: application/json' -d '{"parts":[{"type":"text","text":"stream-probe"}]}' "$H/session/$SSID/prompt_async")"
sleep 0.15
check "stream abort" 200 "$(code -X POST "$H/session/$SSID/abort")"
BUSY=1
for _ in $(seq 1 100); do
  if ! curl -s "$H/session/status" | grep -q "$SSID"; then
    BUSY=0
    break
  fi
  sleep 0.1
done
check_true "abort leaves the session not busy" "$BUSY"
BEFORE=$(messages "$SSID" | wc -c | tr -d ' ')
sleep 1
AFTER=$(messages "$SSID" | wc -c | tr -d ' ')
check "no new text after abort" "$BEFORE" "$AFTER"
echo "stream transcript bytes after abort: $BEFORE"
stop_host

echo
echo "### 4. Basic Authentication"
env OPENCODE_SERVER_USERNAME=dev "$BIN" --engine-host --engine-host-hostname 127.0.0.1 --engine-host-port 0 >/tmp/t3-one.log 2>&1
RC=$?
check_true "username only fails startup (exit != 0)" "$([ "$RC" -ne 0 ] && echo 0 || echo 1)"
check_true "username only prints no ready line" "$(grep -q 'listening on' /tmp/t3-one.log && echo 1 || echo 0)"
echo "startup error: $(head -1 /tmp/t3-one.log)"
env OPENCODE_SERVER_PASSWORD=dev "$BIN" --engine-host --engine-host-hostname 127.0.0.1 --engine-host-port 0 >/tmp/t3-two.log 2>&1
RC=$?
check_true "password only fails startup (exit != 0)" "$([ "$RC" -ne 0 ] && echo 0 || echo 1)"

start_host "OPENCODE_SERVER_USERNAME=dev OPENCODE_SERVER_PASSWORD=dev" "--provider mock"
check_true "authenticated host started" $?
H="http://127.0.0.1:$(port_of)"
check "anonymous health" 401 "$(code "$H/global/health")"
check "anonymous degraded route" 401 "$(code "$H/config")"
check "wrong credentials" 401 "$(code -u dev:wrong "$H/global/health")"
check "authorized health" 200 "$(code -u dev:dev "$H/global/health")"
check "authorized create" 200 "$(code -u dev:dev -X POST -H 'content-type: application/json' -d '{}' "$H/session")"
check_true "WWW-Authenticate advertised" "$(curl -s -o /dev/null -D - "$H/global/health" | grep -qi 'www-authenticate: Basic realm=' && echo 0 || echo 1)"
stop_host

echo
echo "### 5. turn admission: same session 409, other sessions parallel (mock tools + manual approval)"
start_host "OPENPAW_ENGINE_MOCK=tools OPENPAW_ENGINE_PERMISSIONS=manual" "--provider mock"
check_true "manual host started" $?
H="http://127.0.0.1:$(port_of)"
A=$(new_session '{}')
B=$(new_session '{}')
check "first prompt on session A" 204 "$(code -X POST -H 'content-type: application/json' -d '{"parts":[{"type":"text","text":"tools"}]}' "$H/session/$A/prompt_async")"
sleep 0.5
check "second prompt on the busy session A" 409 "$(code -X POST -H 'content-type: application/json' -d '{"parts":[{"type":"text","text":"again"}]}' "$H/session/$A/prompt_async")"
check "prompt on session B while A is busy" 204 "$(code -X POST -H 'content-type: application/json' -d '{"parts":[{"type":"text","text":"other"}]}' "$H/session/$B/prompt_async")"
check "abort session A" 200 "$(code -X POST "$H/session/$A/abort")"
stop_host

echo
echo "SUMMARY: pass=$pass fail=$fail"
[ "$fail" -eq 0 ]
