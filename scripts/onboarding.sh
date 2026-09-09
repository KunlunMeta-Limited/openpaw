#!/usr/bin/env bash
# OpenPaw onboarding — one-shot "can a fresh clone actually run?" gate
# (unix counterpart of scripts/onboarding.ps1).
#
# Run from the repository root (the script resolves the repo root relative
# to its own location):
#   bash scripts/onboarding.sh
#
# Steps (each reports PASS/FAIL):
#   1. cargo --version                toolchain present
#   2. cargo build -p openpaw-cli     timed debug build
#   3. target/debug/openpaw hello     one piped line -> "Echo: <line>"
#   4. openpaw doctor                 output audit (summary + network-audit report)
#   5. openpaw evolve status          evolution surface loads on a throwaway home
#
# Onboarding success-rate metric: one exit-0 run counts as one successful
# onboarding sample. In CI the rate is the share of matrix runners
# (windows-latest / ubuntu-latest / macos-latest) where this script passes.
#
# Every run is hermetic: OpenPaw state goes to a temp home, never ~/.openpaw.

set -u

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OP_HOME="${TMPDIR:-/tmp}/openpaw-onboarding-$$"
EXE="$ROOT/target/debug/openpaw"
BUILD_LOG="${TMPDIR:-/tmp}/openpaw-onboarding-build-$$.log"

pass=0
fail=0
start=$SECONDS

step() { # step name ok detail
    if [ "$2" -eq 1 ]; then
        echo "[$1] PASS ${3:-}"
        pass=$((pass + 1))
    else
        echo "[$1] FAIL ${3:-}"
        fail=$((fail + 1))
    fi
}

mkdir -p "$OP_HOME"

# 1. Toolchain check.
if cargo --version >/dev/null 2>&1; then
    step "1/5 cargo --version" 1 "$(cargo --version)"
else
    step "1/5 cargo --version" 0 "cargo not found"
fi

# 2. Timed debug build of the CLI (trailing errors on failure).
t0=$SECONDS
if cargo build -p openpaw-cli >"$BUILD_LOG" 2>&1; then
    build_ok=1
else
    build_ok=0
    tail -n 20 "$BUILD_LOG"
fi
t1=$SECONDS
step "2/5 cargo build -p openpaw-cli" "$build_ok" "$((t1 - t0))s"

# 3. hello — one piped line, echo reply, exit 0.
if [ "$build_ok" -eq 1 ] && [ -x "$EXE" ]; then
    hello_out="$(printf 'hello openpaw\nexit\n' | "$EXE" hello 2>&1)"
    if [ $? -eq 0 ] && printf '%s' "$hello_out" | grep -q 'Echo: hello openpaw'; then
        step "3/5 openpaw hello" 1 "echo reply received"
    else
        step "3/5 openpaw hello" 0 "$hello_out"
    fi
else
    step "3/5 openpaw hello" 0 "binary not built"
fi

# 4. doctor — exit 0 plus an auditable report: the summary line, at least one
#    [PASS] check, and the network-audit section. WARN never fails.
if [ "$build_ok" -eq 1 ] && [ -x "$EXE" ]; then
    doctor_out="$("$EXE" doctor --home "$OP_HOME" --workspace "$ROOT" 2>&1)"
    if [ $? -eq 0 ] \
        && printf '%s' "$doctor_out" | grep -q 'doctor summary:' \
        && printf '%s' "$doctor_out" | grep -q '\[PASS\]' \
        && printf '%s' "$doctor_out" | grep -q 'network call sites'; then
        step "4/5 openpaw doctor" 1 "summary + network-audit report audited"
    else
        step "4/5 openpaw doctor" 0 "$doctor_out"
    fi
else
    step "4/5 openpaw doctor" 0 "binary not built"
fi

# 5. evolve status — the knowledge/policy surface loads from an empty home.
if [ "$build_ok" -eq 1 ] && [ -x "$EXE" ]; then
    evolve_out="$("$EXE" evolve status --home "$OP_HOME" 2>&1)"
    if [ $? -eq 0 ] && printf '%s' "$evolve_out" | grep -q 'unverified knowledge artifacts:'; then
        step "5/5 openpaw evolve status" 1 "surface loads on empty home"
    else
        step "5/5 openpaw evolve status" 0 "$evolve_out"
    fi
else
    step "5/5 openpaw evolve status" 0 "binary not built"
fi

rm -rf "$OP_HOME" "$BUILD_LOG"

elapsed=$((SECONDS - start))
if [ "$fail" -eq 0 ]; then
    echo "onboarding: PASS (5 steps, ${elapsed}s)"
    exit 0
else
    echo "onboarding: FAIL ($pass/5 steps, ${elapsed}s)"
    exit 1
fi
