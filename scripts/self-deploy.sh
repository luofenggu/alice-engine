#!/bin/bash
# Alice Engine 自我部署脚本
# 前置检查 → 优雅切换（不中断当前beat）

set -e

PROJECT_DIR="/data/alice-dev"
ENGINE_BIN="/data/rust-target-dev/release/alice-engine"
RUNTIME_DIR="/data/alice-dev-runtime"
ENGINE_PORT=9527

cd "$PROJECT_DIR"

echo "========================================="
echo "  Alice Engine Self-Deploy"
echo "========================================="

# === 前置检查 ===
echo ""
echo "[1/4] Guardian..."
GUARDIAN_OUTPUT=$(python3 defense/guardian/guardian.py engine/src 2>&1 || true)
if echo "$GUARDIAN_OUTPUT" | grep -q "0 violations"; then
    echo "  ✅ Guardian passed"
else
    echo "  ❌ Guardian FAILED:"
    echo "$GUARDIAN_OUTPUT" | tail -5
    exit 1
fi

echo "[2/4] Unit tests..."
TEST_OUTPUT=$(CARGO_TARGET_DIR=/data/rust-target-dev cargo test --release 2>&1 || true)
if echo "$TEST_OUTPUT" | grep -q "FAILED"; then
    echo "  ❌ Unit tests FAILED:"
    echo "$TEST_OUTPUT" | grep -E "(FAILED|failures)" | head -10
    exit 1
else
    PASSED=$(echo "$TEST_OUTPUT" | grep "test result: ok" | head -1)
    echo "  ✅ Unit tests passed ($PASSED)"
fi

echo "[3/4] E2E test: hello_world..."
E2E1_OUTPUT=$(bash integration/scripts/run_e2e.sh hello_world 2>&1 || true)
if echo "$E2E1_OUTPUT" | grep -q "E2E Test 'hello_world' PASSED"; then
    echo "  ✅ E2E hello_world passed"
else
    echo "  ❌ E2E hello_world FAILED"
    echo "$E2E1_OUTPUT" | tail -10
    exit 1
fi

echo "[4/4] E2E test: settings_knowledge..."
E2E2_OUTPUT=$(bash integration/scripts/run_e2e.sh settings_knowledge 2>&1 || true)
if echo "$E2E2_OUTPUT" | grep -q "E2E Test 'settings_knowledge' PASSED"; then
    echo "  ✅ E2E settings_knowledge passed"
else
    # Retry once (E2E tests can be flaky)
    echo "  ⚠️  Retrying settings_knowledge..."
    sleep 2
    E2E2_OUTPUT=$(bash integration/scripts/run_e2e.sh settings_knowledge 2>&1 || true)
    if echo "$E2E2_OUTPUT" | grep -q "E2E Test 'settings_knowledge' PASSED"; then
        echo "  ✅ E2E settings_knowledge passed (retry)"
    else
        echo "  ❌ E2E settings_knowledge FAILED (after retry)"
        echo "$E2E2_OUTPUT" | tail -10
        exit 1
    fi
fi

echo ""
echo "========================================="
echo "  All checks passed ✅ Deploying..."
echo "========================================="

# === 读取当前引擎的环境变量 ===
OLD_PID=$(ss -tlnp | grep ":${ENGINE_PORT} " | grep -oP 'pid=\K[0-9]+' || echo "")

if [ -z "$OLD_PID" ]; then
    echo "  ❌ No engine running on port $ENGINE_PORT, cannot inherit env"
    exit 1
fi

echo "Old engine PID: $OLD_PID"
echo "Inheriting environment variables from running engine..."

# 从运行中的引擎进程读取所有ALICE_*环境变量
ENV_FILE=$(mktemp)
cat /proc/$OLD_PID/environ | tr '\0' '\n' | grep '^ALICE_' > "$ENV_FILE"
echo "  Found $(wc -l < "$ENV_FILE") ALICE_* variables"

# === 部署：后台切换 ===
nohup bash -c '
    sleep 3
    # 加载环境变量
    set -a
    source "'"$ENV_FILE"'"
    set +a
    # Kill旧进程
    OLD_PID="'"$OLD_PID"'"
    if [ -n "$OLD_PID" ] && kill -0 "$OLD_PID" 2>/dev/null; then
        kill "$OLD_PID"
        echo "Killed old engine PID $OLD_PID"
        sleep 1
    fi
    # 启动新引擎（环境变量已从旧进程继承）
    '"$ENGINE_BIN"' &
    echo "New engine started with PID $!"
    # 清理临时文件
    rm -f "'"$ENV_FILE"'"
' > "$RUNTIME_DIR/logs/deploy.log" 2>&1 &

echo "Deploy scheduled. Engine will restart in ~3 seconds."
