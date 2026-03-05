#!/bin/bash
# Alice Engine Auto-Updater with Report
# 由 systemd timer 定期触发

LOG="/opt/alice/logs/updater.log"
RELEASE_BASE="${ALICE_UPDATE_URL:-http://YOUR_SERVER_IP/release}"
ENGINE_PATH="/opt/alice/engine/alice-engine"
WEB_PATH="/opt/alice/web"
TMP_DIR="/tmp/alice-update"
HOSTNAME=$(hostname)
REPORT_HOST="${ALICE_REPORT_HOST:-YOUR_SERVER_IP}"

log() {
    echo "[$(date '+%Y-%m-%d %H:%M:%S')] $1" >> "$LOG"
}

report() {
    # 汇报状态到打包机
    local status="$1"
    local ts=$(date '+%Y%m%d_%H%M%S')
    local engine_md5=$(md5sum "$ENGINE_PATH" 2>/dev/null | awk '{print $1}' | cut -c1-8)
    local engine_status=$(systemctl is-active alice-engine 2>/dev/null || echo "unknown")
    
    # 收集env配置（API_KEY脱敏）
    local env_line=""
    # 查找engine.env（优先根目录，fallback到ops目录）
    local env_file=""
    if [ -f /opt/alice/engine.env ]; then
        env_file="/opt/alice/engine.env"
    elif [ -f /opt/alice/ops/engine.env ]; then
        env_file="/opt/alice/ops/engine.env"
    fi
    if [ -n "$env_file" ]; then
        local uid=$(grep '^ALICE_USER_ID=' "$env_file" | cut -d= -f2 | tr -d '"')
        local auth=$(grep '^ALICE_AUTH_SECRET=' "$env_file" | cut -d= -f2 | tr -d '"')
        local model=$(grep '^ALICE_DEFAULT_MODEL=' "$env_file" | cut -d= -f2 | tr -d '"')
        local host=$(grep '^ALICE_HOST=' "$env_file" | cut -d= -f2 | tr -d '"')
        local apikey=$(grep '^ALICE_DEFAULT_API_KEY=' "$env_file" | cut -d= -f2 | tr -d '"')
        # API_KEY只显示前6位
        local apikey_masked="${apikey:0:6}***"
        # AUTH只显示前3位
        local auth_masked="${auth:0:3}***"
        env_line="USER_ID=$uid AUTH=$auth_masked MODEL=$model HOST=$host KEY=$apikey_masked"
    fi
    
    # 收集系统状态
    local disk_pct=$(df /opt/alice --output=pcent 2>/dev/null | tail -1 | tr -d ' ')
    local mem_info=$(free -m 2>/dev/null | awk '/Mem:/{printf "%dM/%dM", $3, $2}')
    local load_avg=$(cat /proc/loadavg 2>/dev/null | awk '{print $1}')
    local up=$(uptime -p 2>/dev/null | sed 's/up //')
    local sys_line="disk=$disk_pct mem=$mem_info load=$load_avg up=$up"
    
    # 组装多行报告块
    local report_block="===== $HOSTNAME $ts =====
status: md5=${engine_md5:-NONE} engine=$engine_status $status
env: $env_line
sys: $sys_line
========================================"
    
    # SSH到打包机追加日志（静默失败不影响主流程）
    sshpass -p "${ALICE_REPORT_PASS:-YOUR_PASSWORD}" ssh -o StrictHostKeyChecking=no -o ConnectTimeout=5 \
        root@$REPORT_HOST "cat >> /opt/alice/logs/updater-reports.log << 'REPORT_EOF'
$report_block
REPORT_EOF" 2>/dev/null
}

# --- Self-update updater.sh ---
if [ "$1" != "--self-updated" ]; then
    REMOTE_UPDATER=$(curl -sfk --connect-timeout 10 --max-time 30 "$RELEASE_BASE/updater.sh" 2>/dev/null)
    if [ -n "$REMOTE_UPDATER" ]; then
        LOCAL_UPDATER=$(cat "$0" 2>/dev/null)
        if [ "$REMOTE_UPDATER" != "$LOCAL_UPDATER" ]; then
            log "Updater self-update detected, replacing..."
            echo "$REMOTE_UPDATER" > "$0"
            chmod 755 "$0"
            log "Updater self-updated, re-executing..."
            exec "$0" --self-updated
        fi
    fi
fi

# Safety: refuse to run from within alice-engine process (agent script)
if [ "$ALICE_ENGINE_CHILD" = "1" ]; then
    echo "ERROR: updater.sh cannot be called from within alice-engine (agent script). Use systemd timer."
    log "BLOCKED: updater called from within alice-engine process"
    exit 1
fi

mkdir -p "$TMP_DIR"

# --- 检查引擎更新 ---
REMOTE_VERSION=$(curl -sfk --connect-timeout 10 --max-time 30 "$RELEASE_BASE/version" 2>/dev/null)
if [ -z "$REMOTE_VERSION" ]; then
    log "ERROR: Failed to fetch remote version from $RELEASE_BASE/version"
    report "ERROR:fetch_version_failed"
    exit 1
fi

LOCAL_VERSION=$(md5sum "$ENGINE_PATH" 2>/dev/null | awk '{print $1}')
if [ -z "$LOCAL_VERSION" ]; then
    log "INFO: Local engine binary not found, will download"
    
    LOCAL_VERSION="NONE"
fi

ENGINE_UPDATED="no"
if [ "$REMOTE_VERSION" = "$LOCAL_VERSION" ]; then
    log "Engine up-to-date (MD5: ${LOCAL_VERSION:0:8}...)"
else
    log "Engine update available: local=${LOCAL_VERSION:0:8}... remote=${REMOTE_VERSION:0:8}..."
    
    curl -sfk --connect-timeout 10 --max-time 300 -o "$TMP_DIR/alice-engine" "$RELEASE_BASE/alice-engine"
    if [ $? -ne 0 ]; then
        log "ERROR: Failed to download new engine binary"
        report "ERROR:download_failed"
        rm -f "$TMP_DIR/alice-engine"
        exit 1
    fi
    
    DL_MD5=$(md5sum "$TMP_DIR/alice-engine" | awk '{print $1}')
    if [ "$DL_MD5" != "$REMOTE_VERSION" ]; then
        log "ERROR: Downloaded binary MD5 mismatch (expected=$REMOTE_VERSION got=$DL_MD5)"
        report "ERROR:md5_mismatch"
        rm -f "$TMP_DIR/alice-engine"
        exit 1
    fi
    
    chmod 755 "$TMP_DIR/alice-engine"
    
    log "Stopping engine for update..."
    systemctl stop alice-engine
    sleep 1
    if pgrep -f alice-engine > /dev/null 2>&1; then
        pkill -9 -f alice-engine
        sleep 1
    fi
    cp "$TMP_DIR/alice-engine" "$ENGINE_PATH"
    systemctl start alice-engine
    sleep 2
    
    if systemctl is-active --quiet alice-engine; then
        log "Engine updated successfully (MD5: ${REMOTE_VERSION:0:8}...)"
        ENGINE_UPDATED="yes"
    else
        log "ERROR: Engine failed to start after update!"
        report "ERROR:start_failed_after_update"
        rm -f "$TMP_DIR/alice-engine"
        exit 1
    fi
    
    rm -f "$TMP_DIR/alice-engine"
fi

# --- 检查前端更新 ---
WEB_UPDATED="no"
REMOTE_WEB_VERSION=$(curl -sfk --connect-timeout 10 --max-time 30 "$RELEASE_BASE/web-version" 2>/dev/null)
if [ -n "$REMOTE_WEB_VERSION" ]; then
    LOCAL_WEB_VERSION=$(cd "$WEB_PATH" && cat filelist.txt 2>/dev/null | while read f; do md5sum "$f" 2>/dev/null; done | sort | md5sum | awk '{print $1}')
    
    if [ "$REMOTE_WEB_VERSION" != "$LOCAL_WEB_VERSION" ]; then
        log "Frontend update available, downloading..."
        
        # Download file list, then download each file
        FILELIST=$(curl -sfk --connect-timeout 10 --max-time 30 "$RELEASE_BASE/web/filelist.txt" 2>/dev/null)
        if [ -n "$FILELIST" ]; then
            for f in $FILELIST; do
                curl -sfk --connect-timeout 10 --max-time 60 -o "$TMP_DIR/$f" "$RELEASE_BASE/web/$f"
                if [ $? -eq 0 ]; then
                    cp "$TMP_DIR/$f" "$WEB_PATH/$f"
                else
                    log "WARNING: Failed to download $f"
                fi
            done
        else
            log "WARNING: Failed to download web filelist"
        fi
        # Save filelist.txt to local web dir (needed for web-version MD5 match)
        curl -sfk --connect-timeout 10 --max-time 30 -o "$WEB_PATH/filelist.txt" "$RELEASE_BASE/web/filelist.txt" 2>/dev/null
        
        log "Frontend updated"
        WEB_UPDATED="yes"
        rm -f "$TMP_DIR"/*.html
    else
        log "Frontend up-to-date"
    fi
fi

# 汇报
if [ "$ENGINE_UPDATED" = "yes" ] || [ "$WEB_UPDATED" = "yes" ]; then
    report "UPDATED engine=$ENGINE_UPDATED web=$WEB_UPDATED"
else
    report "OK"
fi

rmdir "$TMP_DIR" 2>/dev/null

