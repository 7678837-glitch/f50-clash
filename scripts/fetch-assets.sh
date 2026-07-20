#!/usr/bin/env bash
set -euo pipefail

PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ASSETS_MANIFEST="${PROJECT_ROOT}/third-party/assets.toml"

MIHOMO_VERSION="v1.19.24"
MIHOMO_URL="https://github.com/MetaCubeX/mihomo/releases/download/v1.19.24/mihomo-android-arm64-v8-v1.19.24.gz"
MIHOMO_ARCHIVE_SHA256="e45a0b18ea3554cf7f322b59cf2dd21f3e4879dd2db657cbba6a557235e33115"
MIHOMO_INSTALLED_SHA256="5915d69c8440267158d7b9fbcb4089d522a3d66e86482dd121200e614c1af68f"
MIHOMO_SOURCE_URL="https://github.com/MetaCubeX/mihomo/archive/a84724665eb7f989809abe463c05f5723bd24975.tar.gz"
MIHOMO_SOURCE_SHA256="e86b0d24163023c082561ff0b3cf8a592df51b9823b5d83868b79f0bf20c795c"

EASYTIER_VERSION="v2.4.5"
EASYTIER_ARCHIVE_SHA256="df08c842f2ab2b8e9922f13c686a1d0f5a5219775cfdabb3e4a8599c6772201f"
EASYTIER_CORE_SHA256="88fd4f8ec30b0766251578cdc82631eddc3710d6dd913547224a4afc34693d36"
EASYTIER_CLI_SHA256="8cdabc31ebfb0c2acb2e81a21237ec99f0397e35016055869556fc8338fcd6b8"
EASYTIER_SOURCE_URL="https://github.com/EasyTier/EasyTier/archive/4c4d172e2f0b345b42102969ce4a98e936ca1d81.tar.gz"
EASYTIER_SOURCE_SHA256="2b13eb34d3946077d0780d421b88ef9dbd722aa51bd0867f80f70ff6d2d3b679"

METACUBEXD_VERSION="v1.269.0"
METACUBEXD_ARCHIVE_SHA256="22704e7bdea257bc69498e790760c6b8098885972273d3dcb3ea647451a1151e"
METACUBEXD_INSTALLED_SHA256="9ffe4792b9b1790e7129fc26efdfc7ddb2ad707acc3321525fcfba957ddfcce2"

sha256_file() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | awk '{print $1}'
    else
        shasum -a 256 "$1" | awk '{print $1}'
    fi
}

verify_file() {
    local label="$1"
    local path="$2"
    local expected="$3"
    if [[ ! -f "${path}" ]]; then
        echo "error: ${label} 缺失: ${path}" >&2
        return 1
    fi
    local actual
    actual="$(sha256_file "${path}")"
    if [[ "${actual}" != "${expected}" ]]; then
        echo "error: ${label} SHA256 不匹配: ${actual}" >&2
        return 1
    fi
    echo "verified: ${label}"
}

verify_manifest_sync() {
    local value
    for value in \
        "${MIHOMO_URL}" \
        "${MIHOMO_ARCHIVE_SHA256}" \
        "${MIHOMO_INSTALLED_SHA256}" \
        "${MIHOMO_SOURCE_URL}" \
        "${MIHOMO_SOURCE_SHA256}" \
        "${EASYTIER_ARCHIVE_SHA256}" \
        "${EASYTIER_CORE_SHA256}" \
        "${EASYTIER_CLI_SHA256}" \
        "${EASYTIER_SOURCE_URL}" \
        "${EASYTIER_SOURCE_SHA256}" \
        "${METACUBEXD_ARCHIVE_SHA256}" \
        "${METACUBEXD_INSTALLED_SHA256}"; do
        if ! grep -Fq "${value}" "${ASSETS_MANIFEST}"; then
            echo "error: 构建资产与 third-party/assets.toml 不一致: ${value}" >&2
            return 1
        fi
    done
}

print_manifest() {
    echo "Mihomo ${MIHOMO_VERSION} ${MIHOMO_ARCHIVE_SHA256}"
    echo "EasyTier ${EASYTIER_VERSION} ${EASYTIER_ARCHIVE_SHA256}"
    echo "MetaCubeXD ${METACUBEXD_VERSION} ${METACUBEXD_ARCHIVE_SHA256}"
}

verify_installed_assets() {
    verify_file "Mihomo ${MIHOMO_VERSION}" \
        "${PROJECT_ROOT}/magisk-module/bin/mihomo.fallback" "${MIHOMO_INSTALLED_SHA256}"
    verify_file "EasyTier core ${EASYTIER_VERSION}" \
        "${PROJECT_ROOT}/magisk-module/bin/easytier.fallback" "${EASYTIER_CORE_SHA256}"
    verify_file "EasyTier CLI ${EASYTIER_VERSION}" \
        "${PROJECT_ROOT}/magisk-module/bin/easytier-cli.fallback" "${EASYTIER_CLI_SHA256}"
    verify_file "MetaCubeXD ${METACUBEXD_VERSION}" \
        "${PROJECT_ROOT}/magisk-module/assets/zashboard.tar.gz" "${METACUBEXD_INSTALLED_SHA256}"
}

fetch_mihomo() {
    local destination="${PROJECT_ROOT}/magisk-module/bin/mihomo.fallback"
    if [[ -f "${destination}" ]] \
        && [[ "$(sha256_file "${destination}")" == "${MIHOMO_INSTALLED_SHA256}" ]]; then
        echo "verified: Mihomo ${MIHOMO_VERSION}"
        return
    fi

    command -v curl >/dev/null 2>&1 || {
        echo "error: 获取 Mihomo 需要 curl" >&2
        return 1
    }
    command -v gzip >/dev/null 2>&1 || {
        echo "error: 解压 Mihomo 需要 gzip" >&2
        return 1
    }

    local temp_archive temp_binary
    temp_archive="$(mktemp "${TMPDIR:-/tmp}/f50mgr-mihomo.XXXXXX.gz")"
    temp_binary="${temp_archive%.gz}"
    trap "rm -f '${temp_archive}' '${temp_binary}'" EXIT

    echo "download: Mihomo ${MIHOMO_VERSION}"
    curl -fL --retry 3 --connect-timeout 15 -o "${temp_archive}" "${MIHOMO_URL}"
    verify_file "Mihomo 上游归档 ${MIHOMO_VERSION}" "${temp_archive}" "${MIHOMO_ARCHIVE_SHA256}"
    gzip -dc "${temp_archive}" >"${temp_binary}"
    verify_file "Mihomo 解压二进制 ${MIHOMO_VERSION}" "${temp_binary}" "${MIHOMO_INSTALLED_SHA256}"
    chmod 755 "${temp_binary}"
    mkdir -p "$(dirname "${destination}")"
    mv "${temp_binary}" "${destination}"
    rm -f "${temp_archive}"
    trap - EXIT
}

fetch_source_archive() {
    local label="$1"
    local url="$2"
    local expected="$3"
    local destination="$4"
    local temp
    temp="$(mktemp "${TMPDIR:-/tmp}/f50mgr-source.XXXXXX.tar.gz")"
    if ! curl -fL --retry 3 --connect-timeout 15 -o "${temp}" "${url}"; then
        rm -f "${temp}"
        return 1
    fi
    if ! verify_file "${label}" "${temp}" "${expected}"; then
        rm -f "${temp}"
        return 1
    fi
    mv "${temp}" "${destination}"
}

fetch_source_offers() {
    local output_dir="$1"
    local mihomo_name="mihomo-a84724665eb7-source.tar.gz"
    local easytier_name="easytier-4c4d172e2f0b-source.tar.gz"
    mkdir -p "${output_dir}"
    fetch_source_archive "Mihomo 对应源码" \
        "${MIHOMO_SOURCE_URL}" "${MIHOMO_SOURCE_SHA256}" "${output_dir}/${mihomo_name}"
    fetch_source_archive "EasyTier 对应源码" \
        "${EASYTIER_SOURCE_URL}" "${EASYTIER_SOURCE_SHA256}" "${output_dir}/${easytier_name}"
    printf '%s  %s\n%s  %s\n' \
        "${MIHOMO_SOURCE_SHA256}" "${mihomo_name}" \
        "${EASYTIER_SOURCE_SHA256}" "${easytier_name}" \
        >"${output_dir}/third-party-sources.sha256.tmp"
    mv "${output_dir}/third-party-sources.sha256.tmp" \
        "${output_dir}/third-party-sources.sha256"
}

case "${1:-}" in
    --print-manifest)
        print_manifest
        exit 0
        ;;
    --verify-only)
        verify_manifest_sync
        verify_installed_assets
        exit 0
        ;;
    --source-offers)
        [[ $# -eq 2 ]] || {
            echo "usage: $0 --source-offers OUTPUT_DIR" >&2
            exit 2
        }
        verify_manifest_sync
        fetch_source_offers "$2"
        exit 0
        ;;
    "")
        verify_manifest_sync
        fetch_mihomo
        verify_file "EasyTier core ${EASYTIER_VERSION}" \
            "${PROJECT_ROOT}/magisk-module/bin/easytier.fallback" "${EASYTIER_CORE_SHA256}"
        verify_file "EasyTier CLI ${EASYTIER_VERSION}" \
            "${PROJECT_ROOT}/magisk-module/bin/easytier-cli.fallback" "${EASYTIER_CLI_SHA256}"
        verify_file "MetaCubeXD ${METACUBEXD_VERSION}" \
            "${PROJECT_ROOT}/magisk-module/assets/zashboard.tar.gz" "${METACUBEXD_INSTALLED_SHA256}"
        ;;
    *)
        echo "usage: $0 [--verify-only|--print-manifest|--source-offers OUTPUT_DIR]" >&2
        exit 2
        ;;
esac
