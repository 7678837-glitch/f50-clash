#!/system/bin/sh
set -eu

ROOT="${F50MGR_ROOT:-}"
MIGRATION_DIR="${ROOT}/data/adb/f50mgr/migration"
BOX_DIR="${ROOT}/data/adb/box"
BOX_MODULE="${ROOT}/data/adb/modules/box_for_root"
F50_MODULE="${ROOT}/data/adb/modules/f50mgr"
F50_BIN="${ROOT}/data/adb/f50mgr/bin/f50mgr"
OLD_CONFIG="/data/adb/box/clash/config.yaml"
NEW_CONFIG="/data/adb/f50mgr/config/clash/config.yaml"
TAR="${F50MGR_TAR:-tar}"
SYSTEM_SH="${F50MGR_SYSTEM_SH:-/system/bin/sh}"
PROC_ROOT="${F50MGR_PROC_ROOT:-/proc}"
LOCK_DIR="${MIGRATION_DIR}/.lock"
LOCK_HELD=0
RESTORE_DISABLE_CREATED=0
BACKUP_CLEANUP_ACTIVE=0
IMPORT_CLEANUP_ACTIVE=0

fail() {
    echo "error: $*" >&2
    exit 1
}

release_migration_lock() {
    [ "${LOCK_HELD}" = "1" ] || return 0
    if ! rmdir "${LOCK_DIR}" 2>/dev/null; then
        echo "warning: 无法释放迁移锁，请人工核查: ${LOCK_DIR}" >&2
    fi
    LOCK_HELD=0
}

install_lock_traps() {
    trap 'release_migration_lock' EXIT
    trap 'release_migration_lock; exit 130' INT
    trap 'release_migration_lock; exit 143' TERM
}

acquire_migration_lock() {
    mkdir -p "${MIGRATION_DIR}"
    if ! mkdir "${LOCK_DIR}" 2>/dev/null; then
        echo "error: 迁移操作已被锁定: ${LOCK_DIR}" >&2
        echo "error: 请人工核查该目录及当前迁移进程；确认无进程后再处理锁目录" >&2
        return 1
    fi
    LOCK_HELD=1
    install_lock_traps
}

run_mutating_command() {
    acquire_migration_lock || exit 1
    "$@"
    release_migration_lock
    trap - EXIT INT TERM
}

rollback_restore_disable_marker() {
    [ "${RESTORE_DISABLE_CREATED}" = "1" ] || return 0
    rm -f "${F50_MODULE}/disable" \
        || echo "warning: 无法撤回本次 f50mgr disable 标记" >&2
    RESTORE_DISABLE_CREATED=0
}

cleanup_backup() {
    [ "${BACKUP_CLEANUP_ACTIVE}" = "1" ] || return 0
    rm -f "${sources}" "${temporary}" "${receipt_tmp}" \
        "${archive}" "${archive}.sha256" "${snapshot}"
    BACKUP_CLEANUP_ACTIVE=0
    release_migration_lock
}

cleanup_import() {
    [ "${IMPORT_CLEANUP_ACTIVE}" = "1" ] || return 0
    rm -f "${temporary}"
    IMPORT_CLEANUP_ACTIVE=0
    release_migration_lock
}

require_root() {
    if [ "${F50MGR_ALLOW_NON_ROOT:-0}" != "1" ] && [ "$(id -u)" != "0" ]; then
        fail "此操作必须以 root 运行"
    fi
}

sha256_file() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | awk '{print $1}'
    else
        shasum -a 256 "$1" | awk '{print $1}'
    fi
}

pid_belongs_to_f50mgr() {
    pid="$1"
    case "${pid}" in
        ''|*[!0-9]*) return 1 ;;
    esac
    kill -0 "${pid}" 2>/dev/null || return 1
    [ -r "${PROC_ROOT}/${pid}/cmdline" ] || return 1
    executable="$(tr '\000' '\n' <"${PROC_ROOT}/${pid}/cmdline" | sed -n '1p')"
    [ "${executable}" = "${F50_BIN}" ]
}

validated_f50mgr_pid_from_file() {
    pid_file="$1"
    [ -f "${pid_file}" ] || {
        echo "error: f50mgr PID 文件不存在" >&2
        return 1
    }
    pid="$(sed -n '1p' "${pid_file}")"
    case "${pid}" in
        ''|*[!0-9]*)
            echo "error: f50mgr PID 文件无效" >&2
            return 1
            ;;
    esac
    pid_belongs_to_f50mgr "${pid}" || {
        echo "error: PID ${pid} 不属于活动的 f50mgr，拒绝终止" >&2
        return 1
    }
    printf '%s\n' "${pid}"
}

discover_f50mgr_pid() {
    if [ "${F50MGR_TEST_OBSERVE_DISABLE_BEFORE_SCAN:-0}" = "1" ]; then
        [ "${F50MGR_ALLOW_NON_ROOT:-0}" = "1" ] \
            && [ -n "${ROOT}" ] && [ "${PROC_ROOT}" != "/proc" ] \
            || {
                echo "error: 进程扫描观察仅允许在隔离测试模式使用" >&2
                return 1
            }
        [ -e "${F50_MODULE}/disable" ] || {
            echo "error: 扫描 f50mgr 进程前尚未创建 disable 标记" >&2
            return 1
        }
    fi
    candidates=""
    for process_dir in "${PROC_ROOT}"/[0-9]*; do
        [ -d "${process_dir}" ] || continue
        candidates="${candidates}
${process_dir##*/}"
    done

    found=""
    count=0
    for candidate in ${candidates}; do
        pid_belongs_to_f50mgr "${candidate}" || continue
        found="${candidate}"
        count=$((count + 1))
    done
    [ "${count}" -le 1 ] || {
        echo "error: 发现多个活动的 f50mgr 进程，无法唯一确认目标" >&2
        return 1
    }
    [ "${count}" -eq 0 ] || printf '%s\n' "${found}"
}

resolve_f50mgr_pid() {
    pid_file="$1"
    discovered="$(discover_f50mgr_pid)" || return 1
    if [ -f "${pid_file}" ]; then
        recorded="$(validated_f50mgr_pid_from_file "${pid_file}")" || return 1
        [ -n "${discovered}" ] && [ "${discovered}" = "${recorded}" ] || {
            echo "error: f50mgr PID 文件与唯一活动进程不一致" >&2
            return 1
        }
        printf '%s\n' "${recorded}"
    else
        [ -z "${discovered}" ] || printf '%s\n' "${discovered}"
    fi
}

receipt_value() {
    sed -n "s/^$1=//p" "${MIGRATION_DIR}/latest"
}

verify_backup() {
    receipt="${MIGRATION_DIR}/latest"
    [ -f "${receipt}" ] || {
        echo "error: 未找到迁移备份凭据，请先运行 backup" >&2
        return 1
    }
    archive="$(receipt_value archive)"
    expected="$(receipt_value sha256)"
    case "${archive}" in
        "${MIGRATION_DIR}"/*.tar.gz) ;;
        *) echo "error: 备份路径不在受控迁移目录内" >&2; return 1 ;;
    esac
    [ -f "${archive}" ] || {
        echo "error: 备份归档不存在: ${archive}" >&2
        return 1
    }
    [ -n "${expected}" ] || {
        echo "error: 备份凭据缺少 SHA256" >&2
        return 1
    }
    actual="$(sha256_file "${archive}")"
    [ "${actual}" = "${expected}" ] || {
        echo "error: 备份 SHA256 不匹配，拒绝迁移" >&2
        return 1
    }
}

validate_archive() {
    entries="$("${TAR}" -tzf "${archive}")" || fail "备份归档无法读取"
    echo "${entries}" | while IFS= read -r entry; do
        case "${entry}" in
            data/adb/box|data/adb/box/*|\
            data/adb/modules/box_for_root|data/adb/modules/box_for_root/*|\
            data/adb/modules_update/box_for_root|data/adb/modules_update/box_for_root/*|\
            data/adb/service.d/box*|data/adb/post-fs-data.d/box*|data/adb/ksu/service.d/box*) ;;
            *) exit 1 ;;
        esac
    done || fail "备份归档包含迁移范围外路径，拒绝恢复"
}

append_source() {
    actual="${ROOT}$1"
    [ -e "${actual}" ] || return 0
    printf '%s\n' "${actual#${ROOT}/}" >>"${sources}"
}

capture_network_state() {
    snapshot="$1"
    {
        echo '# f50mgr box_for_root migration network snapshot'
        echo '## iptables-save'
        iptables-save 2>&1 || true
        echo '## ip6tables-save'
        ip6tables-save 2>&1 || true
        echo '## ip rule show'
        ip rule show 2>&1 || true
        echo '## ip route show table all'
        ip route show table all 2>&1 || true
        echo '## ip -6 rule show'
        ip -6 rule show 2>&1 || true
        echo '## ip -6 route show table all'
        ip -6 route show table all 2>&1 || true
        echo '## processes'
        ps -A 2>&1 || ps 2>&1 || true
    } >"${snapshot}"
    chmod 600 "${snapshot}"
}

backup() {
    require_root
    umask 077
    mkdir -p "${MIGRATION_DIR}"
    chmod 700 "${MIGRATION_DIR}"
    now="${F50MGR_NOW:-$(date '+%Y%m%d-%H%M%S')}"
    archive="${MIGRATION_DIR}/box-for-root-${now}.tar.gz"
    snapshot="${MIGRATION_DIR}/box-for-root-${now}.network.txt"
    sources="${MIGRATION_DIR}/.sources.$$"
    temporary="${archive}.tmp.$$"
    receipt_tmp="${MIGRATION_DIR}/.latest.$$"
    for output in "${archive}" "${archive}.sha256" "${snapshot}" \
        "${sources}" "${temporary}" "${receipt_tmp}"; do
        [ ! -e "${output}" ] || fail "备份输出已存在，拒绝覆盖: ${output}"
    done
    BACKUP_CLEANUP_ACTIVE=1
    trap 'cleanup_backup' EXIT
    trap 'cleanup_backup; exit 130' INT
    trap 'cleanup_backup; exit 143' TERM
    : >"${sources}"

    append_source /data/adb/box
    append_source /data/adb/modules/box_for_root
    append_source /data/adb/modules_update/box_for_root
    for directory in /data/adb/service.d /data/adb/post-fs-data.d /data/adb/ksu/service.d; do
        for actual in "${ROOT}${directory}"/box*; do
            [ -e "${actual}" ] || continue
            printf '%s\n' "${actual#${ROOT}/}" >>"${sources}"
        done
    done
    [ -s "${sources}" ] || fail "未发现 box_for_root 数据或模块，无法创建迁移备份"

    base="${ROOT:-/}"
    "${TAR}" -czf "${temporary}" -C "${base}" -T "${sources}"
    mv "${temporary}" "${archive}"
    capture_network_state "${snapshot}"
    digest="$(sha256_file "${archive}")"
    printf '%s  %s\n' "${digest}" "$(basename "${archive}")" >"${archive}.sha256"
    {
        printf 'archive=%s\n' "${archive}"
        printf 'sha256=%s\n' "${digest}"
        printf 'created_at=%s\n' "${now}"
    } >"${receipt_tmp}"
    chmod 600 "${archive}" "${archive}.sha256" "${receipt_tmp}"
    rm -f "${sources}"
    install_lock_traps
    BACKUP_CLEANUP_ACTIVE=0
    mv "${receipt_tmp}" "${MIGRATION_DIR}/latest"
    echo "backup: verified"
    echo "archive: ${archive}"
    echo "sha256: ${digest}"
}

status() {
    require_root
    [ -d "${ROOT}/data/adb/modules" ] || fail "无法读取 Magisk modules 目录"
    if [ -d "${BOX_MODULE}" ]; then
        if [ -e "${BOX_MODULE}/disable" ]; then
            echo 'box_for_root: disabled'
        else
            echo 'box_for_root: installed'
        fi
    elif [ -d "${BOX_DIR}" ]; then
        echo 'box_for_root: data-only'
    else
        echo 'box_for_root: absent'
    fi
    if [ ! -f "${MIGRATION_DIR}/latest" ]; then
        echo 'backup: missing'
    elif verify_backup >/dev/null 2>&1; then
        echo 'backup: verified'
    else
        echo 'backup: invalid'
    fi
    if [ -e "${F50_MODULE}/disable" ]; then
        echo 'f50mgr: disabled-next-boot'
    else
        echo 'f50mgr: enabled-next-boot'
    fi
}

import_config() {
    require_root
    verify_backup
    source_path="${ROOT}${OLD_CONFIG}"
    target_path="${ROOT}${NEW_CONFIG}"
    temporary="${target_path}.tmp"
    [ -s "${source_path}" ] || fail "旧 Clash 配置不存在: ${OLD_CONFIG}"
    [ -x "${F50_BIN}" ] || fail "f50mgr 二进制不可执行"
    [ ! -e "${temporary}" ] || fail "发现未完成的临时配置: ${NEW_CONFIG}.tmp"
    mkdir -p "$(dirname "${target_path}")"
    IMPORT_CLEANUP_ACTIVE=1
    trap 'cleanup_import' EXIT
    trap 'cleanup_import; exit 130' INT
    trap 'cleanup_import; exit 143' TERM
    "${F50_BIN}" subscription render "${OLD_CONFIG}" --output "${NEW_CONFIG}.tmp"
    [ -s "${temporary}" ] || fail "f50mgr 未生成有效配置"
    if [ -e "${target_path}" ]; then
        if [ "$(sha256_file "${target_path}")" = "$(sha256_file "${temporary}")" ]; then
            rm -f "${temporary}"
            install_lock_traps
            IMPORT_CLEANUP_ACTIVE=0
            echo 'config: already imported'
            return 0
        fi
        fail "目标配置已存在且内容不同，拒绝覆盖: ${NEW_CONFIG}"
    fi
    mv "${temporary}" "${target_path}"
    chmod 600 "${target_path}"
    install_lock_traps
    IMPORT_CLEANUP_ACTIVE=0
    echo "config: imported"
}

rollback_old_runtime_after_stop() {
    reason="$1"
    if [ -f "${BOX_DIR}/scripts/box.service" ]; then
        if ! "${SYSTEM_SH}" "${BOX_DIR}/scripts/box.service" start; then
            fail "${reason}；旧服务恢复失败，需要人工介入，disable 标记已保留"
        fi
    fi
    if [ -f "${BOX_DIR}/scripts/box.iptables" ]; then
        if ! "${SYSTEM_SH}" "${BOX_DIR}/scripts/box.iptables" enable; then
            if [ -f "${BOX_DIR}/scripts/box.service" ]; then
                "${SYSTEM_SH}" "${BOX_DIR}/scripts/box.service" stop \
                    || echo "warning: 旧规则恢复失败后再次停止旧服务也失败" >&2
            fi
            fail "${reason}；旧规则恢复失败，需要人工介入，disable 标记已保留"
        fi
    fi
    rm -f "${BOX_MODULE}/disable" \
        || fail "旧运行态已恢复但无法撤回 disable 标记，需要人工介入"
    fail "${reason}；旧运行态已恢复并撤回 disable 标记"
}

disable_old() {
    require_root
    if [ ! -d "${BOX_MODULE}" ]; then
        echo 'box_for_root: already absent'
        return 0
    fi
    verify_backup
    if [ -e "${BOX_MODULE}/disable" ]; then
        echo 'box_for_root: already disabled'
        return 0
    fi
    [ -x "${F50_BIN}" ] || fail "f50mgr 二进制不可执行"
    f50_status="$(${F50_BIN} status 2>&1)" || fail "f50mgr 状态检查失败"
    echo "${f50_status}" | grep -q 'daemon: running' || fail "f50mgr daemon 未运行"
    echo "${f50_status}" | grep -q 'mihomo: running' || fail "f50mgr Mihomo 未运行"
    pid_file="${ROOT}/data/adb/f50mgr/run/f50mgr.pid"
    resolve_f50mgr_pid "${pid_file}" >/dev/null \
        || fail "f50mgr daemon 进程身份校验失败"

    touch "${BOX_MODULE}/disable" || fail "无法创建旧模块 disable 标记"
    if [ -f "${BOX_DIR}/scripts/box.service" ]; then
        if ! "${SYSTEM_SH}" "${BOX_DIR}/scripts/box.service" stop; then
            rm -f "${BOX_MODULE}/disable" \
                || fail "旧模块服务停止失败且无法撤回 disable 标记，需要人工介入"
            fail "旧模块服务停止失败，已撤回 disable 标记"
        fi
    fi
    if [ -f "${BOX_DIR}/scripts/box.iptables" ]; then
        "${SYSTEM_SH}" "${BOX_DIR}/scripts/box.iptables" disable \
            || rollback_old_runtime_after_stop "旧模块 iptables 清理失败"
    fi
    if iptables-save 2>/dev/null | grep -q 'BOX_'; then
        rollback_old_runtime_after_stop "仍检测到 BOX_ iptables 链"
    fi
    echo 'box_for_root: disabled'
    echo 'next: reboot and verify f50mgr health before removing any old data'
}

restore_old() {
    require_root
    verify_backup
    validate_archive
    mkdir -p "${F50_MODULE}"
    if [ ! -e "${F50_MODULE}/disable" ]; then
        touch "${F50_MODULE}/disable" || fail "无法创建 f50mgr disable 标记"
        RESTORE_DISABLE_CREATED=1
    fi
    trap 'rollback_restore_disable_marker; release_migration_lock' EXIT
    trap 'rollback_restore_disable_marker; release_migration_lock; exit 130' INT
    trap 'rollback_restore_disable_marker; release_migration_lock; exit 143' TERM
    pid_file="${ROOT}/data/adb/f50mgr/run/f50mgr.pid"
    pid="$(resolve_f50mgr_pid "${pid_file}")" \
        || fail "无法安全确认 f50mgr daemon 状态"
    if [ -x "${F50_BIN}" ]; then
        "${F50_BIN}" iptables disable
        "${F50_BIN}" iptables v6-block-disable
    elif iptables-save 2>/dev/null | grep -q 'F50MGR_'; then
        fail "f50mgr 二进制不可执行且规则仍存在，拒绝启动旧模块"
    fi
    if [ -n "${pid}" ]; then
        pid_belongs_to_f50mgr "${pid}" \
            || fail "f50mgr PID 归属在终止前发生变化，拒绝继续"
        kill -TERM "${pid}" 2>/dev/null || fail "无法停止 f50mgr daemon"
        attempts=0
        while kill -0 "${pid}" 2>/dev/null && [ "${attempts}" -lt 10 ]; do
            sleep 1
            attempts=$((attempts + 1))
        done
        kill -0 "${pid}" 2>/dev/null && fail "f50mgr daemon 未在 10 秒内退出"
    fi
    base="${ROOT:-/}"
    "${TAR}" -xzf "${archive}" -C "${base}"
    RESTORE_DISABLE_CREATED=0
    install_lock_traps
    rm -f "${BOX_MODULE}/disable" "${BOX_MODULE}/remove"
    echo 'restore: completed'
    echo 'next: reboot; box_for_root will start and f50mgr will stay disabled'
}

usage() {
    cat <<'EOF'
用法: migrate-box-for-root.sh {status|backup|import-config|disable|restore}

  status         只读显示旧模块、备份与下次启动状态
  backup         备份旧数据、模块和网络状态，不修改运行态
  import-config  将旧 Clash 配置规范化为 f50mgr 配置，拒绝覆盖冲突
  disable        校验备份和 f50mgr 健康后停止并禁用旧模块，不删除数据
  restore        校验备份后恢复旧模块，并禁用 f50mgr 以避免双代理冲突
EOF
}

case "${1:-}" in
    status) status ;;
    backup) run_mutating_command backup ;;
    import-config) run_mutating_command import_config ;;
    disable) run_mutating_command disable_old ;;
    restore) run_mutating_command restore_old ;;
    help|-h|--help|'') usage ;;
    *) usage >&2; exit 2 ;;
esac
