#!/bin/bash
#
# Hub Distributed E2E Tests
# Tests hub host/slave operations with temporary engine processes.
#
# Usage: bash tests/hub_e2e.sh
#

set -euo pipefail

# ── Configuration ──
BINARY="/data/cargo-target/release/alice-engine"
HOST_PORT=9901
SLAVE_PORT=9902
HOST_URL="http://localhost:${HOST_PORT}"
SLAVE_URL="http://localhost:${SLAVE_PORT}"
JOIN_TOKEN="e2e-test-token"
HOST_DIR="/tmp/e2e-hub-host"
SLAVE_DIR="/tmp/e2e-hub-slave"
HTML_DIR="$(cd "$(dirname "$0")/../html-frontend" && pwd)"

# ── State ──
HOST_PID=""
SLAVE_PID=""
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
    echo -e "\n${CYAN}[CLEANUP]${NC} Stopping processes and removing temp dirs..."
    [ -n "$HOST_PID" ] && kill "$HOST_PID" 2>/dev/null && wait "$HOST_PID" 2>/dev/null || true
    [ -n "$SLAVE_PID" ] && kill "$SLAVE_PID" 2>/dev/null && wait "$SLAVE_PID" 2>/dev/null || true
    HOST_PID=""
    SLAVE_PID=""
    rm -rf "$HOST_DIR" "$SLAVE_DIR"
}

trap cleanup EXIT

start_engine() {
    local name="$1" port="$2" data_dir="$3" host_url="$4"
    rm -rf "$data_dir"
    mkdir -p "$data_dir"
    ALICE_HTTP_PORT="$port" \
    ALICE_BASE_DIR="$data_dir" \
    ALICE_INSTANCES_DIR="$data_dir/instances" \
    ALICE_LOGS_DIR="$data_dir/logs" \
    ALICE_SKIP_AUTH=true \
    ALICE_HOST="$host_url" \
    ALICE_HTML_DIR="$HTML_DIR" \
    ALICE_AUTH_SECRET="e2e-secret-${port}" \
    "$BINARY" > "$data_dir/engine.log" 2>&1 &
    local pid=$!
    echo "$pid"
}

wait_for_port() {
    local port="$1" max_wait="${2:-15}"
    local elapsed=0
    while ! curl -sf "http://localhost:${port}/api/hub/status" > /dev/null 2>&1; do
        sleep 0.5
        elapsed=$((elapsed + 1))
        if [ "$elapsed" -ge "$((max_wait * 2))" ]; then
            echo -e "${RED}[ERROR]${NC} Port $port not ready after ${max_wait}s"
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
        echo -e "  ${RED}✗${NC} $desc (expected to contain: ${needle}, got: ${haystack})"
        FAIL_COUNT=$((FAIL_COUNT + 1))
    fi
}

# Get hub mode from status response
hub_mode() {
    local url="$1"
    api GET "$url/api/hub/status" | python3 -c "import sys,json; print(json.load(sys.stdin).get('mode',''))" 2>/dev/null
}

# Count endpoint groups from hub/endpoints
endpoint_count() {
    local url="$1"
    api GET "$url/api/hub/endpoints" | python3 -c "import sys,json; print(len(json.load(sys.stdin)))" 2>/dev/null
}

# Get all endpoint names
endpoint_names() {
    local url="$1"
    api GET "$url/api/hub/endpoints" | python3 -c "
import sys,json
groups = json.load(sys.stdin)
for g in groups:
    print(g['endpoint'])
" 2>/dev/null
}

# Count total remote instances visible from host
remote_instance_count() {
    local url="$1"
    api GET "$url/api/hub/endpoints" | python3 -c "
import sys,json
groups = json.load(sys.stdin)
total = 0
for g in groups:
    if g['endpoint'] != 'local':
        total += len(g['instances'])
print(total)
" 2>/dev/null
}

# Create an instance on an engine
create_instance() {
    local url="$1" name="$2"
    api_post "$url/api/instances" "{\"name\":\"$name\"}"
}

# Count instances on an engine
instance_count() {
    local url="$1"
    api GET "$url/api/instances" | python3 -c "import sys,json; d=json.load(sys.stdin); print(len(d.get('instances',d)) if isinstance(d,dict) else len(d))" 2>/dev/null
}

start_host() {
    HOST_PID=$(start_engine "host" "$HOST_PORT" "$HOST_DIR" "$HOST_URL")
    wait_for_port "$HOST_PORT"
}

start_slave() {
    SLAVE_PID=$(start_engine "slave" "$SLAVE_PORT" "$SLAVE_DIR" "$SLAVE_URL")
    wait_for_port "$SLAVE_PORT"
}

stop_host() {
    [ -n "$HOST_PID" ] && kill "$HOST_PID" 2>/dev/null && wait "$HOST_PID" 2>/dev/null || true
    HOST_PID=""
}

stop_slave() {
    [ -n "$SLAVE_PID" ] && kill "$SLAVE_PID" 2>/dev/null && wait "$SLAVE_PID" 2>/dev/null || true
    SLAVE_PID=""
}

# ── Test Cases ──

test_basic_flow() {
    echo -e "\n${CYAN}[TEST 1]${NC} Basic flow: enable → join → verify → leave → verify"

    start_host
    start_slave

    # Create an instance on slave so it has a real engine_id
    create_instance "$SLAVE_URL" "slave-inst-1" > /dev/null

    # Host enable
    api_post "$HOST_URL/api/hub/enable" "{\"join_token\":\"$JOIN_TOKEN\"}" > /dev/null
    assert_eq "Host mode is 'host'" "host" "$(hub_mode "$HOST_URL")"

    # Slave join
    api_post "$SLAVE_URL/api/hub/join" "{\"host_url\":\"$HOST_URL\",\"join_token\":\"$JOIN_TOKEN\"}" > /dev/null
    sleep 1
    assert_eq "Slave mode is 'joined'" "joined" "$(hub_mode "$SLAVE_URL")"

    # Verify host sees remote instances
    local ep_count
    ep_count=$(endpoint_count "$HOST_URL")
    assert_eq "Host has 2 endpoint groups (local + slave)" "2" "$ep_count"

    local remote_count
    remote_count=$(remote_instance_count "$HOST_URL")
    assert_eq "Host sees 1 remote instance" "1" "$remote_count"

    # Slave leave
    api_post "$SLAVE_URL/api/hub/leave" "{}" > /dev/null
    sleep 1
    assert_eq "Slave mode is 'off' after leave" "off" "$(hub_mode "$SLAVE_URL")"

    # Host should have only local group after slave leaves
    sleep 2  # Wait for heartbeat to detect disconnection
    ep_count=$(endpoint_count "$HOST_URL")
    assert_eq "Host has 1 endpoint group after slave leave" "1" "$ep_count"

    # Disable host
    api_post "$HOST_URL/api/hub/disable" "{}" > /dev/null
    assert_eq "Host mode is 'off' after disable" "off" "$(hub_mode "$HOST_URL")"

    stop_host
    stop_slave
}

test_host_restart_reconnect() {
    echo -e "\n${CYAN}[TEST 2]${NC} Host restart → slave auto-reconnect"

    start_host
    start_slave

    create_instance "$SLAVE_URL" "slave-inst-2" > /dev/null

    # Enable host and join
    api_post "$HOST_URL/api/hub/enable" "{\"join_token\":\"$JOIN_TOKEN\"}" > /dev/null
    api_post "$SLAVE_URL/api/hub/join" "{\"host_url\":\"$HOST_URL\",\"join_token\":\"$JOIN_TOKEN\"}" > /dev/null
    sleep 1
    assert_eq "Slave joined before restart" "joined" "$(hub_mode "$SLAVE_URL")"

    # Kill host
    stop_host
    sleep 2

    # Restart host and re-enable
    start_host
    api_post "$HOST_URL/api/hub/enable" "{\"join_token\":\"$JOIN_TOKEN\"}" > /dev/null
    assert_eq "Host re-enabled" "host" "$(hub_mode "$HOST_URL")"

    # Wait for slave auto-reconnect (slave has reconnect logic)
    local max_wait=30
    local waited=0
    local reconnected=false
    while [ "$waited" -lt "$max_wait" ]; do
        local ep_count
        ep_count=$(endpoint_count "$HOST_URL" 2>/dev/null || echo "0")
        if [ "$ep_count" = "2" ]; then
            reconnected=true
            break
        fi
        sleep 1
        waited=$((waited + 1))
    done

    TOTAL_COUNT=$((TOTAL_COUNT + 1))
    if [ "$reconnected" = "true" ]; then
        echo -e "  ${GREEN}✓${NC} Slave auto-reconnected after host restart (${waited}s)"
        PASS_COUNT=$((PASS_COUNT + 1))
    else
        echo -e "  ${RED}✗${NC} Slave did not auto-reconnect after ${max_wait}s"
        FAIL_COUNT=$((FAIL_COUNT + 1))
    fi

    # Cleanup
    api_post "$SLAVE_URL/api/hub/leave" "{}" > /dev/null 2>&1 || true
    api_post "$HOST_URL/api/hub/disable" "{}" > /dev/null 2>&1 || true
    stop_host
    stop_slave
}

test_zombie_endpoint() {
    echo -e "\n${CYAN}[TEST 3]${NC} Zombie endpoint: join(no instances) → leave → join(with instances)"
    echo -e "  ${YELLOW}(Expected to FAIL — this is the bug we're reproducing)${NC}"

    start_host
    start_slave

    # Enable host
    api_post "$HOST_URL/api/hub/enable" "{\"join_token\":\"$JOIN_TOKEN\"}" > /dev/null

    # First join: slave has NO instances → engine_id will be "unknown"
    api_post "$SLAVE_URL/api/hub/join" "{\"host_url\":\"$HOST_URL\",\"join_token\":\"$JOIN_TOKEN\"}" > /dev/null
    sleep 1

    local ep_count_first
    ep_count_first=$(endpoint_count "$HOST_URL")
    assert_eq "Host has 2 groups after first join" "2" "$ep_count_first"

    # Leave
    api_post "$SLAVE_URL/api/hub/leave" "{}" > /dev/null
    sleep 2  # Wait for cleanup

    # Create an instance on slave → engine_id will change
    create_instance "$SLAVE_URL" "zombie-test-inst" > /dev/null

    # Second join: slave now has instances → engine_id will be the instance id
    api_post "$SLAVE_URL/api/hub/join" "{\"host_url\":\"$HOST_URL\",\"join_token\":\"$JOIN_TOKEN\"}" > /dev/null
    sleep 1

    # BUG: host should have exactly 2 groups (local + slave), but zombie "unknown" entry may persist
    local ep_count_second
    ep_count_second=$(endpoint_count "$HOST_URL")
    assert_eq "Host has exactly 2 groups after second join (no zombie)" "2" "$ep_count_second"

    # Also check: no "unknown" engine_id in endpoint names
    local ep_names
    ep_names=$(endpoint_names "$HOST_URL")
    TOTAL_COUNT=$((TOTAL_COUNT + 1))
    if echo "$ep_names" | grep -q "unknown"; then
        echo -e "  ${RED}✗${NC} Zombie 'unknown' endpoint still exists in host connections"
        FAIL_COUNT=$((FAIL_COUNT + 1))
    else
        echo -e "  ${GREEN}✓${NC} No zombie 'unknown' endpoint"
        PASS_COUNT=$((PASS_COUNT + 1))
    fi

    # Cleanup
    api_post "$SLAVE_URL/api/hub/leave" "{}" > /dev/null 2>&1 || true
    api_post "$HOST_URL/api/hub/disable" "{}" > /dev/null 2>&1 || true
    stop_host
    stop_slave
}

test_slave_new_instance_visibility() {
    echo -e "\n${CYAN}[TEST 4]${NC} Slave creates new instance after join → host should see it"
    echo -e "  ${YELLOW}(Expected to FAIL — host doesn't know about new instances)${NC}"

    start_host
    start_slave

    # Create initial instance on slave
    create_instance "$SLAVE_URL" "initial-inst" > /dev/null

    # Enable host and join
    api_post "$HOST_URL/api/hub/enable" "{\"join_token\":\"$JOIN_TOKEN\"}" > /dev/null
    api_post "$SLAVE_URL/api/hub/join" "{\"host_url\":\"$HOST_URL\",\"join_token\":\"$JOIN_TOKEN\"}" > /dev/null
    sleep 1

    local remote_before
    remote_before=$(remote_instance_count "$HOST_URL")
    assert_eq "Host sees 1 remote instance initially" "1" "$remote_before"

    # Create a NEW instance on slave after join
    create_instance "$SLAVE_URL" "new-after-join" > /dev/null
    sleep 2  # Give some time for potential sync

    local remote_after
    remote_after=$(remote_instance_count "$HOST_URL")
    assert_eq "Host sees 2 remote instances after slave creates new one" "2" "$remote_after"

    # Cleanup
    api_post "$SLAVE_URL/api/hub/leave" "{}" > /dev/null 2>&1 || true
    api_post "$HOST_URL/api/hub/disable" "{}" > /dev/null 2>&1 || true
    stop_host
    stop_slave
}

# ── Main ──

echo -e "${CYAN}╔══════════════════════════════════════╗${NC}"
echo -e "${CYAN}║   Hub Distributed E2E Tests          ║${NC}"
echo -e "${CYAN}╚══════════════════════════════════════╝${NC}"
echo ""
echo "Binary: $BINARY"
echo "Host:   localhost:$HOST_PORT"
echo "Slave:  localhost:$SLAVE_PORT"

# Check binary exists
if [ ! -x "$BINARY" ]; then
    echo -e "${RED}[ERROR]${NC} Binary not found: $BINARY"
    exit 1
fi

# Check ports are free
for port in $HOST_PORT $SLAVE_PORT; do
    if curl -sf "http://localhost:${port}/" > /dev/null 2>&1; then
        echo -e "${RED}[ERROR]${NC} Port $port is already in use"
        exit 1
    fi
done

test_basic_flow
test_host_restart_reconnect
test_zombie_endpoint
test_slave_new_instance_visibility

# ── Summary ──
echo -e "\n${CYAN}══════════════════════════════════════${NC}"
echo -e "Results: ${GREEN}${PASS_COUNT} passed${NC}, ${RED}${FAIL_COUNT} failed${NC}, ${TOTAL_COUNT} total"
if [ "$FAIL_COUNT" -gt 0 ]; then
    echo -e "${RED}SOME TESTS FAILED${NC}"
    exit 1
else
    echo -e "${GREEN}ALL TESTS PASSED${NC}"
    exit 0
fi

