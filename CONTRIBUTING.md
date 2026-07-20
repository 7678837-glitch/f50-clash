# 参与贡献

感谢参与 f50mgr。这个项目直接修改移动热点的网络栈，合并标准以“可恢复、可在真机
复现、不会泄露设备信息”为先。

## 开始前

1. 先搜索已有 Issue，确认问题尚未覆盖。
2. 功能或架构变更先提交 Issue 说明使用场景；小型 bug、文档和测试修复可直接 PR。
3. 每个 PR 聚焦一件事，避免夹带重命名、全仓格式化或无关清理。
4. 不提交生成的 Release ZIP、真实 `secrets.toml`、设备备份或运行日志。

## 本地环境

- Rust 1.95.0（由 `rust-toolchain.toml` 锁定）
- Docker，构建 Android ARM64 制品
- curl、zip、ADB
- 真机变更需要 ZTE F50 / MU300、Magisk root 和一个热点客户端

```bash
git clone https://github.com/xjoker/f50-clash.git
cd f50-clash
HOST_TARGET="$(rustc -vV | sed -n 's/^host: //p')"
cargo test --locked --target "${HOST_TARGET}" -- --test-threads=1
```

## 修改与测试

有明确行为契约的 Rust 或 shell 逻辑应先提交能复现失败的测试，再写最小实现。纯文档、
配置和样式变更不要求测试先行，但不能破坏已有契约。

提交 PR 前运行：

```bash
HOST_TARGET="$(rustc -vV | sed -n 's/^host: //p')"
cargo clippy --locked --target "${HOST_TARGET}" --all-targets -- -D warnings
cargo test --locked --target "${HOST_TARGET}" -- --test-threads=1
bash tests/version-contract.sh
bash tests/easytier-pin.sh
bash tests/migration-contract.sh
bash tests/release-build-contract.sh
bash tests/open-source-contract.sh
```

涉及 iptables、watchdog、安装、订阅、Mihomo 或 EasyTier 的变更，还需在 PR 中列出
实际真机命令、设备版本和结果。不要用 mock 结果代替真机证据。

## 版本和发布

- 根目录 `VERSION` 是唯一用户可见版本源，格式为 `YYYYMMDD.N`。
- 修改版本前先执行 `date`，同日递增 `.N`，跨日从 `.1` 开始。
- 不要手工编辑 Release ZIP；干净工作树中运行 `./build.sh`。
- 第三方版本、下载地址或哈希变更必须同步更新 `third-party/assets.toml`、
  `THIRD_PARTY_NOTICES.md` 和相关契约测试。

## 隐私与安全

Issue、PR、commit message 和测试夹具都不得包含真实 IMEI、ICCID、IMSI、手机号、
公网地址、订阅 URL、Basic Auth 凭据、EasyTier network secret 或访问令牌。安全漏洞
请按 [SECURITY.md](SECURITY.md) 私下报告。

## 许可

提交贡献即表示你有权提供该内容，并同意按仓库的 GPL-3.0-only 许可证发布。
