#!/bin/bash
#
# Alice Engine Release Build Script
# Run on build machine (8.149.243.230)
# Usage: bash scripts/release-build.sh [--update-runtime]
#

set -e

REPO_DIR="/root/alice-dev"
RELEASE_DIR="/opt/alice/release/latest"
RUNTIME_DIR="/opt/alice/runtime"

# Target mapping: cargo zigbuild target → start.sh expected filename
declare -A TARGETS=(
    ["x86_64-unknown-linux-gnu"]="alice-engine-linux-x86_64"
    ["aarch64-apple-darwin"]="alice-engine-macos-arm64"
    ["x86_64-apple-darwin"]="alice-engine-macos-x86_64"
)

UPDATE_RUNTIME=false
if [ "$1" = "--update-runtime" ]; then
    UPDATE_RUNTIME=true
fi

echo "=== Step 1: git pull ==="
cd "$REPO_DIR"
git pull
echo ""

echo "=== Step 2: Build all targets ==="
for target in "${!TARGETS[@]}"; do
    echo "Building $target ..."
    cargo zigbuild --release --target "$target" -p alice-engine
done
echo ""

echo "=== Step 3: Copy binaries to release directory ==="
mkdir -p "$RELEASE_DIR"
for target in "${!TARGETS[@]}"; do
    src="$REPO_DIR/target/$target/release/alice-engine"
    dst="$RELEASE_DIR/${TARGETS[$target]}"
    cp "$src" "$dst"
    chmod +x "$dst"
    echo "  $target → ${TARGETS[$target]}"
done

# Copy start.sh
cp "$REPO_DIR/start.sh" "$RELEASE_DIR/start.sh"
chmod +x "$RELEASE_DIR/start.sh"
echo "  start.sh copied"
echo ""

echo "=== Step 4: Update version.txt ==="
VERSION=$(md5sum "$RELEASE_DIR/alice-engine-linux-x86_64" | awk '{print $1}')
echo "$VERSION" > "$RELEASE_DIR/version.txt"
echo "  version: $VERSION"
echo ""

echo "=== Step 5: Verify ==="
ls -la "$RELEASE_DIR/"
echo ""

if [ "$UPDATE_RUNTIME" = true ]; then
    echo "=== Step 6: Update runtime ==="
    RUNTIME_BIN="$RUNTIME_DIR/alice-engine"
    
    echo "  Stopping runtime..."
    systemctl stop alice-runtime.service 2>/dev/null || true
    sleep 2
    
    echo "  Copying new binary..."
    cp "$RELEASE_DIR/alice-engine-linux-x86_64" "$RUNTIME_BIN"
    chmod +x "$RUNTIME_BIN"
    
    echo "  Starting runtime..."
    systemctl start alice-runtime.service 2>/dev/null || bash "$RUNTIME_DIR/restart.sh"
    sleep 2
    
    echo "  Runtime updated ✅"
else
    echo "Runtime not updated (use --update-runtime to update)"
fi

echo ""
echo "✅ Release build complete!"
