#!/bin/bash
# Kill-During-Action E2E Test
#
# Verifies Write-Ahead Doing: when engine is killed while executing a shell action,
# the current.txt should retain the doing block (start marker + pending text)
# without an end marker, proving the agent can know it was interrupted.
#
# Usage: ./run_kill_test.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
INTEGRATION_DIR="$PROJECT_DIR/integration"

# Signal file with random suffix
SIGNAL_FILE="/tmp/alice-e2e-kill-signal-$$-$RANDOM"

# Ports
MOCK_LLM_PORT=$(python3 -c "import socket; s=socket.socket(); s.bind(('',0)); print(s.getsockname()[1]); s.close()")
ENGINE_PORT=$(python3 -c "import socket; s=socket.socket(); s.bind(('',0)); print(s.getsockname()[1]); s.close()")
AUTH_SECRET="test-secret-kill"

# Binary paths
CARGO_TARGET="${CARGO_TARGET_DIR:-target}"
ENGINE_BIN="$PROJECT_DIR/$CARGO_TARGET/release/alice-engine"
MOCK_LLM_BIN="$PROJECT_DIR/$CARGO_TARGET/release/mock-llm-server"

# Temp directory for test data
TMP_DIR=$(mktemp -d /tmp/alice-e2e-kill-XXXXXX)
INSTANCES_DIR="$TMP_DIR/instances"
LOGS_DIR="$TMP_DIR/logs"
mkdir -p "$INSTANCES_DIR" "$LOGS_DIR"

# Generate temp JSON script with actual signal file path
TEMP_SCRIPT="$TMP_DIR/kill_script.json"
sed "s|{SIGNAL_FILE}|$SIGNAL_FILE|g" "$INTEGRATION_DIR/scripts/kill_during_action.json" > "$TEMP_SCRIPT"

echo "[KILL-TEST] ============================================"
echo "[KILL-TEST] Write-Ahead Doing Kill Test"
echo "[KILL-TEST] ============================================"
echo "[KILL-TEST] Signal file: $SIGNAL_FILE"
echo "[KILL-TEST] Temp dir: $TMP_DIR"
echo "[KILL-TEST] Mock LLM port: $MOCK_LLM_PORT"
echo "[KILL-TEST] Engine port: $ENGINE_PORT"

# Cleanup function
cleanup() {
    echo "[KILL-TEST] Cleaning up..."
    [ -n "${MOCK_PID:-}" ] && kill "$MOCK_PID" 2>/dev/null || true
    [ -n "${ENGINE_PID:-}" ] && kill "$ENGINE_PID" 2>/dev/null || true
    rm -rf "$TMP_DIR"
    rm -f "$SIGNAL_FILE"
    echo "[KILL-TEST] Cleanup done."
}
trap cleanup EXIT

# === Step 0: Check binaries ===
echo "[KILL-TEST] Checking binaries..."
if [ ! -f "$ENGINE_BIN" ]; then
    # Try without project dir prefix (CARGO_TARGET_DIR might be absolute)
    ENGINE_BIN="$CARGO_TARGET/release/alice-engine"
    MOCK_LLM_BIN="$CARGO_TARGET/release/mock-llm-server"
fi
if [ ! -f "$ENGINE_BIN" ] || [ ! -f "$MOCK_LLM_BIN" ]; then
    echo "[KILL-TEST] ERROR: Binaries not found. Run 'cargo build --release' first."
    echo "[KILL-TEST]   Expected: $ENGINE_BIN"
    echo "[KILL-TEST]   Expected: $MOCK_LLM_BIN"
    exit 1
fi
echo "[KILL-TEST] Binaries found."

# === Step 1: Start Mock LLM ===
echo "[KILL-TEST] Starting Mock LLM server on port $MOCK_LLM_PORT..."
"$MOCK_LLM_BIN" "$TEMP_SCRIPT" "$MOCK_LLM_PORT" &
MOCK_PID=$!

for i in $(seq 1 10); do
    if curl -s "http://127.0.0.1:$MOCK_LLM_PORT/v1/chat/completions" -X POST -d '{}' >/dev/null 2>&1; then
        break
    fi
    sleep 0.5
done
echo "[KILL-TEST] Mock LLM ready (PID: $MOCK_PID)"

# === Step 2: Start Engine ===
echo "[KILL-TEST] Starting Alice Engine on port $ENGINE_PORT..."
ALICE_BASE_DIR="$TMP_DIR" \
ALICE_HTTP_PORT="$ENGINE_PORT" \
ALICE_HTML_DIR="$PROJECT_DIR/html-frontend" \
ALICE_INSTANCES_DIR="$INSTANCES_DIR" \
ALICE_LOGS_DIR="$LOGS_DIR" \
ALICE_AUTH_SECRET="$AUTH_SECRET" \
ALICE_USER_ID="test-user" \
ALICE_DEFAULT_MODEL="http://127.0.0.1:$MOCK_LLM_PORT/v1/chat/completions@test-model" \
ALICE_DEFAULT_API_KEY="test-api-key" \
"$ENGINE_BIN" &
ENGINE_PID=$!

for i in $(seq 1 20); do
    if curl -s "http://127.0.0.1:$ENGINE_PORT/login" >/dev/null 2>&1; then
        break
    fi
    sleep 0.5
done
echo "[KILL-TEST] Engine ready (PID: $ENGINE_PID)"

# === Step 3: Create instance ===
echo "[KILL-TEST] Creating instance..."
CREATE_RESPONSE=$(curl -s -X POST "http://127.0.0.1:$ENGINE_PORT/api/instances" \
    -H "Content-Type: application/json" \
    -d '{"name": "kill-test", "settings": {"privileged": true}}')
echo "[KILL-TEST] Create response: $CREATE_RESPONSE"

INSTANCE_ID=$(echo "$CREATE_RESPONSE" | python3 -c "import sys,json; d=json.load(sys.stdin); print(d.get('message',''))")
if [ -z "$INSTANCE_ID" ]; then
    echo "[KILL-TEST] FAIL: Could not extract instance ID from response"
    exit 1
fi
echo "[KILL-TEST] Instance created: $INSTANCE_ID"

# === Step 4: Send message to trigger inference ===
echo "[KILL-TEST] Sending message to trigger inference..."
MSG_RESPONSE=$(curl -s -X POST "http://127.0.0.1:$ENGINE_PORT/api/instances/$INSTANCE_ID/messages" \
    -H "Content-Type: application/json" \
    -d '{"content": "trigger"}')
echo "[KILL-TEST] Message response: $MSG_RESPONSE"

# === Step 5: Wait for signal file (script started executing) ===
echo "[KILL-TEST] Waiting for signal file (max 30s)..."
WAITED=0
while [ ! -f "$SIGNAL_FILE" ]; do
    sleep 0.5
    WAITED=$((WAITED + 1))
    if [ "$WAITED" -ge 60 ]; then
        echo "[KILL-TEST] FAIL: Signal file not created within 30 seconds"
        echo "[KILL-TEST] Engine logs:"
        cat "$LOGS_DIR"/*.log 2>/dev/null | tail -30 || echo "(no logs)"
        exit 1
    fi
done
echo "[KILL-TEST] Signal file detected! Script is executing."

# Give a moment for the doing block to be flushed to disk
sleep 0.5

# === Step 6: Kill engine with SIGKILL ===
echo "[KILL-TEST] Killing engine (PID: $ENGINE_PID) with SIGKILL..."
kill -9 "$ENGINE_PID" 2>/dev/null || true
wait "$ENGINE_PID" 2>/dev/null || true
ENGINE_PID=""  # Prevent cleanup from trying to kill again
echo "[KILL-TEST] Engine killed."

# === Step 7: Check current.txt ===
echo "[KILL-TEST] Checking current.txt..."
CURRENT_FILE="$INSTANCES_DIR/$INSTANCE_ID/memory/sessions/current.txt"

if [ ! -f "$CURRENT_FILE" ]; then
    echo "[KILL-TEST] FAIL: current.txt not found at $CURRENT_FILE"
    echo "[KILL-TEST] Instance dir contents:"
    find "$INSTANCES_DIR" -type f 2>/dev/null || echo "(empty)"
    exit 1
fi

echo "[KILL-TEST] current.txt contents:"
echo "---"
cat "$CURRENT_FILE"
echo "---"

# === Step 8: Assert ===
START_COUNT=$(grep -c "开始---------" "$CURRENT_FILE" || echo 0)
END_COUNT=$(grep -c "结束---------" "$CURRENT_FILE" || echo 0)
PENDING_COUNT=$(grep -c "action executing, result pending" "$CURRENT_FILE" || echo 0)

echo "[KILL-TEST] Start markers: $START_COUNT"
echo "[KILL-TEST] End markers: $END_COUNT"
echo "[KILL-TEST] Pending markers: $PENDING_COUNT"

if [ "$START_COUNT" -gt 0 ] && [ "$PENDING_COUNT" -gt 0 ] && [ "$START_COUNT" -gt "$END_COUNT" ]; then
    echo ""
    echo "========================================="
    echo "  ✅ PASS: Found incomplete doing block"
    echo "  (start=$START_COUNT, end=$END_COUNT, pending=$PENDING_COUNT)"
    echo "========================================="
    exit 0
else
    echo ""
    echo "========================================="
    echo "  ❌ FAIL: Expected incomplete doing block"
    echo "  (start=$START_COUNT, end=$END_COUNT, pending=$PENDING_COUNT)"
    echo "========================================="
    exit 1
fi