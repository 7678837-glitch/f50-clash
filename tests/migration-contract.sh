#!/usr/bin/env bash
set -euo pipefail

PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCRIPT="${PROJECT_ROOT}/magisk-module/migrate-box-for-root.sh"
TEST_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/f50mgr-migration-test.XXXXXX")"
cleanup() {
  for pid in $(jobs -pr); do
    kill "${pid}" 2>/dev/null || true
  done
  rm -rf "${TEST_ROOT}"
}
trap cleanup EXIT

ROOT="${TEST_ROOT}/root"
LOG="${TEST_ROOT}/actions.log"
mkdir -p \
    "${ROOT}/data/adb/box/scripts" \
    "${ROOT}/data/adb/box/clash" \
    "${ROOT}/data/adb/modules/box_for_root" \
    "${ROOT}/data/adb/modules/f50mgr" \
    "${ROOT}/data/adb/f50mgr/bin" \
    "${ROOT}/data/adb/f50mgr/config/clash" \
    "${TEST_ROOT}/proc" \
    "${TEST_ROOT}/bin"

printf 'id=box_for_root\nversion=v1.10.2\n' >"${ROOT}/data/adb/modules/box_for_root/module.prop"
printf 'proxies: []\n' >"${ROOT}/data/adb/box/clash/config.yaml"
: >"${LOG}"

cat >"${ROOT}/data/adb/box/scripts/box.iptables" <<'EOF'
#!/bin/sh
printf 'old-iptables:%s\n' "$1" >>"${F50MGR_TEST_LOG}"
if [ "$1" = "disable" ]; then
  [ "${F50MGR_TEST_FAIL_OLD_IPTABLES:-0}" != "1" ]
else
  [ "${F50MGR_TEST_FAIL_OLD_IPTABLES_ENABLE:-0}" != "1" ]
fi
EOF
cat >"${ROOT}/data/adb/box/scripts/box.service" <<'EOF'
#!/bin/sh
printf 'old-service:%s\n' "$1" >>"${F50MGR_TEST_LOG}"
if [ "$1" = "stop" ]; then
  [ "${F50MGR_TEST_FAIL_OLD_SERVICE:-0}" != "1" ]
else
  [ "${F50MGR_TEST_FAIL_OLD_SERVICE_START:-0}" != "1" ]
fi
EOF
cat >"${ROOT}/data/adb/f50mgr/bin/f50mgr" <<'EOF'
#!/bin/sh
printf 'f50mgr:%s\n' "$*" >>"${F50MGR_TEST_LOG}"
if [ "$1" = "status" ]; then
  printf 'daemon: running\nmihomo: running\n'
elif [ "$1 $2" = "subscription render" ]; then
  output=""
  while [ "$#" -gt 0 ]; do
    [ "$1" = "--output" ] && output="$2"
    shift
  done
  output="${F50MGR_ROOT}${output}"
  printf 'mixed-port: 14190\n' >"${output}"
elif [ "$1 $2" = "iptables status" ]; then
  printf 'iptables: active\n'
fi
EOF

cat >"${TEST_ROOT}/bin/iptables-save" <<'EOF'
#!/bin/sh
if [ "${F50MGR_TEST_BOX_RULES:-0}" = "1" ]; then
  printf '%s\n' '-N BOX_TEST'
else
  printf 'iptables-save snapshot\n'
fi
EOF
chmod 755 "${TEST_ROOT}/bin/iptables-save"

for command in ip6tables-save ip ps; do
  cat >"${TEST_ROOT}/bin/${command}" <<EOF
#!/bin/sh
printf '${command} snapshot\\n'
EOF
  chmod 755 "${TEST_ROOT}/bin/${command}"
done
chmod 755 \
    "${ROOT}/data/adb/box/scripts/box.iptables" \
    "${ROOT}/data/adb/box/scripts/box.service" \
    "${ROOT}/data/adb/f50mgr/bin/f50mgr"

export PATH="${TEST_ROOT}/bin:${PATH}"
export F50MGR_ROOT="${ROOT}"
export F50MGR_ALLOW_NON_ROOT=1
export F50MGR_TEST_LOG="${LOG}"
export F50MGR_SYSTEM_SH=sh
export F50MGR_PROC_ROOT="${TEST_ROOT}/proc"

run_migration() {
  sh "${SCRIPT}" "$@"
}

status_output="$(run_migration status)"
grep -Fq 'box_for_root: installed' <<<"${status_output}"
grep -Fq 'backup: missing' <<<"${status_output}"

if run_migration disable >/dev/null 2>&1; then
  echo 'error: 无备份时 disable 必须拒绝执行' >&2
  exit 1
fi
test ! -e "${ROOT}/data/adb/modules/box_for_root/disable"

F50MGR_NOW=20260718-000001 run_migration backup >/dev/null
RECEIPT="${ROOT}/data/adb/f50mgr/migration/latest"
test -f "${RECEIPT}"
ARCHIVE="$(sed -n 's/^archive=//p' "${RECEIPT}")"
test -f "${ARCHIVE}"
tar -tzf "${ARCHIVE}" | grep -Fxq 'data/adb/box/clash/config.yaml'
SNAPSHOT="${ARCHIVE%.tar.gz}.network.txt"
test -f "${SNAPSHOT}"
ARCHIVE_HASH_BEFORE_REPEAT="$(sha256sum "${ARCHIVE}" | awk '{print $1}')"
SHA_FILE_BEFORE_REPEAT="$(sha256sum "${ARCHIVE}.sha256" | awk '{print $1}')"
SNAPSHOT_HASH_BEFORE_REPEAT="$(sha256sum "${SNAPSHOT}" | awk '{print $1}')"
RECEIPT_HASH_BEFORE_REPEAT="$(sha256sum "${RECEIPT}" | awk '{print $1}')"
if F50MGR_NOW=20260718-000001 run_migration backup >/dev/null 2>&1; then
  echo 'error: 同一时间戳的重复 backup 必须拒绝执行' >&2
  exit 1
fi
test -f "${ARCHIVE}" -a -f "${ARCHIVE}.sha256" -a -f "${SNAPSHOT}" -a -f "${RECEIPT}"
test "$(sha256sum "${ARCHIVE}" | awk '{print $1}')" = "${ARCHIVE_HASH_BEFORE_REPEAT}"
test "$(sha256sum "${ARCHIVE}.sha256" | awk '{print $1}')" = "${SHA_FILE_BEFORE_REPEAT}"
test "$(sha256sum "${SNAPSHOT}" | awk '{print $1}')" = "${SNAPSHOT_HASH_BEFORE_REPEAT}"
test "$(sha256sum "${RECEIPT}" | awk '{print $1}')" = "${RECEIPT_HASH_BEFORE_REPEAT}"
grep -Fq "${ARCHIVE_HASH_BEFORE_REPEAT}" "${ARCHIVE}.sha256"

LOCK_DIR="${ROOT}/data/adb/f50mgr/migration/.lock"
mkdir "${LOCK_DIR}"
RECEIPT_BEFORE_LOCK="$(sha256sum "${RECEIPT}" | awk '{print $1}')"
LOG_SIZE_BEFORE_LOCK="$(wc -c <"${LOG}" 2>/dev/null || printf '0')"
for command in backup import-config disable restore; do
  if F50MGR_NOW=20260718-locked run_migration "${command}" >/dev/null 2>&1; then
    echo "error: 持有迁移锁时 ${command} 必须拒绝执行" >&2
    exit 1
  fi
done
grep -Fq 'box_for_root: installed' <<<"$(run_migration status)"
test "$(sha256sum "${RECEIPT}" | awk '{print $1}')" = "${RECEIPT_BEFORE_LOCK}"
test ! -e "${ROOT}/data/adb/f50mgr/config/clash/config.yaml"
test ! -e "${ROOT}/data/adb/modules/box_for_root/disable"
test ! -e "${ROOT}/data/adb/modules/f50mgr/disable"
test "$(wc -c <"${LOG}" 2>/dev/null || printf '0')" = "${LOG_SIZE_BEFORE_LOCK}"
test ! -e "${ROOT}/data/adb/f50mgr/migration/box-for-root-20260718-locked.tar.gz"
rmdir "${LOCK_DIR}"

cat >"${TEST_ROOT}/bin/tar-signal" <<'EOF'
#!/bin/sh
: >"$2"
kill -TERM "$PPID"
exit 0
EOF
chmod 755 "${TEST_ROOT}/bin/tar-signal"
set +e
F50MGR_TAR="${TEST_ROOT}/bin/tar-signal" F50MGR_NOW=20260718-signal \
  run_migration backup >/dev/null 2>&1
SIGNAL_RC=$?
set -e
test "${SIGNAL_RC}" = "143"
test ! -e "${LOCK_DIR}"
test ! -e "${ROOT}/data/adb/f50mgr/migration/box-for-root-20260718-signal.tar.gz"

printf 'tampered\n' >>"${ARCHIVE}"
grep -Fq 'backup: invalid' <<<"$(run_migration status)"
if run_migration import-config >/dev/null 2>&1; then
  echo 'error: 备份被篡改时 import-config 必须拒绝执行' >&2
  exit 1
fi
if run_migration disable >/dev/null 2>&1; then
  echo 'error: 备份被篡改时 disable 必须拒绝执行' >&2
  exit 1
fi

rm -f "${ROOT}/data/adb/f50mgr/config/clash/config.yaml"
F50MGR_NOW=20260718-000002 run_migration backup >/dev/null
run_migration import-config >/dev/null
grep -Fq 'mixed-port: 14190' "${ROOT}/data/adb/f50mgr/config/clash/config.yaml"
printf 'user-owned: true\n' >"${ROOT}/data/adb/f50mgr/config/clash/config.yaml"
if run_migration import-config >/dev/null 2>&1; then
  echo 'error: 已有不同配置时 import-config 必须拒绝覆盖' >&2
  exit 1
fi
grep -Fq 'user-owned: true' "${ROOT}/data/adb/f50mgr/config/clash/config.yaml"

mkdir -p "${ROOT}/data/adb/f50mgr/run"
OLD_ACTIONS_BEFORE="$(grep -Ec '^old-(service|iptables):' "${LOG}" || true)"
printf '999999\n' >"${ROOT}/data/adb/f50mgr/run/f50mgr.pid"
if run_migration disable >/dev/null 2>&1; then
  echo 'error: stale f50mgr PID 时 disable 必须拒绝执行' >&2
  exit 1
fi
test ! -e "${ROOT}/data/adb/modules/box_for_root/disable"
test "$(grep -Ec '^old-(service|iptables):' "${LOG}" || true)" = "${OLD_ACTIONS_BEFORE}"

sleep 30 &
WRONG_OWNER_PID=$!
mkdir -p "${F50MGR_PROC_ROOT}/${WRONG_OWNER_PID}"
printf '%s\0' '/system/bin/sleep' \
  >"${F50MGR_PROC_ROOT}/${WRONG_OWNER_PID}/cmdline"
printf '%s\n' "${WRONG_OWNER_PID}" >"${ROOT}/data/adb/f50mgr/run/f50mgr.pid"
if run_migration disable >/dev/null 2>&1; then
  echo 'error: PID 不属于 f50mgr 时 disable 必须拒绝执行' >&2
  exit 1
fi
test ! -e "${ROOT}/data/adb/modules/box_for_root/disable"
test "$(grep -Ec '^old-(service|iptables):' "${LOG}" || true)" = "${OLD_ACTIONS_BEFORE}"
kill "${WRONG_OWNER_PID}" 2>/dev/null || true

sleep 30 &
HEALTHY_DAEMON_PID=$!
mkdir -p "${F50MGR_PROC_ROOT}/${HEALTHY_DAEMON_PID}"
printf '%s\0' "${ROOT}/data/adb/f50mgr/bin/f50mgr" \
  >"${F50MGR_PROC_ROOT}/${HEALTHY_DAEMON_PID}/cmdline"
printf '%s\n' "${HEALTHY_DAEMON_PID}" >"${ROOT}/data/adb/f50mgr/run/f50mgr.pid"
sleep 30 &
SECOND_HEALTHY_PID=$!
mkdir -p "${F50MGR_PROC_ROOT}/${SECOND_HEALTHY_PID}"
printf '%s\0' "${ROOT}/data/adb/f50mgr/bin/f50mgr" \
  >"${F50MGR_PROC_ROOT}/${SECOND_HEALTHY_PID}/cmdline"
OLD_ACTIONS_BEFORE_DUPLICATE="$(grep -Ec '^old-(service|iptables):' "${LOG}" || true)"
if run_migration disable >/dev/null 2>&1; then
  echo 'error: 存在第二个 f50mgr daemon 时 disable 必须拒绝执行' >&2
  exit 1
fi
test ! -e "${ROOT}/data/adb/modules/box_for_root/disable"
test "$(grep -Ec '^old-(service|iptables):' "${LOG}" || true)" = "${OLD_ACTIONS_BEFORE_DUPLICATE}"
kill "${SECOND_HEALTHY_PID}" 2>/dev/null || true
wait "${SECOND_HEALTHY_PID}" 2>/dev/null || true
rm -rf "${F50MGR_PROC_ROOT:?}/${SECOND_HEALTHY_PID}"
run_migration disable >/dev/null
test -f "${ROOT}/data/adb/modules/box_for_root/disable"
test "$(sed -n '1p' "${LOG}")" = 'f50mgr:subscription render /data/adb/box/clash/config.yaml --output /data/adb/f50mgr/config/clash/config.yaml.tmp'
grep -Fq 'old-iptables:disable' "${LOG}"
grep -Fq 'old-service:stop' "${LOG}"
SERVICE_LINE="$(grep -n 'old-service:stop' "${LOG}" | tail -1 | cut -d: -f1)"
IPTABLES_LINE="$(grep -n 'old-iptables:disable' "${LOG}" | tail -1 | cut -d: -f1)"
if [ "${SERVICE_LINE}" -ge "${IPTABLES_LINE}" ]; then
  echo 'error: disable 必须先停止旧服务，再执行最终 iptables 清理' >&2
  exit 1
fi

rm -f "${ROOT}/data/adb/modules/box_for_root/disable"
if F50MGR_TEST_FAIL_OLD_SERVICE=1 run_migration disable >/dev/null 2>&1; then
  echo 'error: service stop 失败时 disable 必须返回失败' >&2
  exit 1
fi
test ! -e "${ROOT}/data/adb/modules/box_for_root/disable"

for failure in iptables residual; do
  rm -f "${ROOT}/data/adb/modules/box_for_root/disable"
  LOG_LINES_BEFORE="$(wc -l <"${LOG}")"
  if [ "${failure}" = "iptables" ]; then
    F50MGR_TEST_FAIL_OLD_IPTABLES=1 run_migration disable >/dev/null 2>&1 && failed=0 || failed=1
  else
    F50MGR_TEST_BOX_RULES=1 run_migration disable >/dev/null 2>&1 && failed=0 || failed=1
  fi
  test "${failed}" = "1"
  test ! -e "${ROOT}/data/adb/modules/box_for_root/disable"
  tail -n "+$((LOG_LINES_BEFORE + 1))" "${LOG}" | grep '^old-' >"${TEST_ROOT}/${failure}.actions"
  printf 'old-service:stop\nold-iptables:disable\nold-service:start\nold-iptables:enable\n' \
    | cmp -s - "${TEST_ROOT}/${failure}.actions"
done

rm -f "${ROOT}/data/adb/modules/box_for_root/disable"
if F50MGR_TEST_FAIL_OLD_IPTABLES=1 F50MGR_TEST_FAIL_OLD_SERVICE_START=1 \
  run_migration disable >/dev/null 2>&1; then
  echo 'error: 旧服务恢复失败时 disable 必须返回失败' >&2
  exit 1
fi
test -e "${ROOT}/data/adb/modules/box_for_root/disable"

rm -f "${ROOT}/data/adb/modules/box_for_root/disable"
LOG_LINES_BEFORE_ENABLE_FAILURE="$(wc -l <"${LOG}")"
if F50MGR_TEST_FAIL_OLD_IPTABLES=1 F50MGR_TEST_FAIL_OLD_IPTABLES_ENABLE=1 \
  run_migration disable >/dev/null 2>&1; then
  echo 'error: 旧规则恢复失败时 disable 必须返回失败' >&2
  exit 1
fi
test -e "${ROOT}/data/adb/modules/box_for_root/disable"
tail -n "+$((LOG_LINES_BEFORE_ENABLE_FAILURE + 1))" "${LOG}" | grep '^old-' \
  >"${TEST_ROOT}/enable-failure.actions"
printf 'old-service:stop\nold-iptables:disable\nold-service:start\nold-iptables:enable\nold-service:stop\n' \
  | cmp -s - "${TEST_ROOT}/enable-failure.actions"
kill "${HEALTHY_DAEMON_PID}" 2>/dev/null || true
rm -f "${ROOT}/data/adb/f50mgr/run/f50mgr.pid"

rm -rf "${ROOT}/data/adb/box" "${ROOT}/data/adb/modules/box_for_root"
mkdir -p "${ROOT}/data/adb/f50mgr/run"
rm -f "${ROOT}/data/adb/modules/f50mgr/disable"
sleep 30 &
DAEMON_PID=$!
mkdir -p "${F50MGR_PROC_ROOT}/${DAEMON_PID}"
printf '%s\0' "${ROOT}/data/adb/f50mgr/bin/f50mgr" \
  >"${F50MGR_PROC_ROOT}/${DAEMON_PID}/cmdline"
printf '%s\n' "${DAEMON_PID}" >"${ROOT}/data/adb/f50mgr/run/f50mgr.pid"
F50MGR_TEST_OBSERVE_DISABLE_BEFORE_SCAN=1 run_migration restore >/dev/null
if kill -0 "${DAEMON_PID}" 2>/dev/null; then
  echo 'error: restore 必须停止当前 f50mgr daemon' >&2
  exit 1
fi
test -f "${ROOT}/data/adb/box/clash/config.yaml"
test -f "${ROOT}/data/adb/modules/box_for_root/module.prop"
test ! -e "${ROOT}/data/adb/modules/box_for_root/disable"
test -f "${ROOT}/data/adb/modules/f50mgr/disable"
grep -Fq 'f50mgr:iptables disable' "${LOG}"
grep -Fq 'f50mgr:iptables v6-block-disable' "${LOG}"

rm -f "${ROOT}/data/adb/f50mgr/run/f50mgr.pid"
sleep 30 &
DISCOVERED_PID=$!
mkdir -p "${F50MGR_PROC_ROOT}/${DISCOVERED_PID}"
printf '%s\0' "${ROOT}/data/adb/f50mgr/bin/f50mgr" \
  >"${F50MGR_PROC_ROOT}/${DISCOVERED_PID}/cmdline"
run_migration restore >/dev/null
if kill -0 "${DISCOVERED_PID}" 2>/dev/null; then
  echo 'error: PID 文件缺失时 restore 必须发现并停止唯一的 f50mgr daemon' >&2
  exit 1
fi

sleep 30 &
FIRST_DUPLICATE_PID=$!
sleep 30 &
SECOND_DUPLICATE_PID=$!
for pid in "${FIRST_DUPLICATE_PID}" "${SECOND_DUPLICATE_PID}"; do
  mkdir -p "${F50MGR_PROC_ROOT}/${pid}"
  printf '%s\0' "${ROOT}/data/adb/f50mgr/bin/f50mgr" \
    >"${F50MGR_PROC_ROOT}/${pid}/cmdline"
done
touch "${ROOT}/data/adb/modules/box_for_root/remove"
rm -f "${ROOT}/data/adb/modules/f50mgr/disable"
F50_ACTIONS_BEFORE_DUPLICATE="$(grep -c '^f50mgr:iptables ' "${LOG}" || true)"
if run_migration restore >/dev/null 2>&1; then
  echo 'error: PID 文件缺失且发现多个 f50mgr daemon 时 restore 必须拒绝执行' >&2
  exit 1
fi
test -e "${ROOT}/data/adb/modules/box_for_root/remove"
test ! -e "${ROOT}/data/adb/modules/f50mgr/disable"
test "$(grep -c '^f50mgr:iptables ' "${LOG}" || true)" = "${F50_ACTIONS_BEFORE_DUPLICATE}"

touch "${ROOT}/data/adb/modules/f50mgr/disable"
if run_migration restore >/dev/null 2>&1; then
  echo 'error: 多实例失败时 restore 必须拒绝执行' >&2
  exit 1
fi
test -e "${ROOT}/data/adb/modules/f50mgr/disable"
rm -f "${ROOT}/data/adb/modules/f50mgr/disable"

printf '%s\n' "${FIRST_DUPLICATE_PID}" >"${ROOT}/data/adb/f50mgr/run/f50mgr.pid"
if run_migration restore >/dev/null 2>&1; then
  echo 'error: PID 文件存在但同时发现第二个 f50mgr daemon 时 restore 必须拒绝执行' >&2
  exit 1
fi
kill -0 "${FIRST_DUPLICATE_PID}" 2>/dev/null
kill -0 "${SECOND_DUPLICATE_PID}" 2>/dev/null
test -e "${ROOT}/data/adb/modules/box_for_root/remove"
test ! -e "${ROOT}/data/adb/modules/f50mgr/disable"
test "$(grep -c '^f50mgr:iptables ' "${LOG}" || true)" = "${F50_ACTIONS_BEFORE_DUPLICATE}"
rm -f "${ROOT}/data/adb/f50mgr/run/f50mgr.pid"
kill "${FIRST_DUPLICATE_PID}" "${SECOND_DUPLICATE_PID}" 2>/dev/null || true

run_migration restore >/dev/null
test -f "${ROOT}/data/adb/modules/f50mgr/disable"

NO_BOX_ROOT="${TEST_ROOT}/already-migrated"
mkdir -p "${NO_BOX_ROOT}/data/adb/modules/f50mgr"
F50MGR_ROOT="${NO_BOX_ROOT}" run_migration disable \
  | grep -Fq 'box_for_root: already absent'

echo '迁移契约测试通过'
