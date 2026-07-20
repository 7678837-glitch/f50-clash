# f50mgr 架构设计

## 总体架构

```
Magisk service.sh
    │
    ▼
/data/adb/f50mgr/bin/f50mgr daemon
    │
    ├─ Web 管理服务 192.168.0.1:14180
    │   ├─ GET  /              分类状态面板，支持手机布局
    │   ├─ GET  /health        版本、Git 提交与 schema 版本
    │   ├─ POST /api/auth/login；GET session；POST logout
    │   ├─ GET  /api/status    聚合状态 JSON
    │   ├─ GET  /api/ping      轻量 LAN RTT 探针
    │   ├─ POST /api/mihomo/start
    │   ├─ POST /api/mihomo/stop
    │   ├─ POST /api/subscription/import
    │   ├─ POST /api/subscription/upload
    │   ├─ GET  /api/subscription/config
    │   ├─ POST /api/subscription/refresh
    │   ├─ POST /api/easytier/start
    │   ├─ POST /api/easytier/stop
    │   ├─ GET  /api/easytier/status
    │   ├─ GET  /api/easytier/preview
    │   ├─ GET/POST /api/easytier/secrets
    │   └─ POST /api/diag/splitflow
    │      （除 `/health`、`/api/ping` 和认证端点外均要求登录；修改请求另验 Origin 与 CSRF）
    │
    ├─ Mihomo watchdog
    │   ├─ 读取 [mihomo] auto_start
    │   ├─ 启动前回收上一代孤儿实例（pid 文件 + /proc cmdline 校验）
    │   ├─ 启动 /data/adb/f50mgr/bin/mihomo
    │   ├─ 维护 /data/adb/f50mgr/run/mihomo.pid
    │   ├─ 异常退出后按策略重启
    │   └─ API 健康探测：豁免 60s 后每 15s 探 14193，连续 3 次失败强制重启
    │      （防"进程存活但监听器消失"的聋态，真机蜂窝切换可触发）
    │
    ├─ EasyTier watchdog（mesh VPN，可选）
    │   ├─ 读取 [easytier] enabled，默认 false
    │   ├─ 缺配置时自动从 secrets.toml 渲染 config.toml
    │   ├─ 启动 /data/adb/f50mgr/bin/easytier
    │   ├─ 维护 /data/adb/f50mgr/run/easytier.pid
    │   └─ 异常退出后按策略重启（同 Mihomo 策略）
    │
    ├─ 状态采集
    │   ├─ /proc、/sys：CPU、内存、流量、存储
    │   ├─ Mihomo 14193：版本、连接数、速率
    │   ├─ Mihomo 14191：国内外站点 HTTPS 代理探测
    │   ├─ ZTE goform 8080：设备、信号、蜂窝数据（不请求订阅者标识）
    │   └─ getprop / service call：仅生成 EasyTier 默认 hostname，不进入状态 API
    │
    └─ iptables
        └─ F50MGR_* chain，只管理本模块规则
           （`coexists_with_easytier_*` 单测保护与 EasyTier 的边界）
```

### Mihomo / EasyTier 共存机制

iptables 同时维护 TPROXY 隔离边界和 LAN 到 EasyTier 的转发规则：

| 维度 | 现有规则的作用 | 对 EasyTier 的效果 |
|---|---|---|
| `mangle PREROUTING` TPROXY | 仅匹配 `-i br+/wlan+/ap+/eth+/...`（热点接口） | F50 本机出向流量不走此链，mesh 流量自由 |
| `EXTERNAL_CHAIN` 私网 RETURN | 含 `-d 10.0.0.0/8 -j RETURN` | EasyTier 默认子网（10.x）流量天然放行到 main 表 |
| `mangle OUTPUT` LOCAL_CHAIN | 链已创建但**保持空规则**（仅跳转，无 mark/TPROXY） | F50 本地进程到 mesh peer 的出向不被接管 |
| policy route fwmark | `pref 100 lookup 2024` | 优先级高于 EasyTier 启动时加的 `from all lookup main`（pref 32766） |
| EasyTier TUN 出向 | EasyTier 自己 `ip route add` 维护 | 与 F50MGR 路由表互不干扰 |
| `filter FORWARD` | 首条跳入 `F50MGR_ET_FWD`，允许 LAN 与 `tun_et` 的已建立/相关流量 | 绕过 Android `tetherctrl_FORWARD` 末尾的兜底 DROP |
| `nat POSTROUTING` | 仅对 `-o tun_et` 执行 MASQUERADE | 热点客户端以 EasyTier TUN 地址访问 mesh，对其他出口无影响 |

修改 `src/iptables/rules.rs::restore_rules` 前必看 `coexists_with_easytier_*` 三个测试断言。

## 模块边界

f50mgr 是新独立模块，不直接替代 `box_for_root`。安装脚本只准备：

```text
/data/adb/modules/f50mgr
/data/adb/f50mgr
```

旧 `box_for_root` 的删除必须在本机备份后进行；迁移工具默认只备份和停用旧模块，
不会删除旧数据。

## 端口分配

所有对外监听默认绑定 `192.168.0.1`，端口统一在 `14100-14200`。

| 地址 | 端口 | 进程 | 功能 | 说明 |
|---|---:|---|---|---|
| `192.168.0.1` | `14180` | `f50mgr` | 主面板与 API | `/`、`/api/status`、控制操作 |
| `192.168.0.1` | `14190` | `mihomo` | mixed | HTTP/SOCKS 混合入口 |
| `192.168.0.1` | `14191` | `mihomo` | HTTP | 显式 HTTP 代理 |
| `192.168.0.1` | `14192` | `mihomo` | SOCKS | 显式 SOCKS 代理 |
| `192.168.0.1` | `14193` | `mihomo` | External Controller | Dashboard API 与 `/ui/` |
| `192.168.0.1` | `14194` | `mihomo` | redir | redir 入口 |
| `192.168.0.1` | `14198` | `mihomo` | TPROXY | 透明代理目标端口 |
| `192.168.0.1` | `14153` | `mihomo` | DNS | UDP 53 重定向目标 |
| `0.0.0.0` / `[::]` | `14010` | `easytier` | P2P listener | TCP/UDP listener，供 mesh peer 反向连接和打洞 |
| `127.0.0.1` | `15888` | `easytier` | RPC portal | EasyTier 控制 RPC，仅设备本机可访问 |

14180 首次安装生成 256-bit 管理密钥。登录成功后服务只下发 HttpOnly、
SameSite=Strict 会话 Cookie 和独立 CSRF nonce；管理密钥不进入 Cookie、URL 或状态接口。
除 `/health`、`/api/ping` 与认证端点外，管理 API 均需有效会话；修改请求还必须满足
Host/Origin 与 CSRF 校验。14193 的 Mihomo `secret` 使用同一管理密钥。两个端口当前
均为明文 HTTP，因此该认证边界只适用于成员可信且受 WPA2/WPA3 保护的 LAN。

## 配置

运行配置位于 `/data/adb/f50mgr/config/f50mgr.toml`。

```toml
[mihomo]
auto_start = true
version_check_interval = "6h"

[download]
github_timeout_secs = 30
# company_base_url = "https://updates.example.com/f50"

[web]
listen = "192.168.0.1:14180"

[iptables]
auto_enable = true

[easytier]
enabled = false   # mesh VPN 总开关，启用后由 watchdog 自动拉起 easytier-core
```

更新模块已实现 GitHub 官方 Release → 公司 HTTPS 备用源、SHA-256、gzip 上限、
AArch64 ELF、`mihomo -v`、事务激活和认证 Controller 健康回滚。但首个公开版本没有
独立于网络源的离线签名公钥，scheduler 固定进入 `disabled_no_trust_root`，不会下载、
执行或激活候选，也没有 unsigned 配置旁路。未来只有在签名清单先于资产下载通过
Ed25519 验证后，才会按 `version_check_interval` 和 `github_timeout_secs` 启用该管线。

当前 daemon Web 监听地址由程序常量固定为 `192.168.0.1:14180`；
`[web] listen` 保留在配置中，作为安装默认值和后续扩展点。

敏感订阅配置位于 `/data/adb/f50mgr/config/secrets.toml`，权限应为 `600`：

```toml
[subscription]
url = ""

# 仅在普通配置启用 company_base_url 时需要
[download]
username = ""
password = ""

# EasyTier 段为可选；启用 [easytier].enabled=true 时必填 network_name / network_secret / ipv4
[easytier]
network_name = ""
network_secret = ""
ipv4 = ""              # 每台 F50 唯一静态 IP，例如 10.0.5.3
# instance_id = ""     # 需稳定身份时显式填写；留空则每次渲染生成 UUID
# hostname = ""        # 留空则用 "F50-{IMEI}"，IMEI 由 getprop / service call 本地读取
peers = [
    "tcp://relay.example.com:11010/",
    "udp://relay.example.com:11010/",
]
```

## Mihomo 生命周期

`[mihomo] auto_start` 是面板开关的持久化状态：

| 值 | 行为 |
|---|---|
| `true` | watchdog 自动启动 Mihomo；异常退出后按策略重启 |
| `false` | watchdog 保持空转，不拉起 Mihomo |

`POST /api/mihomo/stop` 会写入 `auto_start = false` 并向当前 Mihomo 发送
`SIGTERM`。`POST /api/mihomo/start` 会写入 `auto_start = true`，由 watchdog
在下一轮检查中拉起 Mihomo。

## 14180 面板

页面按类别分区：

- 运行状态：daemon、Mihomo、Mihomo API、设备联网
- 设备网络：信号、蜂窝流量、接口实时速率
- 系统资源：CPU、内存、存储
- 代理概览：国内外分流健康、连接数、Mihomo 速率
- 国内外探测：通过 Mihomo HTTP 代理请求百度、腾讯、Google、Cloudflare 的 HTTPS 站点
- 设备属性：运营商、网络类型、固件、LAN/IPv6/MAC 和数据源；不采集或展示
  ICCID、IMEI、IMSI、手机号

面板按钮：

| 控件 | API | 说明 |
|---|---|---|
| 启动 Mihomo | `POST /api/mihomo/start` | 写 `auto_start=true` |
| 停止 Mihomo | `POST /api/mihomo/stop` | 写 `auto_start=false` 并停止进程 |
| 导入 URL | `POST /api/subscription/import` | 下载订阅、保存 URL、重启 Mihomo |
| 上传 YAML | `POST /api/subscription/upload` | 上传 YAML、生成配置、重启 Mihomo，不覆盖已保存 URL |
| 打开面板 | `http://192.168.0.1:14193/ui/` | 打开本地 MetaCubeXD 高级面板 |

EasyTier 状态、节点表、配置和启停已接入 14180 面板；对应 API：

| API | 说明 |
|---|---|
| `POST /api/easytier/start` | 写 `[easytier].enabled=true`，由 watchdog 拉起 |
| `POST /api/easytier/stop` | 写 `false` 并向当前进程发 `SIGTERM` |
| `GET  /api/easytier/status` | 读取节点、路由、延迟、流量、NAT 和版本快照 |
| `GET  /api/easytier/preview` | 渲染当前 secrets 对应的 EasyTier `config.toml`（不落盘；即使已有登录认证仍对 `network_secret` 脱敏） |
| `GET/POST /api/easytier/secrets` | 读取脱敏配置或合并写入 EasyTier 配置；空 secret 保留旧值 |
| `f50mgr easytier render` (CLI) | 同上，命令行版（root shell，输出完整密钥） |
| `f50mgr easytier check` (CLI) | 真机预检：TUN/二进制/secrets/Mihomo 共存性 |

订阅导入统一经过 `src/subscription/mod.rs` 渲染，写入
`/data/adb/f50mgr/config/clash/config.yaml`。渲染时会强制收敛 F50 必要字段，
包括端口、绑定地址、DNS fake-ip、bootstrap DNS、sniffer HTTP
`override-destination`。

## 数据来源

| 数据 | 来源 |
|---|---|
| CPU | `/proc/stat` |
| 内存 | `/proc/meminfo` |
| 流量 | `/proc/net/dev` |
| 存储 | `statvfs("/data")` |
| Mihomo 概览 | `http://192.168.0.1:14193` |
| 国内外探测 | `192.168.0.1:14191` HTTP 代理转发 HTTPS 请求 |
| 设备信号与网络信息 | ZTE 原生 `http://192.168.0.1:8080/goform/goform_get_cmd_process`，命令白名单不含订阅者标识 |
| EasyTier 默认 hostname | `getprop` 多 key 探测 + `service call iphonesubinfo` 兜底，缓存到 `data/device_id.txt`；不进入状态 API |

注意：设备信息集成禁止请求 UFI-TOOLS 的 `2333` 服务。当前实现参考
UFI-TOOLS 源码，直接读取底层 ZTE goform 只读字段。

## 运行目录

```text
/data/adb/modules/f50mgr/          # Magisk 模块目录，只读随模块安装
/data/adb/f50mgr/bin/f50mgr        # 实际运行的 f50mgr
/data/adb/f50mgr/bin/mihomo        # 实际运行的 Mihomo
/data/adb/f50mgr/bin/easytier      # 实际运行的 EasyTier (easytier-core)
/data/adb/f50mgr/bin/easytier-cli  # EasyTier CLI 调试工具
/data/adb/f50mgr/config/           # 配置
/data/adb/f50mgr/config/clash/     # Mihomo config.yaml
/data/adb/f50mgr/config/easytier/  # EasyTier config.toml（首次启动从 secrets 渲染生成）
/data/adb/f50mgr/data/             # 持久缓存（device_id.txt 等）
/data/adb/f50mgr/run/              # PID 文件
```

安装时会刷新 runtime 二进制，避免覆盖复用异常 inode。
