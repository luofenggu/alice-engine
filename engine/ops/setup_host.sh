#!/bin/bash
# Alice 专机初始化脚本
# 用法: bash setup_host.sh <user_id> <host_ip> <root_password> <auth_secret> <api_key> [--custom-root-pass]
# 从打包机执行，SSH到目标机完成全部初始化
#
# 示例:
#   bash setup_host.sh user1 203.0.113.10 your-root-password your-auth-secret sk-your-api-key
#   bash setup_host.sh user2 203.0.113.20 your-secret your-secret sk-your-api-key

set -e

USER_ID="$1"
HOST_IP="$2"
ROOT_PASS="$3"
AUTH_SECRET="$4"
API_KEY="$5"

if [ -z "$USER_ID" ] || [ -z "$HOST_IP" ] || [ -z "$ROOT_PASS" ] || [ -z "$AUTH_SECRET" ] || [ -z "$API_KEY" ]; then
    echo "Usage: bash setup_host.sh <user_id> <host_ip> <root_password> <auth_secret> <api_key>"
    echo "Example: bash setup_host.sh user1 203.0.113.10 your-root-password your-auth-secret sk-your-api-key"
    exit 1
fi

DOMAIN="${USER_ID}.example.com"
PACKAGER_IP="YOUR_SERVER_IP"
PACKAGER_PRIVATE_IP="172.31.122.174"

# 检测是否私网专机（同VPC）
IS_PRIVATE=false
if [[ "$HOST_IP" == 172.31.* ]]; then
    IS_PRIVATE=true
fi

if $IS_PRIVATE; then
    UPDATE_URL="http://${PACKAGER_PRIVATE_IP}/release"
    REPORT_HOST="$PACKAGER_PRIVATE_IP"
    MODEL="http://${PACKAGER_PRIVATE_IP}/zenmux-proxy/v1/chat/completions@anthropic/claude-opus-4.6"
else
    UPDATE_URL="https://${PACKAGER_IP}/release"
    REPORT_HOST="$PACKAGER_IP"
    MODEL="zenmux@anthropic/claude-opus-4.6"
fi

SSH_OPTS="-o StrictHostKeyChecking=no -o ConnectTimeout=10"

echo "============================================"
echo "  Alice Host Setup: ${USER_ID}"
echo "  Target: ${HOST_IP} (${DOMAIN})"
echo "  Private: ${IS_PRIVATE}"
echo "============================================"

# Step 1: SSH connectivity test
echo ""
echo "[1/7] Testing SSH connectivity..."
sshpass -p "$ROOT_PASS" ssh $SSH_OPTS root@${HOST_IP} "echo 'SSH OK: $(hostname)'" || {
    echo "ERROR: SSH connection failed. Check IP/password."
    exit 1
}

# Step 2: Initialize environment
echo ""
echo "[2/7] Initializing environment..."
sshpass -p "$ROOT_PASS" ssh $SSH_OPTS root@${HOST_IP} bash -s << INIT
set -e
mkdir -p /opt/alice/{engine,instances,logs,web,ops,ssl}
yum install -y nginx sshpass 2>&1 | tail -3
id agent-alice 2>/dev/null || useradd -r -s /bin/bash agent-alice
echo "Environment: OK"
INIT

# Step 3: Generate self-signed cert
echo ""
echo "[3/7] Generating self-signed certificate..."
sshpass -p "$ROOT_PASS" ssh $SSH_OPTS root@${HOST_IP} bash -s << CERT
openssl req -x509 -nodes -days 3650 -newkey rsa:2048 \
  -keyout /opt/alice/ssl/selfsigned.key \
  -out /opt/alice/ssl/selfsigned.crt \
  -subj "/CN=${HOST_IP}" \
  -addext "subjectAltName=IP:${HOST_IP},DNS:${DOMAIN}" 2>/dev/null
echo "Certificate: OK"
CERT

# Step 4: SCP ops scripts
echo ""
echo "[4/7] Copying ops scripts..."
sshpass -p "$ROOT_PASS" scp $SSH_OPTS /opt/alice/ops/engine.sh root@${HOST_IP}:/opt/alice/ops/engine.sh
sshpass -p "$ROOT_PASS" scp $SSH_OPTS /opt/alice/release/updater.sh root@${HOST_IP}:/opt/alice/ops/updater.sh
echo "Scripts: OK"

# Step 5: Configure engine.env + Nginx + systemd + updater timer
echo ""
echo "[5/7] Configuring services..."
sshpass -p "$ROOT_PASS" ssh $SSH_OPTS root@${HOST_IP} bash -s << CONFIG
set -e

# engine.env
cat > /opt/alice/ops/engine.env << ENV
ALICE_USER_ID=${USER_ID}
ALICE_AUTH_SECRET=${AUTH_SECRET}
ALICE_DEFAULT_MODEL=${MODEL}
ALICE_DEFAULT_API_KEY=${API_KEY}
ALICE_HOST=${HOST_IP}:8081
ALICE_UPDATE_URL=${UPDATE_URL}
ALICE_REPORT_HOST=${REPORT_HOST}
ENV
echo "engine.env: OK"

# Nginx config
if $IS_PRIVATE; then
# Private: HTTP only (no HTTPS needed)
cat > /etc/nginx/conf.d/alice.conf << 'NGINX'
server {
    listen 80 default_server;
    server_name _;
    location = / { root /opt/alice/web; try_files /index.html =404; }
    location = /login { root /opt/alice/web; try_files /login.html =404; }
    location = /backup { root /opt/alice/web; try_files /backup.html =404; }
    location = /index { root /opt/alice/web; try_files /index.html =404; }
    location / {
        proxy_pass http://127.0.0.1:8081;
        proxy_set_header Host \$host;
        proxy_set_header X-Real-IP \$remote_addr;
        proxy_set_header X-Forwarded-For \$proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto http;
        proxy_http_version 1.1;
        proxy_buffering off;
        proxy_read_timeout 300s;
        proxy_send_timeout 300s;
    }
}
NGINX
else
# Public: HTTP→HTTPS redirect + HTTPS
cat > /etc/nginx/conf.d/alice.conf << 'NGINX'
server {
    listen 80 default_server;
    server_name _;
    return 301 https://\$host\$request_uri;
}
server {
    listen 443 ssl default_server;
    server_name _;
    ssl_certificate /opt/alice/ssl/selfsigned.crt;
    ssl_certificate_key /opt/alice/ssl/selfsigned.key;
    ssl_protocols TLSv1.2 TLSv1.3;
    ssl_ciphers HIGH:!aNULL:!MD5;
    location = / { root /opt/alice/web; try_files /index.html =404; }
    location = /login { root /opt/alice/web; try_files /login.html =404; }
    location = /backup { root /opt/alice/web; try_files /backup.html =404; }
    location = /index { root /opt/alice/web; try_files /index.html =404; }
    location / {
        proxy_pass http://127.0.0.1:8081;
        proxy_set_header Host \$host;
        proxy_set_header X-Real-IP \$remote_addr;
        proxy_set_header X-Forwarded-For \$proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto https;
        proxy_http_version 1.1;
        proxy_buffering off;
        proxy_read_timeout 300s;
        proxy_send_timeout 300s;
    }
}
NGINX
fi
nginx -t && systemctl enable nginx && systemctl restart nginx
echo "Nginx: OK"

# systemd service
cat > /etc/systemd/system/alice-engine.service << 'SVC'
[Unit]
Description=Alice Engine (Rust)
After=network.target
StartLimitIntervalSec=300
StartLimitBurst=10

[Service]
Type=simple
ExecStart=/opt/alice/engine/alice-engine /opt/alice/instances /opt/alice/logs 8081 /opt/alice/web
WorkingDirectory=/opt/alice/engine
Restart=always
RestartSec=5
StandardOutput=append:/opt/alice/logs/engine.log
StandardError=append:/opt/alice/logs/engine.log
EnvironmentFile=-/opt/alice/ops/engine.env

[Install]
WantedBy=multi-user.target
SVC
systemctl daemon-reload && systemctl enable alice-engine
echo "systemd: OK"

# updater timer
cat > /etc/systemd/system/alice-updater.service << 'USVC'
[Unit]
Description=Alice Engine Updater

[Service]
Type=oneshot
ExecStart=/bin/bash /opt/alice/ops/updater.sh
EnvironmentFile=-/opt/alice/ops/engine.env
USVC

cat > /etc/systemd/system/alice-updater.timer << 'UTMR'
[Unit]
Description=Alice Engine Updater Timer

[Timer]
OnCalendar=hourly
RandomizedDelaySec=600

[Install]
WantedBy=timers.target
UTMR
systemctl daemon-reload && systemctl enable alice-updater.timer && systemctl start alice-updater.timer
echo "Timer: OK"

CONFIG

# Step 6: Trigger updater (background, don't wait)
echo ""
echo "[6/7] Triggering updater (background)..."
sshpass -p "$ROOT_PASS" ssh $SSH_OPTS root@${HOST_IP} "nohup bash /opt/alice/ops/updater.sh > /opt/alice/logs/updater-init.log 2>&1 &"
echo "Updater triggered in background. Check updater-reports.log later."

# Step 7: Summary
echo ""
echo "[7/7] Setup complete!"
echo "============================================"
echo "  User: ${USER_ID}"
echo "  Host: ${HOST_IP}"
echo "  Domain: ${DOMAIN}"
echo "  URL: https://${DOMAIN}"
echo ""
echo "  Next steps:"
echo "  1. Wait for updater to download binary (~1min)"
echo "  2. Start engine: ssh root@${HOST_IP} systemctl start alice-engine"
echo "  3. Add server block to dev machine Nginx"
echo "  4. Ensure security group allows TCP 80/443"
echo "============================================"
