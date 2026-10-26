#!/usr/bin/env bash
# OpenPaw + PawOffice 本机编排入口。
# 用法：scripts/openpaw-office.sh build|start|stop|status [--force]
# 配置通过环境变量覆盖：
#   OPENPAW_ENV_FILE       默认 $HOME/.openpaw/.env
#   OPENPAW_HOST_CONFIG    默认 $HOME/.openpaw/engine-host-office.toml
#   OPENPAW_OFFICE_SOURCE  默认仓库旁的 ai-office-react
#   OPENPAW_RUN_DIR        默认 $XDG_RUNTIME_DIR/openpaw-office 或 /tmp/openpaw-office-$UID
#   PAWOFFICE_API_KEY      仅进程环境使用，不写入文件或日志

set -Eeuo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
USER_HOME="${HOME:?HOME is required}"
ENV_FILE="${OPENPAW_ENV_FILE:-$USER_HOME/.openpaw/.env}"
HOST_CONFIG="${OPENPAW_HOST_CONFIG:-$USER_HOME/.openpaw/engine-host-office.toml}"
OFFICE_SOURCE="${OPENPAW_OFFICE_SOURCE:-$(cd "$ROOT/.." && pwd)/ai-office-react}"
RUN_DIR="${OPENPAW_RUN_DIR:-${XDG_RUNTIME_DIR:-/tmp}/openpaw-office-${UID:-$(id -u)}}"
HOST_PORT="${OPENPAW_HOST_PORT:-32885}"
OFFICE_PORT="${OPENPAW_OFFICE_PORT:-8765}"
HOST_BIN="${OPENPAW_HOST_BINARY:-$ROOT/target/debug/openpaw}"
OFFICE_NODE="${OPENPAW_NODE:-node}"

HOST_PID_FILE="$RUN_DIR/host.pid"
OFFICE_PID_FILE="$RUN_DIR/office.pid"
HOST_LOG="$RUN_DIR/host.log"
OFFICE_LOG="$RUN_DIR/office.log"

die() { echo "openpaw-office: $*" >&2; exit 2; }
note() { echo "openpaw-office: $*"; }

ensure_run_dir() { mkdir -p "$RUN_DIR"; chmod 700 "$RUN_DIR"; }

read_pid() {
    local file="$1"
    [[ -s "$file" ]] || return 1
    local pid
    pid="$(cat "$file")"
    [[ "$pid" =~ ^[0-9]+$ ]] || return 1
    printf '%s\n' "$pid"
}

pid_alive() { kill -0 "$1" 2>/dev/null; }

pid_matches() {
    local pid="$1" expected="$2" cmdline
    pid_alive "$pid" || return 1
    [[ -r "/proc/$pid/cmdline" ]] || return 0
    cmdline="$(tr '\0' ' ' < "/proc/$pid/cmdline" 2>/dev/null || true)"
    [[ "$cmdline" == *"$expected"* ]]
}

port_busy() {
    (command -v ss >/dev/null 2>&1 && ss -ltn "sport = :$1" | tail -n +2 | grep -q .) \
        || (command -v lsof >/dev/null 2>&1 && lsof -nP -iTCP:"$1" -sTCP:LISTEN >/dev/null 2>&1)
}

health_ok() {
    curl --silent --show-error --fail --max-time 2 "http://127.0.0.1:$1/health" >/dev/null 2>&1
}

wait_health() {
    local port="$1" label="$2" i
    for i in $(seq 1 60); do
        health_ok "$port" && { note "$label healthy on 127.0.0.1:$port"; return 0; }
        sleep 0.5
    done
    return 1
}

provider_is_echo() {
    local configured env_value effective
    configured="$(awk -F= '/^[[:space:]]*provider[[:space:]]*=/{gsub(/[[:space:]]/,"",$2); gsub(/"/,"",$2); print tolower($2); exit}' "$HOST_CONFIG" 2>/dev/null || true)"
    env_value="$(awk -F= '/^[[:space:]]*OPENPAW_PROVIDER[[:space:]]*=/{gsub(/[[:space:]]/,"",$2); gsub(/"/,"",$2); print tolower($2); exit}' "$ENV_FILE" 2>/dev/null || true)"
    effective="$configured"
    [[ -n "$effective" ]] || effective="$env_value"
    [[ "$effective" == "echo" || -z "$effective" ]]
}

validate_inputs() {
    [[ -f "$HOST_CONFIG" ]] || die "Host配置不存在：$HOST_CONFIG（设置 OPENPAW_HOST_CONFIG）"
    [[ -f "$ENV_FILE" ]] || die "模型.env不存在：$ENV_FILE（设置 OPENPAW_ENV_FILE）"
    [[ -d "$OFFICE_SOURCE" ]] || die "Office源码目录不存在：$OFFICE_SOURCE（设置 OPENPAW_OFFICE_SOURCE）"
    [[ -n "${PAWOFFICE_API_KEY:-}" ]] || die "未设置 PAWOFFICE_API_KEY；密钥不能写入脚本或仓库"
    if provider_is_echo; then
        die "拒绝以 Echo 启动 Office；请从Host配置删除 provider=echo，并在.env配置真实Provider"
    fi
}

build() {
    ensure_run_dir
    command -v cargo >/dev/null 2>&1 || die "cargo 不在 PATH 中"
    command -v npm >/dev/null 2>&1 || die "npm 不在 PATH 中"
    note "building OpenPaw engine-host + sidecar"
    (cd "$ROOT" && cargo build --locked -p openpaw-cli --features engine-host,sidecar) \
        >"$RUN_DIR/build-openpaw.log" 2>&1 \
        || { tail -n 40 "$RUN_DIR/build-openpaw.log" >&2; die "OpenPaw编译失败"; }
    note "building PawOffice production bundle"
    (cd "$OFFICE_SOURCE" && npm run build:full) \
        >"$RUN_DIR/build-office.log" 2>&1 \
        || { tail -n 40 "$RUN_DIR/build-office.log" >&2; die "Office编译失败"; }
    note "build complete; logs are outside the repository: $RUN_DIR"
}

start_one() {
    local name="$1" pid_file="$2" expected="$3" log_file="$4"; shift 4
    if pid="$(read_pid "$pid_file" 2>/dev/null)" && pid_matches "$pid" "$expected"; then
        die "$name 已运行（PID $pid）"
    fi
    rm -f "$pid_file"
    # setsid/nohup + stdin断开，避免调用脚本的终端退出时把服务一起挂断。
    # 没有 setsid 的系统仍使用 nohup；Windows 入口由 .ps1 单独实现。
    if command -v setsid >/dev/null 2>&1; then
        setsid nohup "$@" >>"$log_file" 2>&1 </dev/null &
    else
        nohup "$@" >>"$log_file" 2>&1 </dev/null &
    fi
    pid=$!
    echo "$pid" >"$pid_file"
    note "$name started (PID $pid)"
}

start() {
    validate_inputs
    ensure_run_dir
    [[ -x "$HOST_BIN" ]] || die "Host二进制不存在，请先执行 build：$HOST_BIN"
    [[ -f "$OFFICE_SOURCE/server/dist/index.js" ]] || die "Office生产服务不存在，请先执行 build"
    port_busy "$OFFICE_PORT" && die "Office端口已被占用：$OFFICE_PORT"
    port_busy "$HOST_PORT" && die "Host端口已被占用：$HOST_PORT"

    start_one "Office" "$OFFICE_PID_FILE" "server/dist/index.js" "$OFFICE_LOG" \
        env PAWOFFICE_API_KEY="$PAWOFFICE_API_KEY" PAWOFFICE_STATIC_ROOT="$OFFICE_SOURCE/dist" \
        PORT="$OFFICE_PORT" "$OFFICE_NODE" "$OFFICE_SOURCE/server/dist/index.js"
    if ! wait_health "$OFFICE_PORT" "Office"; then
        stop_one "Office" "$OFFICE_PID_FILE" "$OFFICE_LOG" "server/dist/index.js" 10 0 || true
        die "Office健康检查失败；日志：$OFFICE_LOG"
    fi

    start_one "Host" "$HOST_PID_FILE" "engine-host" "$HOST_LOG" \
        env PAWOFFICE_API_KEY="$PAWOFFICE_API_KEY" \
        OPENPAW_HOST_DIAGNOSTICS="${OPENPAW_HOST_DIAGNOSTICS:-0}" \
        "$HOST_BIN" --env-file "$ENV_FILE" engine-host --config "$HOST_CONFIG" --port "$HOST_PORT"
    if ! wait_health "$HOST_PORT" "Host"; then
        stop_one "Host" "$HOST_PID_FILE" "$HOST_LOG" "engine-host" 10 0 || true
        stop_one "Office" "$OFFICE_PID_FILE" "$OFFICE_LOG" "server/dist/index.js" 10 0 || true
        die "Host健康检查失败；日志：$HOST_LOG"
    fi
    note "all services started; logs: $OFFICE_LOG and $HOST_LOG"
}

stop_one() {
    local name="$1" pid_file="$2" log_file="$3" expected="$4" timeout="$5" force="$6" pid i
    pid="$(read_pid "$pid_file" 2>/dev/null || true)"
    if [[ -z "$pid" ]]; then
        rm -f "$pid_file"
        note "$name already stopped"
        return 0
    fi
    if ! pid_alive "$pid"; then
        rm -f "$pid_file"
        note "$name already stopped (stale PID removed)"
        return 0
    fi
    if ! pid_matches "$pid" "$expected"; then
        rm -f "$pid_file"
        note "$name PID $pid 不再匹配受管理命令，已移除过期PID文件；未终止该进程"
        return 0
    fi
    kill -TERM "$pid" || true
    for i in $(seq 1 "$timeout"); do
        pid_alive "$pid" || { rm -f "$pid_file"; note "$name stopped gracefully"; return 0; }
        sleep 1
    done
    if [[ "$force" == 1 ]]; then
        kill -KILL "$pid" 2>/dev/null || true
        rm -f "$pid_file"
        note "$name force-stopped after ${timeout}s; inspect $log_file"
        return 0
    fi
    echo "openpaw-office: $name 未在 ${timeout}s 内完成优雅停止；未强制终止。可检查 $log_file，确认后使用 stop --force" >&2
    return 1
}

stop() {
    local force=0
    [[ "${1:-}" == "--force" ]] && force=1
    ensure_run_dir
    local failed=0
    stop_one "Host" "$HOST_PID_FILE" "$HOST_LOG" "engine-host" 60 "$force" || failed=1
    stop_one "Office" "$OFFICE_PID_FILE" "$OFFICE_LOG" "server/dist/index.js" 30 "$force" || failed=1
    (( failed == 0 )) || exit 1
}

status() {
    ensure_run_dir
    local name pid_file port pid state health
    for entry in "Host|$HOST_PID_FILE|$HOST_PORT" "Office|$OFFICE_PID_FILE|$OFFICE_PORT"; do
        IFS='|' read -r name pid_file port <<<"$entry"
        pid="$(read_pid "$pid_file" 2>/dev/null || true)"
        state="stopped"; health="down"
        expected="server/dist/index.js"
        [[ "$name" == "Host" ]] && expected="engine-host"
        if [[ -n "$pid" ]] && pid_matches "$pid" "$expected"; then state="running (PID $pid)"; fi
        health_ok "$port" && health="healthy"
        printf '%s: %s, port=%s, health=%s\n' "$name" "$state" "$port" "$health"
    done
}

usage() { echo "用法：$0 {build|start|stop [--force]|status}"; }
case "${1:-}" in
    build) shift; [[ $# -eq 0 ]] || die "build 不接受额外参数"; build ;;
    start) shift; [[ $# -eq 0 ]] || die "start 不接受额外参数"; start ;;
    stop) shift; stop "${1:-}" ;;
    status) shift; [[ $# -eq 0 ]] || die "status 不接受额外参数"; status ;;
    *) usage; exit 2 ;;
esac
