#!/bin/bash
# Alice Engine 自部署（含自动备份）
# 用法: bash /opt/alice/ops/self-deploy.sh [binary_path]

set -e

ENGINE_DIR="/opt/alice/engine"
BINARY="$ENGINE_DIR/alice-engine"
BACKUP="$ENGINE_DIR/alice-engine.prev"
SOURCE="${1:-/opt/alice/sourcecode/alice-engine-rust/target/release/alice-engine}"
PID_FILE="/var/run/alice-engine.pid"
SHUTDOWN_SIGNAL="/var/run/alice-engine-shutdown.signal"

if [ ! -f "$SOURCE" ]; then
    echo "❌ 找不到新 binary: $SOURCE"
    exit 1
fi

echo "📋 当前版本:"
md5sum "$BINARY" 2>/dev/null || echo "  (不存在)"
echo "📋 新版本:"
md5sum "$SOURCE"

# 备份当前版本
if [ -f "$BINARY" ]; then
    cp "$BINARY" "$BACKUP"
    echo "💾 已备份到 $BACKUP"
fi

echo "⏳ 正在部署（nohup 脱离进程树）..."

# nohup 脱离：graceful stop → cp → start
nohup bash -c '
    ENGINE_DIR="/opt/alice/engine"
    BINARY="$ENGINE_DIR/alice-engine"
    SOURCE="'"$SOURCE"'"
    PID_FILE="/var/run/alice-engine.pid"
    SHUTDOWN_SIGNAL="/var/run/alice-engine-shutdown.signal"

    # 优雅停止：写 shutdown signal 文件
    graceful_stop() {
        # 优先 systemctl（专机模式）
        if systemctl stop alice-engine 2>/dev/null; then
            echo "stopped via systemctl"
            return 0
        fi

        # 开发机模式：写 signal 文件等待优雅退出
        local PID=""
        if [ -f "$PID_FILE" ]; then
            PID=$(cat "$PID_FILE")
        fi
        if [ -z "$PID" ]; then
            PID=$(pgrep -f "^/opt/alice/engine/alice-engine" 2>/dev/null || true)
        fi

        if [ -n "$PID" ] && kill -0 "$PID" 2>/dev/null; then
            echo "shutdown" > "$SHUTDOWN_SIGNAL"
            echo "wrote shutdown signal, waiting for PID $PID..."
            for i in $(seq 1 15); do
                if ! kill -0 "$PID" 2>/dev/null; then
                    echo "process exited gracefully after ${i}s"
                    return 0
                fi
                sleep 1
            done
            # 超时强制杀
            echo "graceful timeout, force killing..."
            kill -9 "$PID" 2>/dev/null || true
            sleep 1
        else
            echo "no running engine found"
        fi
        rm -f "$PID_FILE"
    }

    graceful_stop
    sleep 1

    # 复制新 binary
    cp "$SOURCE" "$BINARY"
    chmod +x "$BINARY"

    # 复制静态文件（从git仓库的static/目录）
    STATIC_SRC="/opt/alice/sourcecode/web"
    WEB_DIR="/opt/alice/web"
    if [ -d "$STATIC_SRC" ] && [ -d "$WEB_DIR" ]; then
        cp "$STATIC_SRC"/*.html "$WEB_DIR"/ 2>/dev/null && echo "static files updated" || true
        # Inject version into HTML
        COMMIT_HASH=$(cd /opt/alice/sourcecode/alice-engine-rust && git rev-parse --short HEAD 2>/dev/null || echo "dev")
        sed -i "s/data-version=\"dev\"/data-version=\"$COMMIT_HASH\"/" "$WEB_DIR"/*.html 2>/dev/null || true
        echo "version injected: $COMMIT_HASH"
    fi



    # 加载环境变量（优先根目录，fallback到ops目录）
    ALICE_HOME="/opt/alice"
    if [ -f "$ALICE_HOME/engine.env" ]; then
        set -a; source "$ALICE_HOME/engine.env"; set +a
        echo "loaded engine.env from $ALICE_HOME/"
    elif [ -f "$ALICE_HOME/ops/engine.env" ]; then
        set -a; source "$ALICE_HOME/ops/engine.env"; set +a
        echo "loaded engine.env from $ALICE_HOME/ops/"
    fi

    # 启动：优先 systemctl，否则直接启动
    if systemctl start alice-engine 2>/dev/null; then
        echo "started via systemctl"
    else
        # 直接启动（开发机模式）
        nohup "$BINARY" /opt/alice/instances /opt/alice/logs 8081 /opt/alice/web >> /opt/alice/logs/engine.log 2>&1 &
        echo "started directly, PID=$!"
    fi

    echo "[$(date)] self-deploy complete, source=$SOURCE" >> /opt/alice/logs/deploy.log
' > /opt/alice/logs/self-deploy-output.log 2>&1 &

echo "✅ 部署命令已提交（nohup），引擎将在几秒后重启"
echo "   回滚命令: bash /opt/alice/ops/rollback.sh"
