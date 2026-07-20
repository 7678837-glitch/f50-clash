#!/usr/bin/env bash
set -euo pipefail

PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
VERSION_FILE="${F50MGR_VERSION_FILE:-${PROJECT_ROOT}/VERSION}"

sha256_file() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | awk '{print $1}'
    else
        shasum -a 256 "$1" | awk '{print $1}'
    fi
}

require_release_input() {
    local module_dir="$1"
    local relative
    for relative in \
        module.prop customize.sh service.sh uninstall.sh migrate-box-for-root.sh easytier.sha256 \
        bin/f50mgr bin/f50mgr.build-info bin/mihomo.fallback \
        bin/easytier.fallback bin/easytier-cli.fallback \
        assets/zashboard.tar.gz; do
        if [[ ! -f "${module_dir}/${relative}" ]]; then
            echo "error: 发布输入缺失: ${module_dir}/${relative}" >&2
            return 1
        fi
    done
}

verify_build_info() {
    local module_dir="$1"
    local version="$2"
    local info="${module_dir}/bin/f50mgr.build-info"
    local built_version built_commit built_sha256 expected_commit actual_sha256
    built_version="$(sed -n 's/^version=//p' "${info}")"
    built_commit="$(sed -n 's/^git_commit=//p' "${info}")"
    built_sha256="$(sed -n 's/^binary_sha256=//p' "${info}")"
    expected_commit="$(git -C "${PROJECT_ROOT}" rev-parse --short=12 HEAD)"
    actual_sha256="$(sha256_file "${module_dir}/bin/f50mgr")"

    [[ "${built_version}" == "${version}" ]] || {
        echo "error: f50mgr 二进制版本门禁失败: ${built_version} != ${version}" >&2
        return 1
    }
    [[ "${built_commit}" == "${expected_commit}" ]] || {
        echo "error: f50mgr 提交门禁失败: ${built_commit} != ${expected_commit}" >&2
        return 1
    }
    [[ "${built_sha256}" == "${actual_sha256}" ]] || {
        echo "error: f50mgr 二进制 SHA256 门禁失败" >&2
        return 1
    }
}

package_release() {
    local verify_assets="$1"
    command -v zip >/dev/null 2>&1 || {
        echo "error: 生成 Magisk 包需要 zip" >&2
        return 1
    }
    local module_dir output_dir
    module_dir="${F50MGR_MODULE_DIR:-${PROJECT_ROOT}/magisk-module}"
    output_dir="${F50MGR_OUTPUT_DIR:-${PROJECT_ROOT}/release}"
    require_release_input "${module_dir}" || return 1
    if [[ "${verify_assets}" == "true" ]]; then
        "${PROJECT_ROOT}/scripts/fetch-assets.sh" --verify-only || return 1
    fi

    local version module_version stage archive archive_name digest
    version="$(tr -d '[:space:]' <"${VERSION_FILE}")"
    module_version="$(sed -n 's/^version=//p' "${module_dir}/module.prop")"
    if [[ "${module_version}" != "${version}" ]]; then
        echo "error: module.prop version=${module_version}，VERSION=${version}" >&2
        return 1
    fi
    verify_build_info "${module_dir}" "${version}" || return 1

    mkdir -p "${output_dir}"
    output_dir="$(cd "${output_dir}" && pwd)"
    local publish_temp
    stage="$(mktemp -d "${TMPDIR:-/tmp}/f50mgr-release.XXXXXX")"
    publish_temp="$(mktemp -d "${output_dir}/.f50mgr-publish.XXXXXX")"
    trap "rm -rf '${stage}' '${publish_temp}'" EXIT
    archive_name="f50mgr-${version}.zip"
    archive="${output_dir}/${archive_name}"

    cp -R "${module_dir}/." "${stage}/"
    cp "${PROJECT_ROOT}/LICENSE" "${stage}/LICENSE"
    cp "${PROJECT_ROOT}/THIRD_PARTY_NOTICES.md" "${stage}/THIRD_PARTY_NOTICES.md"
    mkdir -p "${stage}/licenses"
    cp "${PROJECT_ROOT}/third-party/licenses/LGPL-3.0.txt" "${stage}/licenses/LGPL-3.0.txt"
    chmod 755 \
        "${stage}/customize.sh" \
        "${stage}/service.sh" \
        "${stage}/uninstall.sh" \
        "${stage}/migrate-box-for-root.sh" \
        "${stage}/bin/"*
    find "${stage}" -exec touch -t 198001010000 {} +

    (
        cd "${stage}"
        find . -type f -print | sed 's#^\./##' | LC_ALL=C sort \
            | zip -X -9 -q "${publish_temp}/${archive_name}" -@
    ) || return 1
    unzip -t "${publish_temp}/${archive_name}" >/dev/null || return 1
    digest="$(sha256_file "${publish_temp}/${archive_name}")"
    printf '%s  %s\n' "${digest}" "${archive_name}" \
        >"${publish_temp}/${archive_name}.sha256"
    mv "${publish_temp}/${archive_name}" "${archive}"
    mv "${publish_temp}/${archive_name}.sha256" "${archive}.sha256"
    rm -rf "${stage}" "${publish_temp}"
    trap - EXIT

    echo "release: ${archive}"
    echo "sha256: ${digest}"
}

self_test() {
    local root fixture out1 out2 first second relative version
    root="$(mktemp -d "${TMPDIR:-/tmp}/f50mgr-package-test.XXXXXX")"
    fixture="${root}/module"
    out1="${root}/out1"
    out2="${root}/out2"
    version="$(tr -d '[:space:]' <"${VERSION_FILE}")"
    mkdir -p "${fixture}/bin" "${fixture}/assets"
    for relative in customize.sh service.sh uninstall.sh migrate-box-for-root.sh easytier.sha256; do
        printf 'fixture\n' >"${fixture}/${relative}"
    done
    for relative in f50mgr mihomo.fallback easytier.fallback easytier-cli.fallback; do
        printf '%s\n' "${relative}" >"${fixture}/bin/${relative}"
    done
    printf 'dashboard\n' >"${fixture}/assets/zashboard.tar.gz"
    printf 'id=f50mgr\nversion=%s\n' "${version}" >"${fixture}/module.prop"
    printf 'version=%s\ngit_commit=%s\nbinary_sha256=%s\n' \
        "${version}" \
        "$(git -C "${PROJECT_ROOT}" rev-parse --short=12 HEAD)" \
        "$(sha256_file "${fixture}/bin/f50mgr")" \
        >"${fixture}/bin/f50mgr.build-info"

    F50MGR_MODULE_DIR="${fixture}" F50MGR_OUTPUT_DIR="${out1}" package_release false >/dev/null
    printf 'tampered\n' >>"${fixture}/bin/f50mgr"
    if F50MGR_MODULE_DIR="${fixture}" F50MGR_OUTPUT_DIR="${out2}" \
        package_release false >/dev/null 2>&1; then
        echo "error: 篡改后的 f50mgr 不应通过打包门禁" >&2
        return 1
    fi
    printf 'f50mgr\n' >"${fixture}/bin/f50mgr"
    F50MGR_MODULE_DIR="${fixture}" F50MGR_OUTPUT_DIR="${out2}" package_release false >/dev/null
    first="$(sha256_file "${out1}/f50mgr-${version}.zip")"
    second="$(sha256_file "${out2}/f50mgr-${version}.zip")"
    [[ "${first}" == "${second}" ]]
    local entries
    entries="$(unzip -Z1 "${out1}/f50mgr-${version}.zip")"
    grep -Fxq 'LICENSE' <<<"${entries}"
    grep -Fxq 'THIRD_PARTY_NOTICES.md' <<<"${entries}"
    grep -Fxq 'migrate-box-for-root.sh' <<<"${entries}"
    rm -rf "${root}"
    echo "package self-test: deterministic ${first}"
}

case "${1:-}" in
    --self-test)
        self_test
        ;;
    "")
        package_release true
        ;;
    *)
        echo "usage: $0 [--self-test]" >&2
        exit 2
        ;;
esac
