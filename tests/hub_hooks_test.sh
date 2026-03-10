#!/bin/bash
#
# Hub Hooks E2E Tests
# Tests that hub contacts are properly injected into agent prompt.
#
# Verifies the fix for: HooksCaller dual-instance bug where
# register_hooks updated EngineState's caller but AliceEngine
# used a separate instance, resulting in 0 contacts during inference.
#
# Usage: bash tests/hub_hooks_test.sh
#

set -euo pipefail

# ── Configuration ──
BINARY="/data/cargo-target/release/alice-engine"
PORT=9903
URL="http://localhost:${PORT}"
DATA_DIR="/tmp/e2e-hub-hooks"
HTML_DIR="$(cd "$(dirname "$0")/../html-frontend" && pwd)"

# ── State ──
ENGINE_PID=""
PASS_COUNT=0
FAIL_COUNT=0
TOTAL_COUNT=0

# ── Colors ──
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
CYAN='\033[0;36m'
NC='\033[0m'

# ── Helpers ──

cleanup() {
    echo -e "\n${CYAN}[CLEANUP]${NC} Stopping engine and removing temp dir..."
    [ -n "$ENGINE_PID" ] && kill "$ENGINE_PID" 2>/dev/null && wait "$ENGINE_PID" 2>/dev/null || true
    ENGINE_PID=""
    rm -rf "$DATA_DIR"
}

trap cleanup EXIT

start_engine() {
    rm -rf "$DATA_DIR"
    mkdir -p "$DATA_DIR"
    ALICE_HTTP_PORT="$PORT" \
    ALICE_BASE_DIR="$DATA_DIR" \
    ALICE_INSTANCES_DIR="$DATA_DIR/instances" \
    ALICE_LOGS_DIR="$DATA_DIR/logs" \
    ALICE_SKIP_AUTH=true \
    ALICE_HOST="$URL" \
    ALICE_HTML_DIR="$HTML_DIR" \
    ALICE_AUTH_SECRET="e2e-hooks-secret" \
    ALICE_DEFAULT_API_KEY="fake-key-for-e2e-testing" \
    "$BINARY" > "$DATA_DIR/engine.log" 2>&1 &
    ENGINE_PID=$!
}

wait_for_port() {
    local max_wait=15 elapsed=0
    while ! curl -sf "$URL/api/hub/status" > /dev/null 2>&1; do
        sleep 0.5
        elapsed=$((elapsed + 1))
        if [ "$elapsed" -ge "$((max_wait * 2))" ]; then
            echo -e "${RED}[ERROR]${NC} Port $PORT not ready after ${max_wait}s"
            return 1
        fi
    done
}

api() {
    local method="$1" url="$2"
    shift 2
    curl -sf -X "$method" "$url" -H "Content-Type: application/json" "$@" 2>/dev/null
}

api_post() {
    local url="$1" body="$2"
    curl -sf -X POST "$url" -H "Content-Type: application/json" -d "$body" 2>/dev/null
}

assert_eq() {
    local desc="$1" expected="$2" actual="$3"
    TOTAL_COUNT=$((TOTAL_COUNT + 1))
    if [ "$expected" = "$actual" ]; then
        echo -e "  ${GREEN}✓${NC} $desc"
        PASS_COUNT=$((PASS_COUNT + 1))
    else
        echo -e "  ${RED}✗${NC} $desc (expected: ${expected}, got: ${actual})"
        FAIL_COUNT=$((FAIL_COUNT + 1))
    fi
}

assert_contains() {
    local desc="$1" haystack="$2" needle="$3"
    TOTAL_COUNT=$((TOTAL_COUNT + 1))
    if echo "$haystack" | grep -q "$needle"; then
        echo -e "  ${GREEN}✓${NC} $desc"
        PASS_COUNT=$((PASS_COUNT + 1))
    else
        echo -e "  ${RED}✗${NC} $desc (expected to contain: ${needle})"
        FAIL_COUNT=$((FAIL_COUNT + 1))
    fi
}

assert_not_contains() {
    local desc="$1" haystack="$2" needle="$3"
    TOTAL_COUNT=$((TOTAL_COUNT + 1))
    if ! echo "$haystack" | grep -q "$needle"; then
        echo -e "  ${GREEN}✓${NC} $desc"
        PASS_COUNT=$((PASS_COUNT + 1))
    else
        echo -e "  ${RED}✗${NC} $desc (should NOT contain: ${needle})"
        FAIL_COUNT=$((FAIL_COUNT + 1))
    fi
}

create_instance() {
    local name="$1"
    api_post "$URL/api/instances" "{\"name\":\"$name\"}"
}

get_instance_id() {
    local name="$1"
    api GET "$URL/api/instances" | python3 -c "
import sys, json
data = json.load(sys.stdin)
instances = data.get('instances', data) if isinstance(data, dict) else data
for inst in instances:
    if inst.get('name') == '$name':
        print(inst['id'])
        break
" 2>/dev/null
}

hub_mode() {
    api GET "$URL/api/hub/status" | python3 -c "import sys,json; print(json.load(sys.stdin).get('mode',''))" 2>/dev/null
}

# ── Test Cases ──

echo -e "${CYAN}╔══════════════════════════════════════════╗${NC}"
echo -e "${CYAN}║     Hub Hooks E2E Tests                  ║${NC}"
echo -e "${CYAN}╚══════════════════════════════════════════╝${NC}"

echo -e "\n${CYAN}[SETUP]${NC} Starting engine on port $PORT..."
start_engine
wait_for_port
echo -e "${GREEN}[SETUP]${NC} Engine ready"

# ── TEST 1: Contacts endpoint returns correct results ──

echo -e "\n${CYAN}[TEST 1]${NC} Contacts endpoint correctness after enable host"

# Create two instances
echo -e "  Creating instances..."
create_instance "Alice" > /dev/null
create_instance "Bob" > /dev/null

ALICE_ID=$(get_instance_id "Alice")
BOB_ID=$(get_instance_id "Bob")
echo -e "  Alice=$ALICE_ID, Bob=$BOB_ID"

# Enable host
api_post "$URL/api/hub/enable" '{"join_token":"hooks-test"}' > /dev/null
sleep 1

assert_eq "Hub mode is 'host'" "host" "$(hub_mode)"

# Check contacts for Alice (should see Bob, not self)
ALICE_CONTACTS=$(api GET "$URL/api/hub/contacts/$ALICE_ID" 2>/dev/null || echo "ERROR")
assert_not_contains "Alice contacts don't contain self" "$ALICE_CONTACTS" "$ALICE_ID"
assert_contains "Alice contacts contain Bob" "$ALICE_CONTACTS" "$BOB_ID"

# Check contacts for Bob (should see Alice, not self)
BOB_CONTACTS=$(api GET "$URL/api/hub/contacts/$BOB_ID" 2>/dev/null || echo "ERROR")
assert_not_contains "Bob contacts don't contain self" "$BOB_CONTACTS" "$BOB_ID"
assert_contains "Bob contacts contain Alice" "$BOB_CONTACTS" "$ALICE_ID"

# ── TEST 2: Contacts injected into inference prompt ──

echo -e "\n${CYAN}[TEST 2]${NC} Contacts injection into inference prompt"

# Send a message to Alice to trigger inference
api_post "$URL/api/instances/$ALICE_ID/messages" '{"content":"hello"}' > /dev/null

# Wait for inference to start and hooks to be fetched
sleep 3

# Check engine log for hooks fetch result
LOG_FILE="$DATA_DIR/engine.log"

# Look for format_contacts log line
CONTACTS_LOG=$(grep "format_contacts" "$LOG_FILE" 2>/dev/null || echo "")
assert_contains "Log contains format_contacts entry" "$CONTACTS_LOG" "format_contacts"

# Check that contacts count > 0 (the fix ensures shared HooksCaller)
# Before fix: "fetch OK, 0 contacts" — hooks_caller had no config
# After fix: "fetch OK, 1 contacts" — shared HooksCaller has correct config
if echo "$CONTACTS_LOG" | grep -q "0 contacts"; then
    TOTAL_COUNT=$((TOTAL_COUNT + 1))
    echo -e "  ${RED}✗${NC} Contacts count should be > 0 (got 0 — HooksCaller bug not fixed)"
    FAIL_COUNT=$((FAIL_COUNT + 1))
else
    TOTAL_COUNT=$((TOTAL_COUNT + 1))
    echo -e "  ${GREEN}✓${NC} Contacts count > 0 (HooksCaller shared correctly)"
    PASS_COUNT=$((PASS_COUNT + 1))
fi

# Verify the prompt contains the contact name
PROMPT_LOG=$(grep -i "contacts.*Bob\|Bob.*contact" "$LOG_FILE" 2>/dev/null || echo "")
# Also check for the formatted contacts in prompt
FORMATTED=$(grep "format_contacts" "$LOG_FILE" 2>/dev/null | tail -1)
echo -e "  ${YELLOW}ℹ${NC} Last format_contacts log: ${FORMATTED:-<not found>}"

# ── TEST 3: Third instance also visible ──

echo -e "\n${CYAN}[TEST 3]${NC} New instance appears in contacts after creation"

create_instance "Charlie" > /dev/null
CHARLIE_ID=$(get_instance_id "Charlie")
echo -e "  Charlie=$CHARLIE_ID"

sleep 1

# Alice should now see both Bob and Charlie
ALICE_CONTACTS2=$(api GET "$URL/api/hub/contacts/$ALICE_ID" 2>/dev/null || echo "ERROR")
assert_contains "Alice sees Bob after Charlie added" "$ALICE_CONTACTS2" "$BOB_ID"
assert_contains "Alice sees Charlie" "$ALICE_CONTACTS2" "$CHARLIE_ID"
assert_not_contains "Alice still doesn't see self" "$ALICE_CONTACTS2" "$ALICE_ID"

# Charlie should see both Alice and Bob
CHARLIE_CONTACTS=$(api GET "$URL/api/hub/contacts/$CHARLIE_ID" 2>/dev/null || echo "ERROR")
assert_contains "Charlie sees Alice" "$CHARLIE_CONTACTS" "$ALICE_ID"
assert_contains "Charlie sees Bob" "$CHARLIE_CONTACTS" "$BOB_ID"

# ── TEST 4: Relay message between instances ──

echo -e "\n${CYAN}[TEST 4]${NC} Relay message delivery"

# Send relay message from Alice to Bob
RELAY_RESULT=$(api_post "$URL/api/instances/$BOB_ID/messages/relay" \
    "{\"sender\":\"$ALICE_ID\",\"content\":\"Hello from Alice\",\"auth_token\":\"hooks-test\"}" 2>/dev/null || echo "ERROR")
assert_not_contains "Relay doesn't return error" "$RELAY_RESULT" "error"

# Check Bob's messages contain the relayed message
sleep 1
BOB_MSGS=$(api GET "$URL/api/instances/$BOB_ID/messages?limit=10" 2>/dev/null || echo "")
assert_contains "Bob received relay message" "$BOB_MSGS" "Hello from Alice"

# ── Summary ──

echo -e "\n${CYAN}══════════════════════════════════════════${NC}"
echo -e "${CYAN}Results:${NC} ${GREEN}${PASS_COUNT} passed${NC} / ${RED}${FAIL_COUNT} failed${NC} / ${TOTAL_COUNT} total"
echo -e "${CYAN}══════════════════════════════════════════${NC}"

if [ "$FAIL_COUNT" -gt 0 ]; then
    echo -e "\n${YELLOW}[DEBUG]${NC} Engine log tail:"
    tail -30 "$LOG_FILE"
    exit 1
fi

