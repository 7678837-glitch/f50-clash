#!/usr/bin/env bash
set -euo pipefail

PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
GITLEAKS_VERSION="8.30.1"

sha256_file() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | awk '{print $1}'
    else
        shasum -a 256 "$1" | awk '{print $1}'
    fi
}

case "$(uname -s)-$(uname -m)" in
    Linux-x86_64)
        ASSET="gitleaks_${GITLEAKS_VERSION}_linux_x64.tar.gz"
        EXPECTED_SHA256="551f6fc83ea457d62a0d98237cbad105af8d557003051f41f3e7ca7b3f2470eb"
        ;;
    Darwin-arm64)
        ASSET="gitleaks_${GITLEAKS_VERSION}_darwin_arm64.tar.gz"
        EXPECTED_SHA256="b40ab0ae55c505963e365f271a8d3846efbc170aa17f2607f13df610a9aeb6a5"
        ;;
    *)
        echo "error: 暂不支持当前 Gitleaks 平台: $(uname -s)-$(uname -m)" >&2
        exit 2
        ;;
esac

TEMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/f50mgr-gitleaks.XXXXXX")"
trap 'rm -rf "${TEMP_DIR}"' EXIT
ARCHIVE="${TEMP_DIR}/${ASSET}"
URL="https://github.com/gitleaks/gitleaks/releases/download/v${GITLEAKS_VERSION}/${ASSET}"

curl -fL --retry 3 --connect-timeout 15 -o "${ARCHIVE}" "${URL}"
ACTUAL_SHA256="$(sha256_file "${ARCHIVE}")"
if [[ "${ACTUAL_SHA256}" != "${EXPECTED_SHA256}" ]]; then
    echo "error: Gitleaks 下载包 SHA256 不匹配: ${ACTUAL_SHA256}" >&2
    exit 1
fi
tar -xzf "${ARCHIVE}" -C "${TEMP_DIR}" gitleaks

if [[ $# -eq 0 ]]; then
    "${TEMP_DIR}/gitleaks" git --redact --no-banner --no-color "${PROJECT_ROOT}"
elif [[ $# -eq 1 ]]; then
    TARGET="$1"
    [[ "${TARGET}" = /* ]] || TARGET="${PROJECT_ROOT}/${TARGET}"
    [[ -e "${TARGET}" ]] || {
        echo "error: Release 密钥扫描目标不存在: ${TARGET}" >&2
        exit 1
    }
    "${TEMP_DIR}/gitleaks" dir --max-archive-depth=2 --redact --no-banner --no-color "${TARGET}"
else
    echo "usage: $0 [release-file-or-directory]" >&2
    exit 2
fi
