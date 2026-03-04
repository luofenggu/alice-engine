#!/bin/bash
# Self-Deploy Script for Alice Dev Engine
# 自己给自己做手术：编译 → 后台切换 → 新引擎接管
#
# Usage: bash /data/alice-dev/scripts/self-deploy.sh

set -euo pipefail

PROJECT_DIR="/data/alice-dev"
CARGO_TARGET="/data/rust-target-dev"
ENGINE_BIN="$CARGO_TARGET/release/alice-engine"
DEPLOY_LOG="/data/alice-dev-runtime/logs/self-deploy.log"

# 环境变量（硬编码，免得记的累）
export ALICE_HTTP_PORT=9527
export ALICE_HTML_DIR=/data/alice-dev/html-frontend
export ALICE_INSTANCES_DIR=/data/alice-dev-runtime/instances
export ALICE_LOGS_DIR=/data/alice-dev-runtime/logs
export ALICE_AUTH_SECRET=uk100777
export ALICE_USER_ID=24007
export ALICE_DEFAULT_MODEL="zenmux@anthropic/claude-opus-4.6"
export ALICE_DEFAULT_API_KEY="sk-ss-v1-14bc674abc4b2558d23ecb259ec933393681dc014a7edbc5f8ed3f99402d52a9"
export ALICE_PID_FILE=/var/run/alice-engine-dev.pid
export ALICE_INFER_LOG_IN=true
export ALICE_INFER_LOG_RETENTION_DAYS=365
export ALICE_HOST=47.77.237.69:9527
export ALICE_SHELL_ENV="Linux系统（Alibaba Cloud Linux 3），请生成bash脚本"

echo "[DEPLOY] $(date '+%Y-%m-%d %H:%M:%S') Starting self-deploy..." | tee -a "$DEPLOY_LOG"

if [ ! -f "$ENGINE_BIN" ]; then
    echo "[DEPLOY] ERROR: Binary not found! Run 'cargo build --release' first." | tee -a "$DEPLOY_LOG"
    exit 1
fi

# === Step 1: Get current engine PID ===
OLD_PID=$(pgrep -f "$ENGINE_BIN" | head -1)
if [ -z "$OLD_PID" ]; then
    echo "[DEPLOY] WARNING: No running engine found, starting fresh." | tee -a "$DEPLOY_LOG"
    # 直接启动
    nohup "$ENGINE_BIN" >> "$DEPLOY_LOG" 2>&1 &
    echo "[DEPLOY] Engine started (PID: $!)." | tee -a "$DEPLOY_LOG"
    exit 0
fi

echo "[DEPLOY] Current engine PID: $OLD_PID" | tee -a "$DEPLOY_LOG"

# === Step 2: Background switchover ===
# Fork到后台执行切换，主脚本立即返回（让script action完成）
nohup bash -c "
    DEPLOY_LOG='$DEPLOY_LOG'
    ENGINE_BIN='$ENGINE_BIN'
    OLD_PID='$OLD_PID'
    
    echo '[DEPLOY] Switchover: waiting 3s for current beat to finish...' >> \"\$DEPLOY_LOG\"
    sleep 3
    
    echo '[DEPLOY] Switchover: sending SIGTERM to PID \$OLD_PID...' >> \"\$DEPLOY_LOG\"
    kill \$OLD_PID 2>/dev/null || true
    
    # 等旧进程退出（最多等15秒）
    for i in \$(seq 1 30); do
        if ! kill -0 \$OLD_PID 2>/dev/null; then
            echo '[DEPLOY] Switchover: old engine stopped.' >> \"\$DEPLOY_LOG\"
            break
        fi
        sleep 0.5
    done
    
    # 确保端口释放
    sleep 1
    
    echo '[DEPLOY] Switchover: starting new engine...' >> \"\$DEPLOY_LOG\"
    nohup \"\$ENGINE_BIN\" >> \"\$DEPLOY_LOG\" 2>&1 &
    NEW_PID=\$!
    echo \"[DEPLOY] Switchover: new engine started (PID: \$NEW_PID).\" >> \"\$DEPLOY_LOG\"
    
    # 等新引擎就绪（最多等20秒）
    for i in \$(seq 1 40); do
        if curl -s 'http://127.0.0.1:9527/login' >/dev/null 2>&1; then
            echo '[DEPLOY] Switchover: new engine is ready! Deploy complete.' >> \"\$DEPLOY_LOG\"
            exit 0
        fi
        sleep 0.5
    done
    
    echo '[DEPLOY] WARNING: new engine did not respond within 20s!' >> \"\$DEPLOY_LOG\"
" >> "$DEPLOY_LOG" 2>&1 &

echo "[DEPLOY] Switchover scheduled in background (PID: $!)." | tee -a "$DEPLOY_LOG"
echo "[DEPLOY] Current engine will be stopped in ~3 seconds." | tee -a "$DEPLOY_LOG"
