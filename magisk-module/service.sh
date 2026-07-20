#!/system/bin/sh

MODDIR=${0%/*}
F50MGR_DATA_DIR=/data/adb/f50mgr
F50MGR_BIN="$F50MGR_DATA_DIR/bin/f50mgr"

echo "f50mgr: waiting for bootanim stopped"
until [ "$(getprop init.svc.bootanim)" = "stopped" ]; do
    sleep 5
done

mkdir -p "$F50MGR_DATA_DIR/bin" "$F50MGR_DATA_DIR/run"

if [ ! -x "$F50MGR_BIN" ] && [ -x "$MODDIR/bin/f50mgr" ]; then
    cp "$MODDIR/bin/f50mgr" "$F50MGR_BIN"
    chmod 755 "$F50MGR_BIN"
fi

echo "f50mgr: starting daemon"
# 重启循环：daemon 一旦死亡（panic/OOM/异常退出），TPROXY 规则若残留会黑洞全部 LAN
# 流量且无人恢复。daemon 自身退出时会撤规则（fail-open），这里负责把它重新拉起。
while :; do
    if [ -e "$MODDIR/disable" ]; then
        echo "f50mgr: module disabled, stop restart loop"
        exit 0
    fi
    "$F50MGR_BIN" daemon
    rc=$?  # 先存：$(date) 会覆盖 $?
    # daemon 自身日志已落盘 run/f50mgr.log；这里补记退出码（panic/OOM 时
    # daemon 来不及写日志，这行是唯一死因线索）
    echo "$(date '+%Y-%m-%d %H:%M:%S') f50mgr: daemon exited rc=$rc, restarting in 5s" \
        >> "$F50MGR_DATA_DIR/run/f50mgr.log"
    sleep 5
done
