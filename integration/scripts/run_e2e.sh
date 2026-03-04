#!/bin/bash
# End-to-End Test Orchestration Script
#
# Starts three independent processes communicating via HTTP:
#   1. Mock LLM server (Rust binary)
#   2. Alice Engine (Rust binary)  
#   3. Playwright tests (Node.js)
#
# Usage: ./run_e2e.sh [test_name]
#   test_name: name of the test script (default: hello_world)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
INTEGRATION_DIR="$PROJECT_DIR/integration"
PLAYWRIGHT_DIR="$INTEGRATION_DIR/playwright"

TEST_NAME="${1:-hello_world}"
SCRIPT_FILE="$INTEGRATION_DIR/scripts/${TEST_NAME}.json"

# Ports
MOCK_LLM_PORT=19876
ENGINE_PORT=19877
AUTH_SECRET="test-secret-e2e"

# Binary paths
CARGO_TARGET="/data/rust-target-dev"
ENGINE_BIN="$CARGO_TARGET/release/alice-engine"
MOCK_LLM_BIN="$CARGO_TARGET/release/mock-llm-server"

# Temp directory for test data
TMP_DIR=$(mktemp -d /tmp/alice-e2e-XXXXXX)
INSTANCES_DIR="$TMP_DIR/instances"
LOGS_DIR="$TMP_DIR/logs"
mkdir -p "$INSTANCES_DIR" "$LOGS_DIR"

# Cleanup function
cleanup() {
    echo "[E2E] Cleaning up..."
    [ -n "${MOCK_PID:-}" ] && kill "$MOCK_PID" 2>/dev/null || true
    [ -n "${ENGINE_PID:-}" ] && kill "$ENGINE_PID" 2>/dev/null || true
    rm -rf "$TMP_DIR"
    echo "[E2E] Done."
}
trap cleanup EXIT

# === Step 0: Build ===
echo "[E2E] Building binaries..."
cd "$PROJECT_DIR"
CARGO_TARGET_DIR="$CARGO_TARGET" cargo build --release 2>&1 | tail -3

if [ ! -f "$ENGINE_BIN" ] || [ ! -f "$MOCK_LLM_BIN" ]; then
    echo "[E2E] ERROR: Binary not found after build"
    exit 1
fi

# === Step 1: Start Mock LLM ===
echo "[E2E] Starting Mock LLM server on port $MOCK_LLM_PORT..."
"$MOCK_LLM_BIN" "$SCRIPT_FILE" "$MOCK_LLM_PORT" &
MOCK_PID=$!

# Wait for mock LLM to be ready
for i in $(seq 1 10); do
    if curl -s "http://127.0.0.1:$MOCK_LLM_PORT/v1/chat/completions" -X POST -d '{}' >/dev/null 2>&1; then
        break
    fi
    sleep 0.5
done
echo "[E2E] Mock LLM ready (PID: $MOCK_PID)"

# === Step 2: Start Engine ===
echo "[E2E] Starting Alice Engine on port $ENGINE_PORT..."
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

# Wait for engine to be ready
for i in $(seq 1 20); do
    if curl -s "http://127.0.0.1:$ENGINE_PORT/login" >/dev/null 2>&1; then
        break
    fi
    sleep 0.5
done
echo "[E2E] Engine ready (PID: $ENGINE_PID)"

# === Step 3: Run Playwright Tests ===
echo "[E2E] Running Playwright test: $TEST_NAME..."
cd "$PLAYWRIGHT_DIR"
ENGINE_URL="http://127.0.0.1:$ENGINE_PORT" \
AUTH_SECRET="$AUTH_SECRET" \
INSTANCES_DIR="$INSTANCES_DIR" \
npx playwright test "${TEST_NAME}.spec.js" --reporter=list

echo ""
echo "========================================="
echo "  ✅ E2E Test '$TEST_NAME' PASSED"
echo "========================================="
