#!/bin/bash
#
# Alice Engine Launcher
# Double-click this file (macOS: rename to Alice.command) or run: bash start.sh
#

set -e

ALICE_HOME="$HOME/.alice"
ALICE_BIN="$ALICE_HOME/alice-engine"
ALICE_DATA="$ALICE_HOME/data"
ALICE_VERSION="$ALICE_HOME/version.txt"
ALICE_PORT=8081

# Download source
BASE_URL="http://8.149.243.230/release/latest"

# --- Detect platform ---
detect_platform() {
    local os arch
    os="$(uname -s)"
    arch="$(uname -m)"

    case "$os" in
        Linux)  os="linux" ;;
        Darwin) os="macos" ;;
        *)      echo "❌ Unsupported OS: $os"; exit 1 ;;
    esac

    case "$arch" in
        x86_64|amd64)  arch="x86_64" ;;
        arm64|aarch64) arch="arm64" ;;
        *)             echo "❌ Unsupported architecture: $arch"; exit 1 ;;
    esac

    # Linux arm64 not available yet
    if [ "$os" = "linux" ] && [ "$arch" = "arm64" ]; then
        echo "❌ Linux arm64 is not supported yet. Use x86_64."
        exit 1
    fi

    echo "${os}-${arch}"
}

# --- Check and download/update binary ---
ensure_binary() {
    local platform="$1"
    local download_url="${BASE_URL}/alice-engine-${platform}"

    mkdir -p "$ALICE_HOME"
    mkdir -p "$ALICE_DATA"

    # Check for updates
    local remote_version=""
    remote_version=$(curl -fsSL "${BASE_URL}/version.txt" 2>/dev/null || echo "")

    if [ -f "$ALICE_BIN" ] && [ -f "$ALICE_VERSION" ]; then
        local local_version
        local_version=$(cat "$ALICE_VERSION")
        if [ -n "$remote_version" ] && [ "$local_version" = "$remote_version" ]; then
            echo "✅ Alice Engine is up to date ($local_version)"
            return 0
        elif [ -n "$remote_version" ]; then
            echo "🔄 Update available: $local_version → $remote_version"
        else
            echo "⚠️  Could not check for updates. Using existing binary."
            return 0
        fi
    else
        echo "📦 First time setup — downloading Alice Engine..."
    fi

    # Download
    echo "⬇️  Downloading alice-engine-${platform}..."
    if curl -fSL --progress-bar -o "${ALICE_BIN}.tmp" "$download_url"; then
        mv "${ALICE_BIN}.tmp" "$ALICE_BIN"
        chmod +x "$ALICE_BIN"
        if [ -n "$remote_version" ]; then
            echo "$remote_version" > "$ALICE_VERSION"
        fi
        echo "✅ Download complete!"
    else
        rm -f "${ALICE_BIN}.tmp"
        if [ -f "$ALICE_BIN" ]; then
            echo "⚠️  Download failed. Using existing binary."
        else
            echo "❌ Download failed. Please check your network and try again."
            exit 1
        fi
    fi
}

# --- Check if port is in use ---
check_port() {
    if command -v lsof >/dev/null 2>&1; then
        if lsof -i ":$ALICE_PORT" -sTCP:LISTEN >/dev/null 2>&1; then
            echo ""
            echo "🔔 Alice Engine is already running on port $ALICE_PORT"
            echo "   Opening browser..."
            open_browser
            exit 0
        fi
    elif command -v ss >/dev/null 2>&1; then
        if ss -tlnp "sport = :$ALICE_PORT" 2>/dev/null | grep -q LISTEN; then
            echo ""
            echo "🔔 Alice Engine is already running on port $ALICE_PORT"
            echo "   Opening browser..."
            open_browser
            exit 0
        fi
    fi
}

# --- Open browser ---
open_browser() {
    local url="http://127.0.0.1:${ALICE_PORT}"
    if command -v open >/dev/null 2>&1; then
        open "$url"       # macOS
    elif command -v xdg-open >/dev/null 2>&1; then
        xdg-open "$url"   # Linux
    else
        echo "🌐 Open in your browser: $url"
    fi
}

# --- Main ---
main() {
    echo ""
    echo "  🤖 Alice Engine"
    echo "  ─────────────────"
    echo ""

    local platform
    platform=$(detect_platform)
    echo "📋 Platform: $platform"

    check_port
    ensure_binary "$platform"

    echo ""
    echo "🚀 Starting Alice Engine..."
    echo "   Data directory: $ALICE_DATA"
    echo ""

    # Open browser after a short delay
    (sleep 2 && open_browser) &

    # Start engine
    cd "$ALICE_DATA"
    exec "$ALICE_BIN"
}

main

