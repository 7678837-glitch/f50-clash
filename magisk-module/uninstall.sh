#!/system/bin/sh

F50MGR_DATA_DIR=/data/adb/f50mgr
F50MGR_BIN="$F50MGR_DATA_DIR/bin/f50mgr"

if [ -x "$F50MGR_BIN" ]; then
    "$F50MGR_BIN" iptables disable 2>/dev/null || true
    # v4 disable 不含 IPv6 leak guard，需单独清理，否则 ip6tables REJECT 链残留到重启
    "$F50MGR_BIN" iptables v6-block-disable 2>/dev/null || true
fi

for pid_name in f50mgr mihomo easytier; do
    pid_file="$F50MGR_DATA_DIR/run/$pid_name.pid"
    if [ -f "$pid_file" ]; then
        kill -TERM "$(cat "$pid_file")" 2>/dev/null || true
    fi
done

echo "f50mgr: runtime data kept at $F50MGR_DATA_DIR"
