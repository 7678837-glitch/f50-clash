#!/usr/bin/env bash
set -euo pipefail

PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MANIFEST="magisk-module/easytier.sha256"
BINARIES=(
    "magisk-module/bin/easytier.fallback"
    "magisk-module/bin/easytier-cli.fallback"
)

cd "${PROJECT_ROOT}"
(cd magisk-module && sha256sum -c easytier.sha256)
git ls-files --error-unmatch "${MANIFEST}" "${BINARIES[@]}" >/dev/null

TEMP_ROOT="$(mktemp -d)"
trap 'rm -rf "${TEMP_ROOT}"' EXIT
mkdir -p "${TEMP_ROOT}/magisk-module/bin"
cp "${MANIFEST}" "${TEMP_ROOT}/magisk-module/easytier.sha256"
cp "${BINARIES[@]}" "${TEMP_ROOT}/magisk-module/bin/"
printf 'tampered' >>"${TEMP_ROOT}/magisk-module/bin/easytier-cli.fallback"

if (cd "${TEMP_ROOT}/magisk-module" && sha256sum -c easytier.sha256 >/dev/null 2>&1); then
    echo "error: 被篡改的 EasyTier CLI 仍通过哈希校验" >&2
    exit 1
fi

echo "EasyTier v2.4.5 core/CLI 版本锁校验通过"
