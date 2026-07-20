# INSTALL-AI.md —— 面向 AI 助手的 f50mgr 安装指导

> **用法**：把本文档整体喂给 AI 助手（Claude Code / Codex 等），并告诉它
> 「按这份文档引导我在 ZTE F50 上安装 f50mgr」。文档为 AI 设计：每步都有
> 命令、期望输出、机器可验证判据和失败分支。
>
> **AI 执行规约**：
> 1. 逐步执行，每步先跑「判据」命令确认再进下一步；判据不满足走对应失败分支
> 2. 高风险操作（`adb reboot`、`magisk --install-module`、删除文件）执行前必须
>    向用户明确说明后果并等确认
> 3. 命令失败 2 次停止重试，向用户报告原始错误输出

## 0. 硬规约：adb + root 命令的引号形态（先读这个）

本机 shell 会消费一层引号。`adb shell su 0 -c 'A; B'` 到达设备后变成
`su 0 -c A; B` —— **只有 A 以 root 运行，B 落到 shell 用户（uid 2000）**，
对 `/data/adb` 的操作会得到 Permission denied（2026-07-16 真机踩坑定案）。

```bash
# ✅ 单命令：也使用统一的双层引号，避免本机 shell 改写参数
adb shell "su 0 -c '/data/adb/f50mgr/bin/f50mgr status'"

# ✅ 多命令 / 含变量 / 含重定向：双引号包裹整个 su 调用
adb shell "su 0 -c 'killall -9 f50mgr; sleep 1; cp /sdcard/new /data/adb/f50mgr/bin/f50mgr'"

# ❌ 错误：分号后的命令不会以 root 运行
adb shell su 0 -c 'cmd1; cmd2'
```

覆盖正在运行的二进制：service.sh 会在 daemon 死后 5s 内重拉，必须
**同一个 su shell 内** `killall -9 f50mgr; sleep 1; cp ...`，否则撞上
Text file busy。

## 1. 前置探针（全部通过才继续）

| # | 检查 | 命令 | 通过判据 | 失败分支 |
|---|---|---|---|---|
| P1 | adb 连通 | `adb get-state` | 输出 `device` | 检查 USB 线/授权弹窗；`adb kill-server && adb start-server` |
| P2 | root 可用 | `adb shell "su 0 -c 'id'"` | 输出含 `uid=0(root)` | 设备未 root 或 Magisk 未授权 shell——在设备 Magisk App 里允许 Shell 的 root 请求 |
| P3 | Magisk 存在 | `adb shell "su 0 -c 'magisk -V'"` | 输出数字版本号（≥27000） | 先安装 Magisk，本模块依赖 Magisk 模块机制 |
| P4 | 设备型号 | `adb shell getprop ro.product.model` | 含 `F50` 或 `MU300` | 非目标设备：iptables 规则集/端口是 F50 专用，其他设备需自行评估，停止并告知用户 |
| P5 | LAN IP | `adb shell ip addr show br0` | 含 `192.168.0.1/24` | 设备 LAN 网段非默认：面板地址/绑定地址硬编码 192.168.0.1，停止并告知用户 |
| P6 | 安装包就绪 | `ls magisk-module/bin/mihomo.fallback magisk-module/bin/geodata 2>/dev/null` | 两者存在 | **仓库 clone 不含兜底二进制**（.gitignore 忽略）。需先获取 mihomo ARM64 二进制与 GeoIP/GeoSite 数据放入对应路径，或使用维护者提供的完整 release zip |

## 2. 构建与打包（已有 release zip 可跳到第 3 步）

```bash
# 需要 Docker/OrbStack。产出 magisk-module/bin/f50mgr
./build.sh
# 判据：输出以 "module binary: .../magisk-module/bin/f50mgr" 结尾，且该文件约 2-3MB

cd magisk-module && zip -r -X /private/tmp/f50mgr.zip . && cd ..
# 判据：zip 包含 module.prop、service.sh、customize.sh、bin/f50mgr、bin/mihomo.fallback
unzip -l /private/tmp/f50mgr.zip | grep -cE "module.prop|service.sh|bin/f50mgr"   # 期望 ≥3
```

失败分支：Docker 拉取镜像失败但本机已有 builder 镜像 → 见 `docs/deployment.md`「构建」一节的直接 docker run 命令。

## 3. 安装（含高风险操作，AI 须先向用户确认）

```bash
adb push /private/tmp/f50mgr.zip /sdcard/Download/f50mgr.zip
# 判据：输出 "1 file pushed"

# ⚠️ 高风险：安装 Magisk 模块。先告知用户再执行
adb shell "su 0 -c 'magisk --install-module /sdcard/Download/f50mgr.zip'"
# 判据：输出无 "Failed"/"Error"，末尾出现 Done 或安装日志正常结束。
# 首次安装还会输出 `f50mgr: 管理密钥 <64位十六进制值>`，必须立即保存该值；
# 升级安装输出“已保留现有管理密钥”时继续使用原密钥。

# ⚠️ 高风险：重启设备。先告知用户（设备热点会短暂中断 ~1 分钟）
adb reboot && adb wait-for-device
```

## 4. 安装后验证（带真机时序预期）

重启后按以下时序验证。**真机基线（v0.2.7 实测）：Web 面板 t+12~14s 起，
mihomo API t+18~22s 起，TPROXY 规则在 API 就绪之后出现（t+20~22s）**。
t 从 `adb wait-for-device` 返回起算。

```bash
# V1 系统启动完成（轮询直到输出 1）
adb shell getprop sys.boot_completed

# V2 daemon 与 mihomo 运行
adb shell "su 0 -c '/data/adb/f50mgr/bin/f50mgr status'"
# 判据：daemon: running 且 mihomo: running

# V3 面板与 mihomo API 监听（14180=0x3764，14193=0x3771，192.168.0.1 小端=0100A8C0）
adb shell "su 0 -c 'grep -c 0100A8C0:3764 /proc/net/tcp'"   # 期望 ≥1
adb shell "su 0 -c 'grep -c 0100A8C0:3771 /proc/net/tcp'"   # 期望 ≥1

# V4 TPROXY 规则已上线
adb shell "su 0 -c '/data/adb/f50mgr/bin/f50mgr iptables status'"
# 判据：输出 "iptables: active"

# V5 面板可达（从连接 F50 热点/USB 网络的电脑上）
curl -s -o /dev/null -w '%{http_code}' http://192.168.0.1:14180/health   # 期望 200
# 随后浏览器打开 14180，使用安装时保存的管理密钥登录；状态卡片应正常刷新
```

### 日志位置（v0.2.8+，排障第一入口）

```text
/data/adb/f50mgr/run/f50mgr.log     daemon 主日志（启动链路、门控、watchdog 动作、上轮退出码）
/data/adb/f50mgr/run/mihomo.log     Mihomo 输出（warning 级起）
/data/adb/f50mgr/run/easytier.log   EasyTier 输出（warn 级起）
```

体积有上界（5MB 超限自动轮转/截断），放心 tail。任何分支拿不准时先看
`f50mgr.log` 最后 30 行。

### 验证失败决策树

- **V2 daemon 不在跑** → `adb shell "su 0 -c 'ls /data/adb/modules/f50mgr/service.sh'"`
  存在则等 30s 重试（service.sh 循环 5s 重拉）；不存在则模块未装成功，回第 3 步。
  反复重启则看 `f50mgr.log` 里的 `daemon exited rc=` 行定位死因。
- **V2 daemon 在跑但 mihomo 不在跑** → 大概率缺订阅配置（config.yaml 不存在），
  这是**预期状态**——先完成第 5 步订阅导入，watchdog 会自动拉起。
- **V4 显示 inactive 且 mihomo 未运行** → 正常（就绪门控：mihomo API 可连之前
  绝不上规则，宁可直连不可黑洞）。完成订阅导入后 ≤60s 自动 active。
- **V4 显示 inactive 但 mihomo 在跑** → 等 60s 复查（watchdog 30s 周期）；仍
  inactive 则 `adb shell "su 0 -c 'nc -w 2 192.168.0.1 14193 </dev/null; echo rc=$?'"`
  检查 API；rc≠0 说明 mihomo 未就绪，看第 6 步排障。
- **V5 curl 超时但 V3 通过** → 电脑到 192.168.0.1 的链路问题（未连 F50 热点/网线），
  与模块无关。
- **刚启动后国内外探测短暂全失败** → 等 Mihomo DNS/代理组热起来（1-2 分钟），
  属已知现象，不是故障。

## 5. 订阅配置（安装后必做一次）

推荐面板操作：浏览器打开 `http://192.168.0.1:14180` → 使用安装时保存的管理密钥登录
→ 订阅区域 → 导入 URL。
面板会下载订阅、生成 F50 专用 config.yaml（强制收敛端口/DNS/sniffer）、保存
URL 到 secrets.toml（chmod 600）并重启 Mihomo。

CLI 等价（AI 注意：URL 含特殊字符时整体双引号包裹）：

```bash
adb shell "su 0 -c '/data/adb/f50mgr/bin/f50mgr subscription update \"https://your-sub-url\"'"
# 判据：退出码 0；随后 f50mgr status 显示 mihomo: running
```

验收：管理页网络诊断显示国内外站点探测结果；热点客户端可正常访问国内外网站。

## 6. 常见故障速查

| 现象 | 首查命令 | 常见根因与处置 |
|---|---|---|
| 客户端完全无网 | `f50mgr iptables status` | 若 active 且 mihomo 不健康属异常（v0.2.6+ 会自动 fail-open 转直连，等 60-90s）；持续黑洞请 `iptables disable` 后收集信息报 issue |
| 代理不生效（直连） | 同上 | inactive + mihomo 健康 → 等 30s watchdog 自动 enable；一直不上线查订阅是否导入 |
| mihomo 反复重启 | `f50mgr status` 连续观察 pid | 订阅配置错误或端口被占；v0.2.7 起「进程活着但 API 不通」也会被主动重启（预期行为） |
| 上游断网恢复后代理没回来 | `nc -w 2 192.168.0.1 14193` | v0.2.7 已自动处理（聋态检测 ~50s 重启 + 规则自动恢复）；旧版本需手动 killall mihomo |
| 装完重启后无变化 | `ls /data/adb/f50mgr/bin/f50mgr` | customize.sh 未执行成功；重装模块并保留安装日志 |

## 7. 卸载

Magisk App 中移除模块并重启即可；uninstall.sh 会撤除 iptables 规则
（含 IPv6 guard）并终止 f50mgr/mihomo/easytier 进程。运行数据目录
`/data/adb/f50mgr` 会一并清理。

---

- 端口表 / 日常运维命令：`docs/deployment.md`
- 设备约束与已知问题：`docs/device-notes.md`
- 本文档由 AI 依照 2026-07-16 真机验证结果编写；EasyTier mesh 配置为可选
  高级功能，见 `docs/deployment.md`「EasyTier mesh VPN 配置」。
