#!/usr/bin/env bash
set -euo pipefail

PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${PROJECT_ROOT}"

VERSION="$(tr -d '[:space:]' <VERSION)"
if [[ ! "${VERSION}" =~ ^[0-9]{8}\.[1-9][0-9]*$ ]]; then
    echo "error: VERSION 必须为 YYYYMMDD.N，当前为 ${VERSION}" >&2
    exit 1
fi

CARGO_VERSION="$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -n 1)"
MODULE_VERSION="$(sed -n 's/^version=//p' magisk-module/module.prop)"
MODULE_VERSION_CODE="$(sed -n 's/^versionCode=//p' magisk-module/module.prop)"

[[ "${CARGO_VERSION}" == "${VERSION}.0" ]]
[[ "${MODULE_VERSION}" == "${VERSION}" ]]
[[ "${MODULE_VERSION_CODE}" == "${VERSION/.}" ]]
grep -Fq 'route("/health", get(api_health))' src/web/mod.rs
grep -Fq 'cargo:rustc-env=F50MGR_VERSION=' build.rs
if grep -Eq 'CARGO_PKG_VERSION' src/main.rs src/status.rs; then
    echo "error: 用户可见版本必须统一使用 F50MGR_VERSION" >&2
    exit 1
fi

echo "VERSION/Cargo/Magisk/health 版本契约通过: ${VERSION}"
