# f50-clash 改良路线图（提案）

> 状态：**提案，待拍板**。2026-07-16 基于 v0.2.7 代码调研产出。
> 规模：S=<半天 / M=1-2 天 / L=多天。
> 标注 ⚖️ 的条目含选型级决策，实施前必须由维护者确认。
>
> **历史说明（2026-07-18）：** 本文保留最初问题和决策背景，不再作为当前状态源。
> O1/O2/O4/O5、日志与 watchdog 可观测性、EasyTier 面板和 Phase 7 可逆迁移均已落地；
> 当前能力以根目录 README、源码和 CI 契约为准。

## 决策摘要（需要拍板的事）

| # | 决策 | 影响 |
|---|---|---|
| ⚖️ O1 | LICENSE 选型（MIT/Apache-2.0/GPL-3.0） | 受 mihomo(GPL)/geodata 许可传染约束，是开源硬前置 |
| ⚖️ O3 | 构建产物（f50mgr 2.6M、MetaCubeXD 2.4M）是否移出 git | 改变「clone 即有二进制」工作流 |
| ⚖️ F4 | 是否要多设备统一视图 | 与单设备自治的简洁定位冲突，默认不做 |
| ⚖️ F2 | 面板内切节点 vs 继续依赖 MetaCubeXD | 产品定位问题 |

---

## 一、健壮性 / 运维强化

### R1. 落盘 mihomo + daemon 日志（当前全部丢失）【最高优先】
- **做什么**：把 daemon 的 tracing 输出和 mihomo 子进程 stdout/stderr 落到 `run/` 下的受控日志文件。
- **为什么**：现状日志无处可查。`init_logging()` 只写 stdout，mihomo 用 `Stdio::inherit()` 继承 daemon 的 stdout，而 daemon 由 service.sh 开机循环拉起、stdout 被 Magisk 丢弃 → **daemon 与 mihomo 全部日志蒸发**。文档声称的 `run/mihomo.log` 从未被创建。2026-07-16 排查「mihomo 活着但聋了」事故时正因无日志而只能黑盒推断。
- **怎么做**：mihomo spawn 的 Stdio 指向 `run/mihomo.log` 文件句柄，每次 spawn 前按大小截断（>5MB 轮转一份 .1）；daemon 侧 tracing 同时写 stdout + `run/f50mgr.log`（同样按大小轮转），遵循全局规范「rotation 交应用内 file appender」。
- **规模**：S-M　**风险**：日志可能含订阅域名等信息，脱敏与 R2 同源。

### R2. 一键诊断快照导出
- **做什么**：新增 API/CLI，一次性打包 status + iptables-save + ip rule/route + mihomo 探测 + 日志尾部，供面板下载。
- **为什么**：排障目前要人工逐条敲 adb；`device-backups/*/snapshots/` 已证明这类快照的价值但纯手工。是 R3/F1/A4 的公共底座。
- **规模**：M　**风险**：secrets（订阅 URL、easytier secret）绝不能进快照，须显式过滤。

### R3. 崩溃现场自动收集
- **做什么**：mihomo 聋态强制重启触发时、daemon 异常退出后首次启动时，自动 dump 一份 R2 快照到 `run/crash-<ts>.txt`。
- **为什么**：service.sh 重启循环会掩盖崩溃死因。注意 `panic = "abort"` 下进程内 hook 不可靠，daemon 自身崩溃须由 service.sh 兜底采集。
- **规模**：M（依赖 R2）　**风险**：崩溃风暴时需节流。

### R4. watchdog 状态可观测化
- **做什么**：把 mihomo watchdog 的重启次数、健康失败计数、上次强制重启时间纳入 `/api/status`。
- **为什么**：v0.2.7 的聋态自愈对用户完全不可见，无法判断代理是否在反复重启。
- **规模**：S-M　**风险**：跨任务共享状态注意锁粒度。

## 二、功能增强

### F1. 面板内日志查看器（依赖 R1）
读取 `run/*.log` 尾部实时显示，员工排障不依赖 adb。规模 S。风险：与 R2 同源脱敏。

### ⚖️ F2. 面板内节点/策略组切换
经 14193 REST API 直接切换，免跳 MetaCubeXD。规模 M。MetaCubeXD 已覆盖该功能，价值取决于「员工极简模式」定位——待拍板。
（注：CLAUDE.md 提到的 `src/web/proxy.rs` 实际不存在，为未落地设计。）

### F3. 订阅更新可视化
面板显示上次更新时间/结果、下次自动更新倒计时。规模 M。多订阅源按 YAGNI 原则先不做。

### ⚖️ F4. 多设备统一视图（默认不做）
聚合多台 F50 状态。EasyTier mesh 是天然底座，但与单设备自治定位冲突且规模 L——除非有真实运维诉求。

## 三、开源就绪

### ⚖️ O1. LICENSE 选型【开源硬前置】
仓库无 LICENSE，`Cargo.toml` 为 `license = "UNLICENSED"`（明确禁止再分发）。选型受 mihomo/geodata/MetaCubeXD 许可约束，须维护者拍板。规模 S。

### O2. 内置二进制的再分发合规 + 可复现获取【开源硬前置】
`mihomo.fallback`/`easytier.fallback`/`geodata/` 被 .gitignore 忽略**不在仓库里**，但 customize.sh 安装依赖它们 → 开源后别人 clone 装出的模块**没有代理内核，完全不可用**。方案：`scripts/fetch-binaries.sh` 按 pin 版本从官方 release 下载 + sha256 校验，文档标注版本与许可。规模 M。

### ⚖️ O3. 构建产物移出 git
`magisk-module/bin/f50mgr`（2.6M）与 `assets/zashboard.tar.gz`（历史文件名，内容为 MetaCubeXD 2.4M）已入库，每次 build 膨胀仓库。改走 GitHub Release 附件 + fetch 脚本，与 O2 统一。规模 S（决策后机械操作）。

### O4. CI + Release 自动化
无 `.github/`。GitHub Actions：PR 跑 clippy/fmt/test；tag 触发容器交叉编译（复用 build.sh + Dockerfile）+ 打包 + sha256 + Release。规模 M。

### O5. secrets 模板 + 贡献指南 + README 国际化
secrets.toml 结构目前只藏在 customize.sh 内联模板里；补 `secrets.toml.example`、CONTRIBUTING.md、README 快速开始。规模 S-M。

## 四、AI 引导安装 SOP（核心创新点）

### A1. INSTALL-AI.md —— 面向 AI 助手的结构化安装指导【核心交付】
专供「喂给 AI 助手、由 AI 引导用户完成安装」的文档：前置探针清单、逐步命令、每步机器可验证判据、故障决策树。现有 deployment.md 是人读手册，不满足 AI 逐步执行+判据校验的结构。规模 M。
**初版已随本提案落地：`docs/INSTALL-AI.md`**（见 A2-A5 要素）。

### A2. adb 引号规约（硬规约）
`adb shell su 0 -c 'A; B'` 的引号被本机 shell 消费，只有 A 以 root 运行、B 落到 shell 用户 → Permission denied。canonical 形态：多命令一律 `adb shell "su 0 -c '...'"` 双引号包裹。2026-07-16 真机踩坑定案，已写入 INSTALL-AI.md 顶部。

### A3. 前置检查清单（机器可验证）
adb 连通 / root / Magisk 版本 / 设备型号 / LAN IP / 端口占用，每项 = 命令 + 期望输出判据 + 失败处置分支。

### A4. 安装后验证决策树
daemon/mihomo running？API 200？proxy_health 正常？每个否分支给明确修复动作。复用 reboot-stability.md 异常处理经验 + 真机实测时序（web t+12~14s、API+规则 t+20~22s）。

### A5. 现有文档复用映射
deployment.md 命令块 → A1 素材；reboot-stability.md 异常处理 → A4 叶子；device-notes.md → A3 判据。注意：CLAUDE.md 声称的 Phase 7「box_for_root 迁移工具」未实现，SOP 只能引导手工清理。

## 交叉依赖与建议顺序

1. **R1 日志落盘**（一切排障的地基，本身 P0）
2. **R2 诊断快照**（R3/F1/A4 底座）
3. **A1 INSTALL-AI.md 初版**（可先出不含日志分支的版本，随 R1/R2 迭代）
4. ⚖️ **O1+O2**（开源前必须，等许可选型拍板）
5. O4/O5/F3/R4 按需排期
