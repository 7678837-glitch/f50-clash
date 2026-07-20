#!/usr/bin/env bash
set -euo pipefail

PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${PROJECT_ROOT}"

test -x scripts/fetch-assets.sh
test -x scripts/package-release.sh
test -x magisk-module/migrate-box-for-root.sh

MANIFEST="$(scripts/fetch-assets.sh --print-manifest)"
grep -Fq 'Mihomo v1.19.24 e45a0b18ea3554cf7f322b59cf2dd21f3e4879dd2db657cbba6a557235e33115' <<<"${MANIFEST}"
grep -Fq 'EasyTier v2.4.5 df08c842f2ab2b8e9922f13c686a1d0f5a5219775cfdabb3e4a8599c6772201f' <<<"${MANIFEST}"
grep -Fq 'MetaCubeXD v1.269.0 22704e7bdea257bc69498e790760c6b8098885972273d3dcb3ea647451a1151e' <<<"${MANIFEST}"

grep -Fq 'scripts/fetch-assets.sh' build.sh
grep -Fq 'scripts/package-release.sh' build.sh
grep -Fq 'status --porcelain --untracked-files=normal' build.sh
grep -Fq 'if [ -e "$MODDIR/disable" ]' magisk-module/service.sh
scripts/package-release.sh --self-test
bash tests/migration-contract.sh

echo "发布构建契约通过"
