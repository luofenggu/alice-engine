#!/bin/bash
# ═══════════════════════════════════════════════════════════════════════
# Alice Engine — 运维脚本
# ═══════════════════════════════════════════════════════════════════════
#
# 用途：在已安装的机器上控制Rust引擎的启停
# 位置：部署后位于 /opt/alice/ops/engine.sh
#
# 用法：
#   engine.sh start    — 启动引擎
#   engine.sh stop     — 停止引擎
#   engine.sh restart  — 重启引擎
#   engine.sh status   — 查看状态
#   engine.sh logs     — 查看最近日志
#   engine.sh tail     — 实时跟踪日志
#
# 配置：
#   环境变量从 /opt/alice/ops/engine.env 加载
#   engine.env 包含 ALICE_AUTH_SECRET、ALICE_DEFAULT_API_KEY 等敏感配置
#
# ═══════════════════════════════════════════════════════════════════════

ALICE_HOME="/opt/alice"
ENGINE_BIN="$ALICE_HOME/engine/alice-engine"
INSTANCES_DIR="$ALICE_HOME/instances"
LOGS_DIR="$ALICE_HOME/logs"
WEB_PORT="8081"
WEB_DIR="$ALICE_HOME/web"
PID_FILE="/var/run/alice-engine.pid"
LOG_FILE="$LOGS_DIR/engine.log"
# 加载环境变量（优先根目录，fallback到ops目录）
ENV_FILE="$ALICE_HOME/engine.env"
if [ ! -f "$ENV_FILE" ]; then
    ENV_FILE="$ALICE_HOME/ops/engine.env"
fi

if [ -f "$ENV_FILE" ]; then
    set -a
    source "$ENV_FILE"
    set +a
else
    echo "⚠️  Warning: No engine.env found. Engine may not work correctly."
fi

mkdir -p "$LOGS_DIR"

start() {
    if [ -f "$PID_FILE" ] && kill -0 $(cat "$PID_FILE") 2>/dev/null; then
        echo "Engine already running (PID $(cat $PID_FILE))"
        exit 1
    fi

    if [ ! -f "$ENGINE_BIN" ]; then
        echo "❌ Engine binary not found: $ENGINE_BIN"
        exit 1
    fi

    echo "Starting Alice engine..."
    nohup "$ENGINE_BIN" "$INSTANCES_DIR" "$LOGS_DIR" "$WEB_PORT" "$WEB_DIR" >> "$LOG_FILE" 2>&1 &
    echo $! > "$PID_FILE"
    sleep 1

    if kill -0 $(cat "$PID_FILE") 2>/dev/null; then
        echo "✅ Started (PID $!)"
    else
        echo "❌ Failed to start. Check logs: $LOG_FILE"
        rm -f "$PID_FILE"
        exit 1
    fi
}

stop() {
    if [ ! -f "$PID_FILE" ] || ! kill -0 $(cat "$PID_FILE") 2>/dev/null; then
        echo "Not running."
        rm -f "$PID_FILE" 2>/dev/null
        return 0
    fi

    local pid=$(cat "$PID_FILE")
    echo "Stopping engine (PID $pid) via graceful shutdown signal..."

    # Write shutdown signal file (engine checks every 3s)
    echo "shutdown" > /var/run/alice-engine-shutdown.signal

    # Wait for graceful exit (up to 15s)
    for i in {1..15}; do
        if ! kill -0 "$pid" 2>/dev/null; then
            echo "✅ Gracefully stopped after ${i}s."
            rm -f "$PID_FILE"
            return 0
        fi
        sleep 1
    done

    # Fallback: force kill
    echo "⚠️  Graceful shutdown timed out, force killing..."
    kill -9 "$pid" 2>/dev/null
    sleep 1
    rm -f "$PID_FILE"
    echo "✅ Force stopped."
}

status() {
    if [ -f "$PID_FILE" ] && kill -0 $(cat "$PID_FILE") 2>/dev/null; then
        echo "✅ Running (PID $(cat $PID_FILE))"
    else
        echo "⭕ Not running."
        [ -f "$PID_FILE" ] && rm -f "$PID_FILE"
    fi
}

logs() {
    if [ -f "$LOG_FILE" ]; then
        tail -50 "$LOG_FILE"
    else
        echo "No log file found."
    fi
}

tail_logs() {
    if [ -f "$LOG_FILE" ]; then
        tail -f "$LOG_FILE"
    else
        echo "No log file found."
    fi
}

case "$1" in
    start)   start ;;
    stop)    stop ;;
    restart) stop; sleep 2; start ;;
    status)  status ;;
    logs)    logs ;;
    tail)    tail_logs ;;
    *)
        echo "Alice Engine — 运维脚本"
        echo "Usage: $0 {start|stop|restart|status|logs|tail}"
        ;;
esac