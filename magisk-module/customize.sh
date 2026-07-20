#!/system/bin/sh

F50MGR_DATA_DIR=/data/adb/f50mgr
F50MGR_BIN="$F50MGR_DATA_DIR/bin/f50mgr"

if [ ! -f "$MODPATH/easytier.sha256" ] \
    || [ ! -f "$MODPATH/bin/easytier.fallback" ] \
    || [ ! -f "$MODPATH/bin/easytier-cli.fallback" ]; then
    abort "f50mgr: EasyTier v2.4.5 core/CLI 不完整，拒绝安装"
fi
if ! (cd "$MODPATH" && sha256sum -c easytier.sha256 >/dev/null 2>&1); then
    abort "f50mgr: EasyTier v2.4.5 core/CLI 哈希不匹配，拒绝安装"
fi

ui_print "f50mgr: preparing runtime directories"
rm -rf "$F50MGR_DATA_DIR/bin" 2>/dev/null || true
mkdir -p \
    "$F50MGR_DATA_DIR/bin" \
    "$F50MGR_DATA_DIR/config/clash" \
    "$F50MGR_DATA_DIR/config/easytier" \
    "$F50MGR_DATA_DIR/data" \
    "$F50MGR_DATA_DIR/run"

chcon u:object_r:adb_data_file:s0 "$F50MGR_DATA_DIR" "$F50MGR_DATA_DIR/bin" 2>/dev/null || true

if [ -f "$MODPATH/bin/f50mgr" ]; then
    rm -f "$F50MGR_DATA_DIR/bin/f50mgr" 2>/dev/null || true
    cp "$MODPATH/bin/f50mgr" "$F50MGR_DATA_DIR/bin/f50mgr"
    chcon u:object_r:adb_data_file:s0 "$F50MGR_DATA_DIR/bin/f50mgr" 2>/dev/null || true
    chown root:root "$F50MGR_DATA_DIR/bin/f50mgr" 2>/dev/null || true
    chmod 755 "$F50MGR_DATA_DIR/bin/f50mgr"
fi

if [ ! -x "$F50MGR_DATA_DIR/bin/f50mgr" ]; then
    abort "f50mgr: 管理程序缺失，无法初始化管理密钥"
fi
AUTH_RESULT="$($F50MGR_BIN auth ensure 2>&1)" \
    || abort "f50mgr: 无法初始化管理密钥：$AUTH_RESULT"
case "$AUTH_RESULT" in
    created=*)
        AUTH_SECRET="${AUTH_RESULT#created=}"
        ui_print "f50mgr: 首次启用管理认证，请立即保存以下密钥"
        ui_print "f50mgr: 管理密钥 $AUTH_SECRET"
        ui_print "f50mgr: 遗失后使用 root 命令 f50mgr auth reset 生成新密钥"
        ;;
    existing)
        ui_print "f50mgr: 已保留现有管理密钥并确认权限为 0600"
        ;;
    *)
        abort "f50mgr: 管理密钥初始化返回未知结果"
        ;;
esac

if [ -f "$MODPATH/bin/mihomo.fallback" ]; then
    rm -f "$F50MGR_DATA_DIR/bin/mihomo" 2>/dev/null || true
    cp "$MODPATH/bin/mihomo.fallback" "$F50MGR_DATA_DIR/bin/mihomo"
    chcon u:object_r:adb_data_file:s0 "$F50MGR_DATA_DIR/bin/mihomo" 2>/dev/null || true
    chown root:net_admin "$F50MGR_DATA_DIR/bin/mihomo" 2>/dev/null || true
    chmod 0755 "$F50MGR_DATA_DIR/bin/mihomo"
fi

ui_print "f50mgr: 安装 EasyTier v2.4.5（Android 数据面稳定版）"
cp "$MODPATH/bin/easytier.fallback" "$F50MGR_DATA_DIR/bin/easytier"
cp "$MODPATH/bin/easytier-cli.fallback" "$F50MGR_DATA_DIR/bin/easytier-cli"
chcon u:object_r:adb_data_file:s0 "$F50MGR_DATA_DIR/bin/easytier" "$F50MGR_DATA_DIR/bin/easytier-cli" 2>/dev/null || true
chown root:net_admin "$F50MGR_DATA_DIR/bin/easytier" "$F50MGR_DATA_DIR/bin/easytier-cli" 2>/dev/null || true
chmod 0755 "$F50MGR_DATA_DIR/bin/easytier"
chmod 755 "$F50MGR_DATA_DIR/bin/easytier-cli"

if [ -f "$MODPATH/assets/zashboard.tar.gz" ]; then
    # 历史文件名保留以兼容旧安装包结构，内容已固定为 MetaCubeXD 静态资源。
    ui_print "f50mgr: 铺设 Mihomo 管理面板（MetaCubeXD）"
    # 必须放在 mihomo 工作目录（config/clash）下，否则 mihomo 拒绝 external-ui（subpath 检查）
    DASH_DIR="$F50MGR_DATA_DIR/config/clash/dashboard"
    rm -rf "$DASH_DIR" 2>/dev/null || true
    mkdir -p "$DASH_DIR"
    tar -xzf "$MODPATH/assets/zashboard.tar.gz" -C "$DASH_DIR" 2>/dev/null \
        && ui_print "f50mgr: 面板已铺设到 $DASH_DIR" \
        || ui_print "f50mgr: 面板解压失败（mihomo /ui/ 将 404，可重装修复）"
    chcon -R u:object_r:adb_data_file:s0 "$DASH_DIR" 2>/dev/null || true
    chmod -R 755 "$DASH_DIR" 2>/dev/null || true
fi

if [ ! -e /dev/net/tun ] && [ -e /dev/tun ]; then
    mkdir -p /dev/net 2>/dev/null || true
    ln -sf /dev/tun /dev/net/tun 2>/dev/null || true
    ui_print "f50mgr: linked /dev/tun -> /dev/net/tun (EasyTier 内核 TUN 路径)"
fi

if [ ! -f "$F50MGR_DATA_DIR/config/f50mgr.toml" ]; then
    cat >"$F50MGR_DATA_DIR/config/f50mgr.toml" <<'EOF'
[mihomo]
auto_start = true
version_check_interval = "6h"

[download]
github_timeout_secs = 30
# company_base_url = "https://updates.example.com/f50"

[web]
listen = "192.168.0.1:14180"

[iptables]
auto_enable = true
block_lan_ipv6 = true

[easytier]
enabled = false
EOF
    chmod 644 "$F50MGR_DATA_DIR/config/f50mgr.toml"
fi

if [ ! -f "$F50MGR_DATA_DIR/config/secrets.toml" ]; then
    cat >"$F50MGR_DATA_DIR/config/secrets.toml" <<'EOF'
[subscription]
url = ""

# 可选公司 Mihomo 更新源；仅在 f50mgr.toml 配置 company_base_url 时读取
# [download]
# username = "your-username"
# password = "your-password"

# EasyTier mesh VPN（默认未启用，填入下列字段并把 [easytier].enabled 设为 true 后生效）
# [easytier]
# network_name = "your-network"
# network_secret = "your-secret"
# ipv4 = "10.0.5.3"            # 每台 F50 唯一
# # instance_id = "..."        # 需稳定身份时填写；留空则每次渲染生成 UUID
# # hostname = "F50-custom"    # 留空则用 "F50-{IMEI}"
# peers = [
#   "tcp://relay.example.com:11010/",
#   "udp://relay.example.com:11010/",
# ]
EOF
    chmod 600 "$F50MGR_DATA_DIR/config/secrets.toml"
fi

chmod 755 "$F50MGR_DATA_DIR/bin" "$F50MGR_DATA_DIR/run"
ui_print "f50mgr: install preparation done"
