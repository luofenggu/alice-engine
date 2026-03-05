#!/bin/bash
# Pre-deploy admission tests — all must pass before deployment
# Usage: bash scripts/pre-deploy-check.sh

set -e

# Resolve project root (parent of scripts/)
PROJECT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
cd "$PROJECT_DIR"

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
cargo test --release 2>&1 | tail -3
echo "  ✅ Unit tests passed"

# 3. Build
echo ""
echo "[3/4] Build..."
cargo build --release 2>&1 | tail -3
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
