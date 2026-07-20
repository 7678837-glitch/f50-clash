#!/usr/bin/env bash
set -euo pipefail

PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
IMAGE_NAME="${F50MGR_BUILD_IMAGE:-f50mgr-android-builder:rust-1.95.0-ndk-26.3}"
TARGET="aarch64-linux-android"
API_LEVEL="33"
BIN="target/${TARGET}/release/f50mgr"
MODULE_BIN="${PROJECT_ROOT}/magisk-module/bin/f50mgr"
BUILD_INFO="${PROJECT_ROOT}/magisk-module/bin/f50mgr.build-info"
VERSION="$(tr -d '[:space:]' <"${PROJECT_ROOT}/VERSION")"
GIT_COMMIT="$(git -C "${PROJECT_ROOT}" rev-parse --short=12 HEAD)"
SOURCE_DATE_EPOCH="$(git -C "${PROJECT_ROOT}" show -s --format=%ct HEAD)"

DIRTY_STATUS="$(git -C "${PROJECT_ROOT}" status --porcelain --untracked-files=normal)"
if [[ -n "${DIRTY_STATUS}" ]]; then
    echo "error: 正式构建要求干净工作树，请先提交或清理以下变更:" >&2
    echo "${DIRTY_STATUS}" >&2
    exit 1
fi

"${PROJECT_ROOT}/scripts/fetch-assets.sh"

if ! command -v docker >/dev/null 2>&1; then
    echo "error: 未找到 docker；f50mgr 二进制必须通过容器构建。" >&2
    exit 1
fi

docker build \
    --platform linux/amd64 \
    -f "${PROJECT_ROOT}/docker/android-build.Dockerfile" \
    -t "${IMAGE_NAME}" \
    "${PROJECT_ROOT}"

docker run --rm \
    --platform linux/amd64 \
    -e "F50MGR_GIT_COMMIT=${GIT_COMMIT}" \
    -e "SOURCE_DATE_EPOCH=${SOURCE_DATE_EPOCH}" \
    -v "${PROJECT_ROOT}:/workspace" \
    -w /workspace \
    "${IMAGE_NAME}" \
    bash -c "cargo ndk --target arm64-v8a --platform ${API_LEVEL} build --release --bin f50mgr && llvm-strip ${BIN} && llvm-strings ${BIN} | grep -Fq 'f50mgr ${VERSION}' && du -h ${BIN}"

mkdir -p "$(dirname "${MODULE_BIN}")"
cp "${PROJECT_ROOT}/${BIN}" "${MODULE_BIN}"
chmod 755 "${MODULE_BIN}"
echo "module binary: ${MODULE_BIN}"

if command -v sha256sum >/dev/null 2>&1; then
    BINARY_SHA256="$(sha256sum "${MODULE_BIN}" | awk '{print $1}')"
else
    BINARY_SHA256="$(shasum -a 256 "${MODULE_BIN}" | awk '{print $1}')"
fi
BUILD_INFO_TEMP="${BUILD_INFO}.tmp"
printf 'version=%s\ngit_commit=%s\nbinary_sha256=%s\n' \
    "${VERSION}" "${GIT_COMMIT}" "${BINARY_SHA256}" >"${BUILD_INFO_TEMP}"
mv "${BUILD_INFO_TEMP}" "${BUILD_INFO}"

"${PROJECT_ROOT}/scripts/package-release.sh"
