# F50 设备技术档案

## 设备信息

| 项目 | 值 |
|---|---|
| 型号 | ZTE MU300 / F50 |
| 系统 | Android 13 |
| Magisk | v30.7 |
| 架构 | ARM64 |
| LAN IP | `192.168.0.1` |
| 运营商示例 | 中国广电 5G / NR |

## 网络拓扑

```text
5G/LTE 上行
    │
    └─ sipa_eth0

设备内部
    ├─ br0 192.168.0.1/24
    │   ├─ wlan0 WiFi 热点
    │   └─ usb0 USB 网络共享
    └─ lo

系统服务
    ├─ dnsmasq: 192.168.0.1:53
    ├─ com.zte.web: 8080 / 9090
    └─ UFI-TOOLS: 2333
```

## 已知端口占用

| 端口 | 占用进程 / 服务 | f50mgr 策略 |
|---:|---|---|
| `2333` | UFI-TOOLS | 禁止请求 |
| `8080` | ZTE Web / goform | 只读采集设备信息 |
| `9090` | ZTE Web | 避让 |
| `9999` | NetworkStats | 避让 |

f50mgr 自用端口统一改为 `14100-14200`，并绑定 `192.168.0.1`。

## UFI-TOOLS 数据来源结论

UFI-TOOLS 页面中的设备信息主要来自三类数据源：

| 类别 | 来源 |
|---|---|
| CPU/内存/温度/连接数 | `/proc`、`/sys`、Android 系统 API |
| 信号/设备/蜂窝流量 | ZTE `goform_get_cmd_process` |
| 控制类操作 | UFI-TOOLS 自身 API 或 shell/AT 能力 |

当前 f50mgr 只复用只读信息来源，不请求 UFI-TOOLS `2333`，也不使用它的
root shell / AT 控制接口。

当前读取的 ZTE 原生接口：

```text
http://192.168.0.1:8080/goform/goform_get_cmd_process
```

请求必须带 ZTE Web 期望的请求头：

```text
Referer: http://192.168.0.1/index.html
Origin:  http://192.168.0.1
```

信号字段需要拆分读取，避免一次 cmd 列表过长导致 ZTE 返回缺字段。

## 热点客户端透明代理

F50 热点客户端流量从 `br0` 进入内核，不是从 `wlan0`。透明代理规则必须覆盖
`br+`：

```text
TPROXY tcp/udp -i br+ → 14198
```

DNS 入口统一使用 `14153`：

```text
UDP 53 → 192.168.0.1:14153
```

## Mihomo 必要配置

运行时配置会被 f50mgr 收敛到 F50 端口：

```yaml
allow-lan: true
bind-address: 192.168.0.1
mixed-port: 14190
port: 14191
socks-port: 14192
redir-port: 14194
tproxy-port: 14198
external-controller: 192.168.0.1:14193

dns:
  enable: true
  listen: 192.168.0.1:14153
  enhanced-mode: fake-ip
  fake-ip-range: 198.18.0.1/16

sniffer:
  enable: true
  force-dns-mapping: true
  parse-pure-ip: true
  override-destination: true
  sniff:
    HTTP:
      override-destination: true
```

透明 HTTP 访问在 fake-ip 模式下依赖 `sniffer.sniff.HTTP.override-destination=true`。
缺少这个字段时，客户端直连 HTTP 站点可能被发往 fake-ip 目的地址，表现为国内或
国外 HTTP 站点异常。

## Magisk 自启动

```text
/data/adb/modules/f50mgr/service.sh
    └─ 等待 bootanim stopped
    └─ while 循环: /data/adb/f50mgr/bin/f50mgr daemon
       （daemon 死亡后 5s 自动重拉；daemon 退出时自行撤除 iptables 规则 fail-open）
```

daemon 内部根据 `[mihomo] auto_start` 决定是否拉起 Mihomo。默认 `true`。
TPROXY 规则由就绪门控启用：mihomo external-controller (192.168.0.1:14193)
可连之前绝不启用（无超时兜底）；运行期 mihomo 连续 2 个周期（60s）不健康则
撤规则转直连，恢复后自动重新启用。

Mihomo watchdog 除进程存活外还做 API 健康探测（启动豁免 60s，之后每 15s 探测
14193，连续 3 次失败强制重启）。背景：2026-07-16 真机实测蜂窝数据切换
（`svc data disable`）可触发 mihomo 监听器重建失败——进程存活但所有端口消失
（"活着但聋了"），无 API 探测时永远无人重启。

daemon 启动时按 pid 文件 + `/proc/<pid>/cmdline` 回收上一代残留的
mihomo/easytier 孤儿（daemon 被 kill -9 时子进程孤儿化，直接 spawn 会双实例
抢端口，reuseport 下 UDP 流量被分流）。

日志（v0.2.8+）落盘 `run/{f50mgr,mihomo,easytier}.log`，spawn 时超 5MB
轮转 .1、运行中超限原地截断。级别限量：mihomo 渲染强制 `log-level: warning`；
easytier 用 `ET_CONSOLE_LOG_LEVEL=warn` 环境变量（config 的
`[console_logger]` 段实测被忽略）。

## SELinux 与二进制路径

运行时二进制放在 `/data/adb/f50mgr/bin/`：

```text
/data/adb/f50mgr/bin/f50mgr
/data/adb/f50mgr/bin/mihomo
```

模块安装时会从 `/data/adb/modules/f50mgr/bin/` 复制到 runtime 目录，并设置：

```text
f50mgr: root:root 755
mihomo: root:net_admin 6755
context: adb_data_file
```

不要直接执行 `/data/adb/modules/f50mgr/bin/f50mgr`；模块目录内文件可能不可执行，
实际运行入口是 `/data/adb/f50mgr/bin/f50mgr`。

## 已验证备份

旧 `box_for_root` 本机备份：

```text
device-backups/20260511-144750/f50-box-backup-20260511-144750.tar.gz
sha256: 0a9ed191bb5a1f7e85fff4527b238e38c14f48c38437dae1d4947d443e60f079
```

## DNS 与 fake-ip 修复记录

时间：2026-05-11

现象：

- 14180 `proxy_health` 一度显示国内异常、国外正常。
- Mihomo 规则中 `ChinaDomain`、`GEOSITE,CN`、`GEOIP,CN` 走 `DIRECT`。
- 国内域名解析命中 `nameserver-policy` 后访问 `223.5.5.5:53`、
  `119.29.29.29:53` 或运营商 DNS `43.239.172.1/43.239.172.2:53`。
- Mihomo 进程从设备本机发起 UDP DNS 查询时返回 `connection refused`。
- 设备系统工具 `ping www.baidu.com` 报 `unknown host`，说明 Android 系统
  resolver 没有被 Mihomo 接管。
- 同一 F50 网络下，电脑作为客户端 `dig @43.239.172.1 www.baidu.com` 可解析，
  因此不是运营商出口整体拦截客户端 DNS。

应急修复：

- 将设备当前 `/data/adb/f50mgr/config/clash/config.yaml` 的国内
  `nameserver-policy` 临时改为当时可用的 DoH over `Proxies`。
- 覆盖范围：`geosite:cn`、`geosite:geolocation-cn`、`+.alicdn.com`、
  `+.qpic.cn`、`+.bdstatic.com`、`+.gtimg.cn`。
- 文件权限已恢复为 `600`。
- `[mihomo] auto_start = true`。

重启验证：

- 重启后 `f50mgr` 和 `mihomo` 均自动运行。
- 配置仍保留 DoH over `Proxies`。
- 14180 `/api/status` 最终恢复 `国内外正常`。
- 注意：刚启动后第一次健康检查可能短暂全失败，等待 Mihomo DNS/代理组热起来后恢复。

已固化：

- `src/subscription/mod.rs` 会在订阅渲染和 Mihomo 启动前收敛 DNS、bootstrap
  DNS、sniffer 和所有 F50 固定端口。
- HTTP fake-ip 访问问题已通过 `sniffer.sniff.HTTP.override-destination=true`
  固化，不再依赖手工编辑配置。

排障经验：

- 若国内外同时异常，先确认 SIM 卡套餐状态和上游蜂窝数据是否可用；欠费会表现为
  DNS/HTTP 全链路异常，容易误判为代理配置问题。
