#!/bin/bash
# Alice Engine 自我部署脚本
# 前置检查 → 优雅切换（不中断当前beat）

set -e

PROJECT_DIR="/data/alice-dev"
ENGINE_BIN="/data/rust-target-dev/release/alice-engine"
RUNTIME_DIR="/data/alice-dev-runtime"
PID_FILE="$RUNTIME_DIR/engine.pid"

cd "$PROJECT_DIR"

echo "========================================="
echo "  Alice Engine Self-Deploy"
echo "========================================="

# === 前置检查 ===
echo ""
echo "[1/4] Guardian..."
GUARDIAN_OUTPUT=$(python3 defense/guardian/guardian.py engine/src 2>&1)
if echo "$GUARDIAN_OUTPUT" | grep -q "0 violations"; then
    echo "  ✅ Guardian passed"
else
    echo "  ❌ Guardian FAILED:"
    echo "$GUARDIAN_OUTPUT" | tail -5
    exit 1
fi

echo "[2/4] Unit tests..."
TEST_OUTPUT=$(CARGO_TARGET_DIR=/data/rust-target-dev cargo test --release 2>&1)
if echo "$TEST_OUTPUT" | grep -q "FAILED"; then
    echo "  ❌ Unit tests FAILED:"
    echo "$TEST_OUTPUT" | grep -E "(FAILED|failures)" | head -10
    exit 1
else
    PASSED=$(echo "$TEST_OUTPUT" | grep "test result: ok" | head -1)
    echo "  ✅ Unit tests passed ($PASSED)"
fi

echo "[3/4] E2E test: hello_world..."
E2E1_OUTPUT=$(bash integration/scripts/run_e2e.sh hello_world 2>&1)
if echo "$E2E1_OUTPUT" | grep -q "E2E Test 'hello_world' PASSED"; then
    echo "  ✅ E2E hello_world passed"
else
    echo "  ❌ E2E hello_world FAILED"
    echo "$E2E1_OUTPUT" | tail -10
    exit 1
fi

echo "[4/4] E2E test: settings_knowledge..."
E2E2_OUTPUT=$(bash integration/scripts/run_e2e.sh settings_knowledge 2>&1)
if echo "$E2E2_OUTPUT" | grep -q "E2E Test 'settings_knowledge' PASSED"; then
    echo "  ✅ E2E settings_knowledge passed"
else
    echo "  ❌ E2E settings_knowledge FAILED"
    echo "$E2E2_OUTPUT" | tail -10
    exit 1
fi

echo ""
echo "========================================="
echo "  All checks passed ✅ Deploying..."
echo "========================================="

# === 部署：后台切换 ===
OLD_PID=$(cat "$PID_FILE" 2>/dev/null || echo "")

# 启动新进程（后台）
nohup bash -c '
    sleep 3
    # Kill旧进程
    OLD_PID="'"$OLD_PID"'"
    if [ -n "$OLD_PID" ] && kill -0 "$OLD_PID" 2>/dev/null; then
        kill "$OLD_PID"
        sleep 1
    fi
    # 启动新引擎
    ALICE_HTTP_PORT=9527 \
    ALICE_HTML_DIR='"$PROJECT_DIR"'/html-frontend \
    ALICE_INSTANCES_DIR='"$RUNTIME_DIR"'/instances \
    ALICE_LOGS_DIR='"$RUNTIME_DIR"'/logs \
    ALICE_AUTH_SECRET=alice-dev-secret \
    ALICE_USER_ID=24007 \
    ALICE_DEFAULT_MODEL="openrouter@anthropic/claude-sonnet-4" \
    ALICE_DEFAULT_API_KEY=$(cat /data/alice-env/api_key.txt 2>/dev/null || echo "") \
    '"$ENGINE_BIN"' &
    echo $! > '"$PID_FILE"'
' > /dev/null 2>&1 &

echo "Deploy scheduled. Engine will restart in ~3 seconds."
echo "Old PID: ${OLD_PID:-none}"
