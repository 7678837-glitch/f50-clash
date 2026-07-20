#!/usr/bin/env bash
set -euo pipefail

PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${PROJECT_ROOT}"

required_files=(
    README.en.md
    CONTRIBUTING.md
    SECURITY.md
    CODE_OF_CONDUCT.md
    .github/ISSUE_TEMPLATE/bug_report.yml
    .github/ISSUE_TEMPLATE/feature_request.yml
    .github/ISSUE_TEMPLATE/config.yml
    .github/pull_request_template.md
    .github/dependabot.yml
    .github/workflows/ci.yml
    .github/workflows/security.yml
    .github/workflows/release.yml
    scripts/ci-secret-scan.sh
    scripts/generate-sbom.sh
)

for path in "${required_files[@]}"; do
    [[ -f "${path}" ]] || {
        echo "error: 开源交付文件缺失: ${path}" >&2
        exit 1
    }
done

grep -Fq '仅支持 ZTE F50 / MU300' README.md
grep -Fq './build.sh' README.md
grep -Fq 'SECURITY.md' README.md
grep -Fq 'Copyright (C) 2026 xjoker' THIRD_PARTY_NOTICES.md
grep -Fq 'HttpOnly' README.md
grep -Fq 'Mihomo 自动更新' README.md
grep -Fq 'company_base_url' docs/deployment.md
grep -Fq '管理密钥 <64位十六进制值>' docs/INSTALL-AI.md
grep -Fq '# company_base_url = "https://updates.example.com/f50"' magisk-module/customize.sh
grep -Fq '# [download]' config/secrets.toml.example
if grep -Fq 'subscription_update_interval' magisk-module/customize.sh; then
    echo "error: 安装模板仍包含未实现的订阅自动更新字段" >&2
    exit 1
fi

if grep -n '/api/reboot' README.md docs/architecture.md docs/deployment.md release/README.md; then
    echo "error: 公开文档仍宣称不存在的 /api/reboot" >&2
    exit 1
fi
if grep -n -E '14180 对 LAN 无认证|当前没有登录认证|provide unauthenticated management APIs|14180/api/status' README.md README.en.md docs/architecture.md docs/deployment.md docs/INSTALL-AI.md; then
    echo "error: 公开文档仍包含认证实施前的过时描述或匿名状态接口示例" >&2
    exit 1
fi
if grep -n -E '设备属性：ICCID|热点客户端不能访问 mesh|没有任何为 EasyTier 添加的规则|自动注入 host/port' docs/architecture.md docs/deployment.md; then
    echo "error: 公开文档包含已被当前实现取代的状态或 EasyTier 行为" >&2
    exit 1
fi
grep -Fq 'MetaCubeXD' docs/architecture.md
grep -Fq '`14010`' docs/deployment.md
grep -Fq 'F50MGR_ET_NAT' docs/deployment.md

grep -Fqx 'FROM debian:bookworm-slim@sha256:7b140f374b289a7c2befc338f42ebe6441b7ea838a042bbd5acbfca6ec875818 AS android-sdk' docker/android-build.Dockerfile
grep -Fqx 'FROM rust:${RUST_VERSION}-bookworm@sha256:6258907abe69656e41cd992e0b705cdcfabcbbe3db374f92ed2d47121282d4a1 AS builder' docker/android-build.Dockerfile
grep -Fqx 'ARG CMDLINE_TOOLS_SHA256=2d2d50857e4eb553af5a6dc3ad507a17adf43d115264b1afc116f95c92e5e258' docker/android-build.Dockerfile
grep -Fqx 'device-backups/' .dockerignore

validate_action_refs() {
    awk '
    /^[[:space:]]*(-[[:space:]]+)?uses:/ && $0 !~ /@[0-9a-f]{40}([[:space:]]*#.*)?$/ {
        print FILENAME ":" FNR ":" $0
        invalid = 1
    }
    END { exit invalid }
' "$@"
}

invalid_action_fixture="$(mktemp "${TMPDIR:-/tmp}/f50-action-ref.XXXXXX")"
trap 'rm -f "${invalid_action_fixture}"' EXIT
printf '%s\n' '- uses: owner/action@stable' >"${invalid_action_fixture}"
if validate_action_refs "${invalid_action_fixture}" >/dev/null; then
    echo "error: Action SHA 契约未拒绝 step 级可变引用" >&2
    exit 1
fi
rm -f "${invalid_action_fixture}"
trap - EXIT

if ! validate_action_refs .github/workflows/*.yml; then
    echo "error: GitHub Action 必须锁定完整 commit SHA" >&2
    exit 1
fi
if grep -R -n -E 'pull_request_target|permissions: write-all' .github/workflows; then
    echo "error: 工作流包含高风险触发器或过宽权限" >&2
    exit 1
fi

grep -Fq 'permissions:' .github/workflows/ci.yml
grep -Fq 'contents: read' .github/workflows/ci.yml
grep -Fq 'cargo clippy --locked' .github/workflows/ci.yml
grep -Fq 'cargo test --locked' .github/workflows/ci.yml
grep -Fq 'bash tests/open-source-contract.sh' .github/workflows/ci.yml
grep -Fq 'scripts/ci-secret-scan.sh' .github/workflows/ci.yml

grep -Fq 'osv-scanner-reusable' .github/workflows/security.yml
grep -Fq '9a498708959aeaef5ef730655706c5a1df1edbc2' .github/workflows/security.yml
grep -Fq 'cargo install cargo-audit --version 0.22.2 --locked' .github/workflows/security.yml
grep -Fq 'cargo audit --deny warnings' .github/workflows/security.yml

grep -Fq '"v[0-9]*"' .github/workflows/release.yml
grep -Fq 'EXPECTED_TAG="v${VERSION}"' .github/workflows/release.yml
grep -Fq 'cargo install cargo-audit --version 0.22.2 --locked' .github/workflows/release.yml
grep -Fq 'cargo audit --deny warnings' .github/workflows/release.yml
grep -Fq 'publish:' .github/workflows/release.yml
test "$(grep -c 'contents: write' .github/workflows/release.yml)" -eq 1
test "$(grep -c 'GH_TOKEN:' .github/workflows/release.yml)" -eq 1
grep -Fq './build.sh' .github/workflows/release.yml
grep -Fq 'scripts/fetch-assets.sh --source-offers release' .github/workflows/release.yml
grep -Fq 'scripts/ci-secret-scan.sh "release/f50mgr-${VERSION}.zip"' .github/workflows/release.yml
grep -Fq 'scripts/generate-sbom.sh' .github/workflows/release.yml
grep -Fq 'f50mgr-${VERSION}.spdx.json' .github/workflows/release.yml
grep -Fq 'gh release create' .github/workflows/release.yml

grep -Fq 'GITLEAKS_VERSION="8.30.1"' scripts/ci-secret-scan.sh
grep -Fq '551f6fc83ea457d62a0d98237cbad105af8d557003051f41f3e7ca7b3f2470eb' scripts/ci-secret-scan.sh
grep -Fq 'b40ab0ae55c505963e365f271a8d3846efbc170aa17f2607f13df610a9aeb6a5' scripts/ci-secret-scan.sh
grep -Fq -- '--max-archive-depth=2' scripts/ci-secret-scan.sh

grep -Fq 'SYFT_VERSION="1.44.0"' scripts/generate-sbom.sh
grep -Fq '0e91737aee2b5baf1d255b959630194a302335d848ff97bb07921eb6205b5f5a' scripts/generate-sbom.sh
grep -Fq '24e4d34078ae81da7c82539616f0ccac3e226cf4f74a38ce6fb3463619e50a55' scripts/generate-sbom.sh

grep -Fq -- '--source-offers' scripts/fetch-assets.sh
grep -Fq 'e86b0d24163023c082561ff0b3cf8a592df51b9823b5d83868b79f0bf20c795c' scripts/fetch-assets.sh
grep -Fq '2b13eb34d3946077d0780d421b88ef9dbd722aa51bd0867f80f70ff6d2d3b679' scripts/fetch-assets.sh

grep -Fq 'unzip' README.md
grep -Fq 'unzip' README.en.md
if git ls-files 'release/*.zip' | grep -q .; then
    echo "error: Release ZIP 不得被 Git 跟踪" >&2
    exit 1
fi

echo "开源文档、CI、安全与 Release 契约通过"
