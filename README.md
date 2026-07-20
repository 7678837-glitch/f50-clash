# f50mgr

面向 ZTE F50 / MU300 移动热点的 Magisk 透明代理管理模块。它在设备内运行
Mihomo 和可选的 EasyTier，并通过 `http://192.168.0.1:14180` 提供适配手机的
本地管理面板。

[English](README.en.md) · [安装手册](docs/deployment.md) ·
[AI 引导安装](docs/INSTALL-AI.md) · [安全策略](SECURITY.md)

## 使用前必读

> **仅支持 ZTE F50 / MU300。** 当前实现和真机验收基于 Android 13、ARM64、
> Magisk root。不要在其他型号上直接安装。

> 模块会修改 iptables、策略路由和 DNS。安装或从 `box_for_root` 切换前应保留
> 可恢复备份；迁移备份可能包含订阅地址和认证信息，不得上传到 Issue 或 Release。

> `14180` 管理 API 使用设备密钥登录、HttpOnly 会话 Cookie 和 CSRF 校验；Mihomo
> `14193` 控制器使用同一密钥。它们仍只面向可信 LAN：不要做公网端口转发，也不要
> 暴露到 EasyTier 之外的不可信网络。两者当前为明文 HTTP，不要让不可信客户端加入
> 同一热点；需要共享热点时必须启用客户端隔离或使用独立管理网络。

## 功能

- Mihomo 生命周期管理、健康探测、异常自愈和透明代理规则维护。
- 响应式状态面板：蜂窝信号、系统资源、实时流量、设备温度、国内外分流诊断。
- URL/YAML 订阅导入、强制 F50 端口和 DNS 模板、敏感配置独立存放。
- MetaCubeXD 高级面板，连接本机 Mihomo External Controller。
- Mihomo 自动更新安全管线：GitHub 官方 Release 优先、可选公司 HTTPS 备用源、摘要/架构/
  版本校验、更新窗口直连和启动失败自动回滚；首个公开版本因尚无离线签名信任根，
  无人值守激活硬禁用且没有 unsigned 旁路。
- EasyTier v2.4.5 可选 mesh VPN、节点/路由/延迟状态和启停配置。
- `box_for_root` 只备份、不卸载的可逆迁移工具。
- 单一 `VERSION`、锁定第三方资产、确定性 Magisk ZIP、SHA-256 与 SPDX JSON SBOM。

## 快速安装

1. 从 [Releases](https://github.com/xjoker/f50-clash/releases) 下载同一版本的
   `f50mgr-YYYYMMDD.N.zip` 和 `.sha256`。
2. 校验制品：

   ```bash
   # Linux
   sha256sum -c f50mgr-YYYYMMDD.N.zip.sha256

   # macOS
   shasum -a 256 -c f50mgr-YYYYMMDD.N.zip.sha256
   ```

3. 通过 Magisk 安装并重启：

   ```bash
   adb push f50mgr-YYYYMMDD.N.zip /sdcard/Download/f50mgr.zip
   adb shell "su 0 -c 'magisk --install-module /sdcard/Download/f50mgr.zip'"
   adb reboot
   ```

4. 安装界面首次生成管理密钥时立即保存；设备启动后打开
   `http://192.168.0.1:14180/`，使用该密钥登录、导入订阅并执行网络诊断。密钥遗失可
   通过 root 命令 `/data/adb/f50mgr/bin/f50mgr auth reset` 重置。

完整的前置探针、失败分支和验收标准见 [部署与运维手册](docs/deployment.md)。

## 从源码构建

要求：Git、Docker、curl、zip、unzip 和可联网的构建环境。正式构建要求工作树干净；
Android NDK 和 Rust 编译器由锁定摘要的容器提供，第三方二进制在使用前校验
SHA-256。

```bash
git clone https://github.com/xjoker/f50-clash.git
cd f50-clash
./build.sh
```

输出位于：

```text
release/f50mgr-<VERSION>.zip
release/f50mgr-<VERSION>.zip.sha256
```

`build.sh` 同时校验 `magisk-module/bin/f50mgr.build-info` 中的版本、Git 提交和
二进制哈希，拒绝把旧二进制装进新版本包。

## 常用操作

```bash
# 无需登录的发布身份健康检查
curl http://192.168.0.1:14180/health

# root shell 状态与管理密钥重置
adb shell "su 0 -c '/data/adb/f50mgr/bin/f50mgr status'"
adb shell "su 0 -c '/data/adb/f50mgr/bin/f50mgr auth reset'"

# EasyTier 预检与节点状态
adb shell "su 0 -c '/data/adb/f50mgr/bin/f50mgr easytier check'"
adb shell "su 0 -c '/data/adb/f50mgr/bin/easytier-cli node'"
```

高级 Mihomo 面板位于 `http://192.168.0.1:14193/ui/`；首次连接时输入与 14180
相同的管理密钥，密钥不会写进链接。

## 端口

| 地址 | 端口 | 用途 |
|---|---:|---|
| `192.168.0.1` | `14180` | f50mgr 面板、健康检查和管理 API |
| `192.168.0.1` | `14190` | Mihomo mixed proxy |
| `192.168.0.1` | `14191` | Mihomo HTTP proxy |
| `192.168.0.1` | `14192` | Mihomo SOCKS proxy |
| `192.168.0.1` | `14193` | Mihomo External Controller / MetaCubeXD |
| `192.168.0.1` | `14194` | Mihomo redir |
| `192.168.0.1` | `14198` | Mihomo TPROXY |
| `192.168.0.1` | `14153` | Mihomo DNS |
| `127.0.0.1` | `15888` | EasyTier RPC portal，仅设备本机 |

## 开发验证

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

真机相关变更还必须在 F50 和热点客户端上复测；测试矩阵见
[部署与运维手册](docs/deployment.md)。

## 配置与数据边界

- 示例配置：`config/secrets.toml.example`。
- 真实密钥：`/data/adb/f50mgr/config/secrets.toml`，权限 `0600`，永不提交。
- 运行数据：`/data/adb/f50mgr/`。
- 本机设备备份、Release ZIP、真实 secrets 均被 `.gitignore` 排除。

## 参与和安全

- 提交代码前阅读 [CONTRIBUTING.md](CONTRIBUTING.md) 和
  [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md)。
- 漏洞、凭据泄漏或未授权网络暴露请按 [SECURITY.md](SECURITY.md) 私下报告，
  不要创建公开 Issue。
- 普通故障可使用 Issue 模板；日志必须先删除 IMEI、ICCID、IMSI、手机号、订阅 URL
  和 EasyTier 密钥。

## 许可证

Copyright (C) 2026 xjoker

项目代码采用 [GNU GPL v3.0 only](LICENSE)。Mihomo、EasyTier、MetaCubeXD 仍按各自
许可证分发；固定版本、源码提交、修改状态和哈希见
[THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) 与
[`third-party/assets.toml`](third-party/assets.toml)。正式 Release 同时附带 Mihomo
与 EasyTier 对应源码归档。

## 文档

- [架构设计](docs/architecture.md)
- [部署与运维](docs/deployment.md)
- [AI 引导安装 SOP](docs/INSTALL-AI.md)
- [设备技术档案](docs/device-notes.md)
- [重启稳定性测试](docs/reboot-stability.md)
- [开源审计记录](docs/open-source-audit.md)
- [Release 制品说明](release/README.md)
