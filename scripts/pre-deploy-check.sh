#!/bin/bash
# 准入测试：自部署前必须全部通过
# 用法：bash scripts/pre-deploy-check.sh

set -e
cd /data/alice-dev

echo "========================================="
echo "  Pre-Deploy Checks"
echo "========================================="

# 1. Guardian
echo ""
echo "[1/4] Guardian..."
RESULT=$(python3 defense/guardian/guardian.py engine/src 2>&1 | grep "violations")
echo "  $RESULT"
if echo "$RESULT" | grep -q "^.GUARD. 0 violations"; then
    echo "  ✅ Guardian passed"
else
    echo "  ❌ Guardian FAILED"
    exit 1
fi

# 2. Unit Tests
echo ""
echo "[2/4] Unit Tests..."
CARGO_TARGET_DIR=/data/rust-target-dev cargo test --release 2>&1 | tail -3
echo "  ✅ Unit tests passed"

# 3. Build
echo ""
echo "[3/4] Build..."
CARGO_TARGET_DIR=/data/rust-target-dev cargo build --release 2>&1 | tail -3
echo "  ✅ Build passed"

# 4. E2E Tests
echo ""
echo "[4/4] E2E Tests..."

echo "  Running hello_world..."
bash integration/scripts/run_e2e.sh hello_world 2>&1 | grep -E "PASSED|FAILED"

echo "  Running settings_knowledge..."
bash integration/scripts/run_e2e.sh settings_knowledge 2>&1 | grep -E "PASSED|FAILED"

echo ""
echo "========================================="
echo "  ✅ All checks passed. Safe to deploy."
echo "========================================="
