# 部署与运维手册

## 命令引号规约

多命令 root 操作一律 `adb shell "su 0 -c '...'"` 双引号包裹——`adb shell su 0 -c 'A; B'`
的引号被本机 shell 消费，只有 A 以 root 运行（2026-07-16 真机踩坑定案）。
完整规约与正误对照见 [INSTALL-AI.md](INSTALL-AI.md) 第 0 节。

## 环境要求

- Docker / OrbStack
- ADB
- 目标设备：ZTE F50 / MU300，Android 13，Magisk root
- 设备 LAN IP：`192.168.0.1`

## 构建

```bash
./build.sh
```

正式构建要求工作树干净。`build.sh` 会校验并获取锁定的第三方资产，使用固定摘要的
Android builder 镜像交叉编译 `aarch64-linux-android` 二进制，校验版本、提交和
二进制哈希，再生成确定性 Magisk ZIP：

```text
magisk-module/bin/f50mgr
magisk-module/bin/f50mgr.build-info
release/f50mgr-<VERSION>.zip
release/f50mgr-<VERSION>.zip.sha256
```

## 打包

```bash
./scripts/package-release.sh
```

不要手工 zip `magisk-module/`：自动脚本会检查二进制是否来自当前提交，并补入许可证、
第三方声明和迁移工具。`release/*.zip` 已被 `.gitignore` 忽略。

## 安装

```bash
VERSION="$(tr -d '[:space:]' < VERSION)"
adb push "release/f50mgr-${VERSION}.zip" /sdcard/Download/f50mgr.zip
adb shell "su 0 -c 'magisk --install-module /sdcard/Download/f50mgr.zip'"
adb reboot
```

设备重启后验证：

```bash
adb wait-for-device
adb shell getprop sys.boot_completed
adb shell "su 0 -c '/data/adb/f50mgr/bin/f50mgr status'"
curl http://192.168.0.1:14180/health
```

首次安装会在安装界面显示一次管理密钥，必须立即保存。14180 的管理 API 需要登录会话，
修改请求还需要页面内存中的 CSRF nonce；日常操作应直接使用管理页，不要把密钥拼进 URL。
升级安装会保留原密钥；密钥遗失时使用下方 `f50mgr auth reset` 生成新密钥，随后 daemon
重启会使旧会话失效。14193 的 MetaCubeXD/Mihomo 控制器也使用这一个密钥。

## f50mgr 端口表

| 地址 | 端口 | 进程 | 功能 | 使用场景 |
|---|---:|---|---|---|
| `192.168.0.1` | `14180` | `f50mgr` | 主面板与 API | 日常状态、控制操作 |
| `192.168.0.1` | `14190` | `mihomo` | mixed | 手动代理入口 |
| `192.168.0.1` | `14191` | `mihomo` | HTTP | HTTP proxy |
| `192.168.0.1` | `14192` | `mihomo` | SOCKS | SOCKS proxy |
| `192.168.0.1` | `14193` | `mihomo` | External Controller / Dashboard API | Mihomo 面板与状态 API |
| `192.168.0.1` | `14194` | `mihomo` | redir | redir 入口 |
| `192.168.0.1` | `14198` | `mihomo` | TPROXY | 透明代理入口 |
| `192.168.0.1` | `14153` | `mihomo` | DNS | DNS 重定向目标 |
| `0.0.0.0` / `[::]` | `14010` | `easytier` | P2P listener | TCP/UDP listener，供 mesh peer 反向连接和打洞 |
| `127.0.0.1` | `15888` | `easytier` | RPC portal | EasyTier 控制接口，仅设备本机可访问 |

## 日常运维

```bash
# 主面板
open http://192.168.0.1:14180/

# 无需登录的发布身份健康检查
curl http://192.168.0.1:14180/health

# CLI 状态
adb shell "su 0 -c '/data/adb/f50mgr/bin/f50mgr status'"

# 遗失管理密钥时重置；daemon 重启后旧会话失效
adb shell "su 0 -c '/data/adb/f50mgr/bin/f50mgr auth reset'"

# iptables
adb shell "su 0 -c '/data/adb/f50mgr/bin/f50mgr iptables status'"
adb shell "su 0 -c '/data/adb/f50mgr/bin/f50mgr iptables enable'"
adb shell "su 0 -c '/data/adb/f50mgr/bin/f50mgr iptables disable'"
```

Mihomo/EasyTier 启停、订阅导入、EasyTier 配置和聚合状态均在登录后的 14180 页面操作。

## 临时文件清理

只清理明确由本项目测试产生的文件，避免误删用户数据：

```bash
adb shell "su 0 -c 'rm -f /data/local/tmp/f50mgr.* /data/local/tmp/f50-secrets.toml'"
rm -f /private/tmp/f50mgr*.yaml /private/tmp/f50-url-import-response.json /private/tmp/f50mgr.zip
```

旧模块备份包只有在确认本机 `device-backups/` 已保存后再清理设备临时目录：

```bash
adb shell "su 0 -c 'rm -rf /data/local/tmp/f50-box-backup-*.tar.gz /data/local/tmp/f50-box-snapshots-*'"
```

## 开机启动配置

`/data/adb/f50mgr/config/f50mgr.toml`：

```toml
[mihomo]
auto_start = true

[easytier]
enabled = false
```

| 段 / 键 | 值 | 行为 |
|---|---|---|
| `[mihomo].auto_start` | `true` | daemon 启动后 watchdog 自动拉起 Mihomo |
| `[mihomo].auto_start` | `false` | daemon 运行，但 Mihomo 保持停止 |
| `[easytier].enabled` | `true` | EasyTier watchdog 自动拉起 mesh VPN（需 secrets.toml 已配置 network_name/secret/ipv4） |
| `[easytier].enabled` | `false` | EasyTier watchdog 空转，不启动进程（默认） |

控制 API 会同步修改此配置：14180 面板可启停 Mihomo、编辑并启停 EasyTier，也可展示
EasyTier 节点、路由、延迟和流量。Mihomo 默认 `true`、EasyTier 默认 `false`。

## 订阅配置

推荐直接在 `http://192.168.0.1:14180` 面板的「订阅」区域操作：

- URL 导入：面板会下载订阅、生成 F50 专用 `config.yaml`、保存订阅地址到
  `/data/adb/f50mgr/config/secrets.toml`，并重启 Mihomo 加载新配置。
- YAML 上传：面板会读取本地 YAML 文件、生成 F50 专用 `config.yaml`，并重启
  Mihomo；此模式不会覆盖已保存的订阅 URL。

也可以手动编辑 `/data/adb/f50mgr/config/secrets.toml`：

```toml
[subscription]
url = "https://your-sub-url"
```

然后使用 CLI 更新：

```bash
adb shell "su 0 -c '/data/adb/f50mgr/bin/f50mgr subscription update \"https://your-sub-url\"'"
```

## Mihomo 更新安全门禁

首个公开版本的管理页会显示 `disabled_no_trust_root`（待配置离线签名信任根）。此状态下
不会访问更新源、下载资产、运行候选 `mihomo -v` 或替换当前二进制，也没有可开启
unsigned 更新的配置项。更新请安装项目正式 Release；不得把 `github_timeout_secs` 或公司
Basic Auth 误认为真实性证明。

代码中已实现的更新管线会在未来签名清单验证落地后启用：同版本或旧版本在下载资产前
跳过；新版本须通过签名授权、SHA-256、64 MiB 上限、gzip、AArch64 ELF 和版本校验。
激活时先撤 TPROXY 转直连，以带 Bearer 管理密钥的 14193 `/version` 验证候选，失败则
恢复 `.pending` 中的旧版。

以下公司源配置当前仅为未来签名更新预留，不会绕过安全门禁：

```toml
[mihomo]
version_check_interval = "6h"

[download]
github_timeout_secs = 30
company_base_url = "https://updates.example.com/f50"
```

`secrets.toml` 只保存 Basic Auth：

```toml
[download]
username = "your-username"
password = "your-password"
```

启用签名更新后，公司站点必须镜像项目离线签名的清单、签名和资产；Basic Auth 只负责
访问控制。未签名的 `stable.json` 即使摘要正确也不得执行。现有 v1 清单示例仅说明旧版
字段兼容，不构成可激活的签名授权：

```json
{
  "schema_version": 1,
  "version": "v1.19.29",
  "asset_url": "https://updates.example.com/files/mihomo.gz",
  "sha256": "0000000000000000000000000000000000000000000000000000000000000000",
  "size": 16985528,
  "arch": "android-arm64-v8"
}
```

管理页的 Mihomo API 卡片会显示 `disabled_no_trust_root`；未来启用后还会显示
`waiting/checking/pending/activating/up_to_date/failed`。任何状态都不会返回公司 URL、
用户名或密码。

## EasyTier mesh VPN 配置

模块内置并锁定 EasyTier **v2.4.5**（core `4c4d172e`）；core 与 CLI 成对纳入版本控制，`magisk-module/easytier.sha256` 在构建和安装前校验二者哈希。该版本在 Android/F50 数据面验证稳定，升级前必须重新完成 peer、路由和实际 ping/TCP 复测。

EasyTier 提供 mesh VPN 能力，让 F50 自身加入虚拟组网（类 Tailscale 语义），便于异地远程
访问 F50 管理面板。F50MGR 规则会放行 10.0.0.0/8、避免 TPROXY 接管 `tun_et`，并为
热点客户端到 `tun_et` 的流量增加受限 FORWARD 与 MASQUERADE，使热点客户端也能访问
mesh 目标。

启用步骤：

1. 编辑 `/data/adb/f50mgr/config/secrets.toml`，填入 `[easytier]` 段：
   ```toml
   [easytier]
   network_name = "your-network"
   network_secret = "your-secret"
   ipv4 = "10.0.5.3"           # 每台 F50 必须分配唯一 IP
   peers = [
     "tcp://relay.example.com:11010/",
     "udp://relay.example.com:11010/",
   ]
   ```
2. 调用 `POST /api/easytier/start`（或在面板操作）写入 `[easytier].enabled = true`。
3. watchdog 检测到配置存在 + enabled=true 后，自动渲染 `config/easytier/config.toml`
   并启动 `easytier-core`。
4. 验证：登录 14180 管理页查看 EasyTier 运行状态；
   `adb shell su 0 -c '/data/adb/f50mgr/bin/easytier-cli node'` 查看 mesh 节点列表。

强制行为（来自 `src/subscription/easytier.rs`）：

- `hostname = "F50-{IMEI}"`，IMEI 由 getprop 多 key 探测 + `service call iphonesubinfo` 兜底，缓存于 `data/device_id.txt`
- `instance_id` 需稳定身份时应显式填写；留空时每次渲染生成 UUIDv4
- `rpc_portal = "127.0.0.1:15888"`（仅本地，不暴露 LAN）
- `[flags] use_smoltcp = false`、`no_tun = false`：强制内核 TUN 路径，避免用户态网络栈性能损失
- `[flags] private_mode = true`、`latency_first = true`：与你提供的参考配置一致
- P2P listeners 固定为 TCP/UDP `0.0.0.0:14010` 与 `[::]:14010`，供 peer 反向连接和打洞；
  F50 不监听 EasyTier 默认的 11010/11011/11012

### EasyTier CLI 工具

```bash
# 渲染配置预览（不落盘，等价于 GET /api/easytier/preview）
adb shell su 0 -c '/data/adb/f50mgr/bin/f50mgr easytier render'

# 真机预检（TUN 设备 / 二进制 / secrets / Mihomo 共存性 / 设备标识）
adb shell su 0 -c '/data/adb/f50mgr/bin/f50mgr easytier check'

# EasyTier 自带 CLI（节点列表、路由、对端状态）
adb shell su 0 -c '/data/adb/f50mgr/bin/easytier-cli node'
adb shell su 0 -c '/data/adb/f50mgr/bin/easytier-cli route'
adb shell su 0 -c '/data/adb/f50mgr/bin/easytier-cli peer'
```

### Mihomo 与 EasyTier 共存验证 checklist

iptables 层面**不需要任何手动配置**就能共存（设计上隔离），但首次部署应在 F50 真机
按下列顺序确认：

1. **TUN 内核支持**：`adb shell ls -la /dev/net/tun /dev/tun`
   一者存在即可（customize.sh 会自动建软链）；都缺失则 EasyTier 启动会失败
2. **Mihomo 不接管 mesh 流量**：启用 EasyTier 后，从 F50 本机 ping mesh 内其他节点
   应直连返回；`tcpdump -i tun_et` 应能看到包，`tcpdump -i br0` 不应出现 mesh IP
3. **热点客户端访问 mesh**：从热点连接的手机 ping 允许访问的 10.x 目标应成功；
   `F50MGR_ET_FWD` 只放行 LAN 与 `tun_et` 的新建/已建立/相关流量，
   `F50MGR_ET_NAT` 只对 `-o tun_et` 执行 MASQUERADE
4. **重启后两者都自动恢复**：`adb reboot` 后 `f50mgr easytier check` 全 OK
5. **iptables 规则集断言**（开发期单测保护，无需真机重跑）：
   `coexists_with_easytier_private_subnet_return` /
   `coexists_with_easytier_no_tproxy_on_tun` /
   `coexists_with_easytier_local_chain_empty`

## 与旧模块关系

f50mgr 是独立新模块。安装时不会停用、删除或标记卸载 `box_for_root`。

发布包包含可逆迁移工具 `/data/adb/modules/f50mgr/migrate-box-for-root.sh`。所有命令
必须在全局挂载命名空间中以 root 执行；工具不会调用旧模块的卸载脚本，也不会删除
`/data/adb/box`：

```sh
MIGRATE=/data/adb/modules/f50mgr/migrate-box-for-root.sh

# 1. 只读预检
adb shell su -mm -c "$MIGRATE status"

# 2. 创建旧数据、模块及网络状态快照；归档和 SHA256 留在 f50mgr 运行目录
adb shell su -mm -c "$MIGRATE backup"

# 3. 可选：将旧 Clash 配置规范化为 f50mgr 端口/模板
#    目标配置已存在且内容不同时会拒绝覆盖
adb shell su -mm -c "$MIGRATE import-config"

# 4. 校验备份与 f50mgr 健康后，清理旧规则、停止旧服务并创建 disable 标记
adb shell su -mm -c "$MIGRATE disable"

# 5. 重启后验证 f50mgr；不要立即删除旧模块或 /data/adb/box
adb reboot

# 回滚：校验同一备份，撤除 f50mgr 规则、恢复旧文件并禁用 f50mgr 下次启动
adb shell su -mm -c "$MIGRATE restore"
adb reboot
```

备份默认写入 `/data/adb/f50mgr/migration/`，权限为 `0700/0600`。其中归档可能包含
订阅地址或认证信息，不得上传 Issue、Release 或提交 Git。`restore` 不重放捕获的原始
iptables；旧模块会在重启后按自身配置重建规则，避免把过期接口状态写回设备。

如果确认切换到 f50mgr，应先备份旧模块再清理。当前已完成备份：

```text
device-backups/20260511-144750/f50-box-backup-20260511-144750.tar.gz
sha256: 0a9ed191bb5a1f7e85fff4527b238e38c14f48c38437dae1d4947d443e60f079
```

已从设备删除的旧路径：

```text
/data/adb/box
/data/adb/modules/box_for_root
/data/adb/modules_update/box_for_root
/data/adb/service.d/box*
/data/adb/post-fs-data.d/box*
```

## ZTE 系统端口

| 端口 | 占用者 | 处理 |
|---:|---|---|
| `2333` | UFI-TOOLS | 禁止请求，避免依赖第三方面板 |
| `8080` | `com.zte.web` | 只读 goform 数据源 |
| `9090` | `com.zte.web` | 避让 |
| `9999` | NetworkStats | 避让 |

## 验证记录

已在真机验证：

- `14180` 页面返回 200。
- `14180/health` 返回 200，登录管理页后状态卡片正常刷新。
- `POST /api/mihomo/stop` 会停止 Mihomo 并写 `auto_start=false`。
- `POST /api/mihomo/start` 会写 `auto_start=true` 并由 watchdog 拉起。
- `POST /api/subscription/import` 支持 URL 导入，导入后保存 `secrets.toml` 并重启 Mihomo。
- `POST /api/subscription/upload` 支持 YAML 文件上传，导入后重启 Mihomo 且不覆盖已保存 URL。
- 登录后的状态接口包含 `proxy_health`，通过 Mihomo HTTP 代理探测国内外站点。
- goform 拆分读取后，SINR、RSRQ、频点、PCI、基站 ID 可正常返回。
- 2026-05-12 已完成 3 轮连续重启测试，daemon、Mihomo、14180 API、Mihomo API、
  国内外探测均自动恢复正常，记录见 [重启稳定性测试](reboot-stability.md)。

## 订阅 robustness 复测方案（Phase 8f baseline）

每次改动 `src/subscription/mod.rs` 或 `enforce_runtime_defaults` 路径后，按下表
跑一遍真机回归。期望全部 PASS（**首次 R5 失败暴露过 sanitize 漏挂在
enforce_runtime_defaults 路径的真 bug，已修**）。

| # | 测试项 | 输入 | 期望 | 通过判据 |
|---|---|---|---|---|
| R1 | base64 订阅 → render | `echo "proxies: []\nrules: []" \| base64` | render 成功 + 输出含 `mixed-port: 14190` | grep 命中 |
| R2 | 含 tun 段订阅 → render | `tun:\n  enable: true\nproxies: []\nrules: []` | 输出**不含** `^tun:` | grep 无匹配 |
| R3 | proxy-providers 订阅 → render | `proxy-providers:\n  remote-1: {type: http, url: ...}` | 渲染成功（不阻断），daemon log 出现 warn | 输出含 `^proxy-providers:` |
| R4 | 顶层非 mapping → render | `- a\n- b` | 退出非 0，stderr 含「顶层必须是 mapping」 | grep 命中 |
| R5 | daemon 重启后 config.yaml 无 tun | 旧 config.yaml 内含 `tun:` 段 | 重启后该段被移除 | `grep -cE "^tun:" config.yaml = 0` |
| R6a | 手机 ping baidu | — | rtt < 200ms | ping 出现 `rtt min/avg/max` |
| R6b | 手机 ping mesh 10.0.5.2 | — | rtt < 150ms | 同上 |
| R6c | F50MGR_ET_NAT 命中增长 | 上面 R6b 跑完 | pkts > 0 | iptables 计数 > 0 |
| R7 | mihomo pid 30s 内不变 | 部署完成后 | 同一 pid | `before == after` |

执行模板：

```bash
# 见 phase8f 提交描述里完整脚本，或直接重跑此处汇总用法
F50=<F50 serial>; PHONE=<phone serial>
adb -s $F50 shell "su 0 -c '/data/adb/f50mgr/bin/f50mgr subscription render <yaml> -o <out>'"
adb -s $F50 shell "su 0 -c 'iptables -t nat -nvxL F50MGR_ET_NAT'"
adb -s $PHONE shell "su 0 -c 'ping -c 3 10.0.5.2'"
```

### 与单测的对应

R1-R5 对应单测：`subscription::tests::robustness_*`（`mod.rs` 末尾）。
R5 在 `enforce_runtime_defaults_also_strips_tun` 单测中保护，避免回归。
R6/R7 是真机端到端，单测无法覆盖（依赖物理网络 + mesh 节点）。
