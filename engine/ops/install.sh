#!/bin/bash
# Alice Engine - One-line Installer
# Usage: curl -fsSL http://YOUR_SERVER_IP:8080/download/install.sh | bash
#
# Detects OS/architecture, downloads the right package, and starts Alice.

set -e

echo ""
echo "  🐰 Alice Engine Installer"
echo "  ========================="
echo ""

# Detect OS and architecture
OS=$(uname -s)
ARCH=$(uname -m)

if [ "$OS" = "Darwin" ] && [ "$ARCH" = "arm64" ]; then
    PACKAGE="alice-local-macos-arm64.tar.gz"
    PLATFORM="macOS (Apple Silicon)"
elif [ "$OS" = "Darwin" ] && [ "$ARCH" = "x86_64" ]; then
    PACKAGE="alice-local-macos-x86_64.tar.gz"
    PLATFORM="macOS (Intel)"
elif [ "$OS" = "Linux" ] && [ "$ARCH" = "x86_64" ]; then
    PACKAGE="alice-local-linux-x86_64.tar.gz"
    PLATFORM="Linux (x86_64)"
else
    echo "  ❌ Unsupported platform: $OS $ARCH"
    echo "     Supported: macOS (arm64/x86_64), Linux (x86_64)"
    exit 1
fi

echo "  Platform: $PLATFORM"
echo ""

BASE_URL="http://YOUR_SERVER_IP:8080/download"
INSTALL_DIR="$HOME/alice"

# Download
echo "  📦 Downloading $PACKAGE ..."
curl -fSL "$BASE_URL/$PACKAGE" -o "/tmp/$PACKAGE"

# Extract
echo "  📂 Installing to $INSTALL_DIR/ ..."
mkdir -p "$INSTALL_DIR"
tar xzf "/tmp/$PACKAGE" -C "$INSTALL_DIR" --strip-components=1
rm -f "/tmp/$PACKAGE"

chmod +x "$INSTALL_DIR/alice-engine"

# macOS: remove quarantine attribute
if [ "$OS" = "Darwin" ]; then
    xattr -d com.apple.quarantine "$INSTALL_DIR/alice-engine" 2>/dev/null || true
fi

echo ""
echo "  ✅ Installed to $INSTALL_DIR/"
echo ""
echo "  🎯 Starting Alice Engine..."
echo "     (Press Ctrl+C to stop)"
echo ""

# Start with local mode defaults
cd "$INSTALL_DIR"
export ALICE_BASE_DIR="$INSTALL_DIR"
export ALICE_BIND_ADDR="127.0.0.1"
export ALICE_PORT="8080"
export ALICE_AUTO_BROWSER="true"
export ALICE_SETUP_ENABLED="true"
export ALICE_SKIP_AUTH="true"

exec ./alice-engine
