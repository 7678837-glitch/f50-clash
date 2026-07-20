#!/usr/bin/env bash
set -euo pipefail

SYFT_VERSION="1.44.0"

sha256_file() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | awk '{print $1}'
    else
        shasum -a 256 "$1" | awk '{print $1}'
    fi
}

if [[ $# -ne 2 ]]; then
    echo "usage: $0 INPUT_ARCHIVE OUTPUT_SPDX_JSON" >&2
    exit 2
fi
INPUT="$1"
OUTPUT="$2"
[[ -f "${INPUT}" ]] || {
    echo "error: SBOM 输入不存在: ${INPUT}" >&2
    exit 1
}

case "$(uname -s)-$(uname -m)" in
    Linux-x86_64)
        ASSET="syft_${SYFT_VERSION}_linux_amd64.tar.gz"
        EXPECTED_SHA256="0e91737aee2b5baf1d255b959630194a302335d848ff97bb07921eb6205b5f5a"
        ;;
    Darwin-arm64)
        ASSET="syft_${SYFT_VERSION}_darwin_arm64.tar.gz"
        EXPECTED_SHA256="24e4d34078ae81da7c82539616f0ccac3e226cf4f74a38ce6fb3463619e50a55"
        ;;
    *)
        echo "error: 暂不支持当前 Syft 平台: $(uname -s)-$(uname -m)" >&2
        exit 2
        ;;
esac

TEMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/f50mgr-syft.XXXXXX")"
trap 'rm -rf "${TEMP_DIR}"' EXIT
ARCHIVE="${TEMP_DIR}/${ASSET}"
URL="https://github.com/anchore/syft/releases/download/v${SYFT_VERSION}/${ASSET}"

curl -fL --retry 3 --connect-timeout 15 -o "${ARCHIVE}" "${URL}"
ACTUAL_SHA256="$(sha256_file "${ARCHIVE}")"
if [[ "${ACTUAL_SHA256}" != "${EXPECTED_SHA256}" ]]; then
    echo "error: Syft 下载包 SHA256 不匹配: ${ACTUAL_SHA256}" >&2
    exit 1
fi
tar -xzf "${ARCHIVE}" -C "${TEMP_DIR}" syft
mkdir -p "$(dirname "${OUTPUT}")"
"${TEMP_DIR}/syft" scan "${INPUT}" -o "spdx-json=${OUTPUT}"
[[ -s "${OUTPUT}" ]] || {
    echo "error: Syft 未生成 SBOM: ${OUTPUT}" >&2
    exit 1
}
