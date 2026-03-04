#!/bin/bash
# Alice Engine 自我部署脚本
# 前置检查 → 优雅切换（不中断当前beat）

set -e

PROJECT_DIR="/data/alice-dev"
ENGINE_BIN="/data/rust-target-dev/release/alice-engine"
RUNTIME_DIR="/data/alice-dev-runtime"
ENGINE_PORT=9527

cd "$PROJECT_DIR"

echo "========================================="
echo "  Alice Engine Self-Deploy"
echo "========================================="

# === 读取当前引擎的环境变量 ===
OLD_PID=$(ss -tlnp | grep ":${ENGINE_PORT} " | grep -oP 'pid=\K[0-9]+' || echo "")

if [ -z "$OLD_PID" ]; then
    echo "  ❌ No engine running on port $ENGINE_PORT, cannot inherit env"
    exit 1
fi

echo "Old engine PID: $OLD_PID"
echo "Inheriting environment variables from running engine..."

# 从运行中的引擎进程读取所有ALICE_*环境变量
ENV_FILE=$(mktemp)
cat /proc/$OLD_PID/environ | tr '\0' '\n' | grep '^ALICE_' > "$ENV_FILE"
echo "  Found $(wc -l < "$ENV_FILE") ALICE_* variables"

# === 部署：后台切换 ===
nohup bash -c '
    sleep 3
    # 加载环境变量
    set -a
    source "'"$ENV_FILE"'"
    set +a
    # Kill旧进程
    OLD_PID="'"$OLD_PID"'"
    if [ -n "$OLD_PID" ] && kill -0 "$OLD_PID" 2>/dev/null; then
        kill "$OLD_PID"
        echo "Killed old engine PID $OLD_PID"
        sleep 1
    fi
    # 启动新引擎（环境变量已从旧进程继承）
    '"$ENGINE_BIN"' &
    echo "New engine started with PID $!"
    # 清理临时文件
    rm -f "'"$ENV_FILE"'"
' > "$RUNTIME_DIR/logs/deploy.log" 2>&1 &

echo "Deploy scheduled. Engine will restart in ~3 seconds."
