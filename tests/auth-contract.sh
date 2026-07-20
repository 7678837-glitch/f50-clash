#!/usr/bin/env bash
set -euo pipefail

CUSTOMIZE="magisk-module/customize.sh"

rg -Fq 'AUTH_RESULT="$($F50MGR_BIN auth ensure 2>&1)"' "$CUSTOMIZE"
rg -Fq 'abort "f50mgr: 无法初始化管理密钥' "$CUSTOMIZE"
rg -Fq 'f50mgr auth reset' "$CUSTOMIZE"
if rg -n 'rm -f .*auth\.key|cat >.*auth\.key|cp .*auth\.key' "$CUSTOMIZE"; then
    echo "customize.sh 不得覆盖既有 auth.key" >&2
    exit 1
fi

echo "管理认证安装契约通过"
