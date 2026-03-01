#!/bin/bash
# Alice Engine Release Script
# 在打包机上执行：bash /opt/alice/ops/release.sh
# 流程：git pull → build all platforms → 部署本机 → 更新 release 目录 → 打包本地版

set -e

SOURCECODE="/opt/alice/sourcecode/alice-engine-rust"
ENGINE_PATH="/opt/alice/engine/alice-engine"
RELEASE_DIR="/opt/alice/release"
DOWNLOAD_DIR="$RELEASE_DIR/download"
WEB_SRC="/opt/alice/sourcecode/web"
WEB_DEPLOY="/opt/alice/web"

cd "$SOURCECODE"

echo "=== [1/6] Git Pull ==="
git pull
COMMIT_HASH=$(git rev-parse --short HEAD)
COMMIT_MSG=$(git log --oneline -1)
echo "Latest commit: $COMMIT_MSG"

echo ""
echo "=== [2/6] Build Linux ==="
cargo build --release 2>&1 | tail -5
LINUX_PATH="$SOURCECODE/target/release/alice-engine"
LINUX_MD5=$(md5sum "$LINUX_PATH" | awk '{print $1}')
echo "Linux x86_64 build OK (MD5: $LINUX_MD5)"

echo ""
echo "=== [3/6] Build macOS ==="
# Apple Silicon (arm64)
echo "Building aarch64-apple-darwin..."
cargo zigbuild --release --target aarch64-apple-darwin 2>&1 | tail -3
MACOS_ARM64_PATH="$SOURCECODE/target/aarch64-apple-darwin/release/alice-engine"
echo "macOS arm64 build OK ($(du -h "$MACOS_ARM64_PATH" | awk '{print $1}'))"

# Intel Mac (x86_64)
echo "Building x86_64-apple-darwin..."
cargo zigbuild --release --target x86_64-apple-darwin 2>&1 | tail -3
MACOS_X64_PATH="$SOURCECODE/target/x86_64-apple-darwin/release/alice-engine"
echo "macOS x86_64 build OK ($(du -h "$MACOS_X64_PATH" | awk '{print $1}'))"

echo ""
echo "=== [4/6] Deploy Engine (本机) ==="
systemctl stop alice-engine
sleep 1
if pgrep -f alice-engine > /dev/null 2>&1; then
    echo "Force killing remaining processes..."
    pkill -9 -f alice-engine
    sleep 1
fi
cp "$LINUX_PATH" "$ENGINE_PATH"
chmod 755 "$ENGINE_PATH"
systemctl start alice-engine
sleep 2
if systemctl is-active --quiet alice-engine; then
    echo "Engine deployed and running ✅"
else
    echo "ERROR: Engine failed to start! ❌"
    systemctl status alice-engine | head -20
    exit 1
fi

echo ""
echo "=== [5/6] Update Release Directory ==="
# Version info (commit hash for human readability)
echo "$LINUX_MD5" > "$RELEASE_DIR/version"

# Linux binary (for cloud servers / updater)
cp "$LINUX_PATH" "$RELEASE_DIR/alice-engine"
chmod 755 "$RELEASE_DIR/alice-engine"
echo "$LINUX_MD5  alice-engine" > "$RELEASE_DIR/alice-engine.md5"
echo "Linux binary updated (MD5: $LINUX_MD5)"

# Updater script (for self-update)
cp "$SOURCECODE/ops/updater.sh" "$RELEASE_DIR/updater.sh"
chmod 755 "$RELEASE_DIR/updater.sh"
echo "Updater script updated"

# Frontend
mkdir -p "$WEB_DEPLOY" "$RELEASE_DIR/web"
cp "$WEB_SRC"/*.html "$WEB_DEPLOY"/
cp "$WEB_SRC"/*.html "$RELEASE_DIR"/web/
(cd "$RELEASE_DIR/web" && ls *.html > filelist.txt)
WEB_MD5=$(cd "$RELEASE_DIR/web" && cat filelist.txt | while read f; do md5sum "$f" 2>/dev/null; done | sort | md5sum | awk '{print $1}')
echo "$WEB_MD5" > "$RELEASE_DIR/web-version"
# Inject version into HTML
sed -i 's/data-version="dev"/data-version="'"$COMMIT_HASH"'"/' "$WEB_DEPLOY"/*.html "$RELEASE_DIR"/web/*.html
echo "Frontend updated (web-version: $WEB_MD5, version injected: $COMMIT_HASH)"

echo ""
echo "=== [6/6] Package Local Versions ==="
mkdir -p "$DOWNLOAD_DIR"

# Helper function: package a local version tar.gz
package_local() {
    local BINARY_PATH="$1"
    local ARCHIVE_NAME="$2"
    local TEMP_DIR=$(mktemp -d)
    local PKG_DIR="$TEMP_DIR/alice"
    
    mkdir -p "$PKG_DIR/web"
    cp "$BINARY_PATH" "$PKG_DIR/alice-engine"
    chmod 755 "$PKG_DIR/alice-engine"
    cp "$WEB_SRC"/*.html "$PKG_DIR/web/"
    
    # Generate install script
    cat > "$PKG_DIR/install.sh" << 'INSTALL_EOF'
#!/bin/bash
# Alice Engine Local Installer
# Usage: bash install.sh

set -e
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

echo "=== Alice Engine Local Setup ==="
echo "Directory: $SCRIPT_DIR"
echo ""

# Check if engine binary exists
if [ ! -f "$SCRIPT_DIR/alice-engine" ]; then
    echo "ERROR: alice-engine binary not found in $SCRIPT_DIR"
    exit 1
fi

chmod 755 "$SCRIPT_DIR/alice-engine"

echo "Starting Alice Engine..."
echo "Open your browser when the setup page appears."
echo ""

export ALICE_BASE_DIR="$SCRIPT_DIR"
export ALICE_BIND_ADDR="127.0.0.1"
export ALICE_PORT="8080"
export ALICE_AUTO_BROWSER="true"
export ALICE_SETUP_ENABLED="true"
export ALICE_SKIP_AUTH="true"

exec "$SCRIPT_DIR/alice-engine"
INSTALL_EOF
    chmod 755 "$PKG_DIR/install.sh"
    
    # Create tar.gz
    (cd "$TEMP_DIR" && tar czf "$DOWNLOAD_DIR/$ARCHIVE_NAME" alice/)
    rm -rf "$TEMP_DIR"
    
    local SIZE=$(du -h "$DOWNLOAD_DIR/$ARCHIVE_NAME" | awk '{print $1}')
    echo "  $ARCHIVE_NAME ($SIZE)"
}

echo "Packaging local versions..."
package_local "$MACOS_ARM64_PATH" "alice-local-macos-arm64.tar.gz"
package_local "$MACOS_X64_PATH" "alice-local-macos-x86_64.tar.gz"
package_local "$LINUX_PATH" "alice-local-linux-x86_64.tar.gz"

# Copy online installer script
cp "$SOURCECODE/ops/install.sh" "$DOWNLOAD_DIR/install.sh"
chmod 755 "$DOWNLOAD_DIR/install.sh"
echo "  install.sh updated"

# Copy download page (project homepage)
cp "$SOURCECODE/ops/download_page.html" "$DOWNLOAD_DIR/index.html"
echo "  index.html updated"

# Write release info for download page
cat > "$DOWNLOAD_DIR/release-info.json" << REOF
{
    "version": "$COMMIT_HASH",
    "commit": "$COMMIT_MSG",
    "date": "$(date -u +%Y-%m-%dT%H:%M:%SZ)",
    "packages": {
        "macos-arm64": "alice-local-macos-arm64.tar.gz",
        "macos-x86_64": "alice-local-macos-x86_64.tar.gz",
        "linux-x86_64": "alice-local-linux-x86_64.tar.gz"
    }
}
REOF
echo "  release-info.json written"

echo ""
echo "=== Summary ==="
echo "Version:    $COMMIT_HASH"
echo "Commit:     $COMMIT_MSG"
echo "Linux MD5:  $LINUX_MD5"
echo "Web:        $WEB_MD5"
echo "Downloads:  $(ls $DOWNLOAD_DIR/*.tar.gz | wc -l) packages"
echo ""
echo "专机 updater 将在下一轮自动拉取更新 ✅"
