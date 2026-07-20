# f50-clash 项目规范

## 项目定位

专为 ZTE MU300/F50 移动热点路由器设计的 Magisk 模块，用 Rust 编写，完整替代 box_for_root，为出差员工提供全自动透明代理服务。

## 技术栈

- 语言：Rust（交叉编译目标：`aarch64-linux-android`，NDK r26+）
- Web 框架：Axum + Tokio
- 前端资源嵌入：rust-embed
- 代理内核：Mihomo (clash meta)
- 打包：Magisk 模块 (.zip)

## 目录结构

```
f50-clash/
├── src/                        Rust 源码
│   ├── main.rs
│   ├── process/mihomo.rs       子进程管理 + watchdog
│   ├── iptables/rules.rs       F50 专用规则集（iptables-restore 模式）
│   ├── download/
│   │   ├── github.rs           GitHub releases API
│   │   └── fileserver.rs       公司文件服务器（Basic Auth + HTTPS）
│   ├── subscription/mod.rs     订阅下载 + config.yaml 模板生成
│   ├── web/
│   │   ├── router.rs
│   │   ├── api.rs              /api/* 管理接口
│   │   └── proxy.rs            反代 Mihomo REST API
│   └── assets/                 rust-embed 嵌入的前端
│       ├── index.html          极简状态页（员工用）
│       └── metacubex/          MetaCubeX dist（高级模式）
├── magisk-module/
│   ├── module.prop
│   ├── service.sh              开机拉起 daemon（重启循环，daemon 死后自动拉起）
│   ├── customize.sh            安装时准备运行目录 + 安装二进制/兜底资源
│   ├── uninstall.sh
│   └── bin/
│       ├── mihomo.fallback     内置兜底 Mihomo ARM64 二进制
│       └── geodata/            内置兜底 GeoIP/GeoSite 数据
├── build.sh                    NDK 交叉编译 + 打包脚本
├── docs/
│   ├── architecture.md
│   ├── device-notes.md
│   └── deployment.md
└── CLAUDE.md                   本文件
```

## 设备运行时目录（F50 上）

```
/data/adb/f50mgr/
├── bin/
│   └── mihomo                  实际运行版本（与 module/bin/mihomo.fallback 路径隔离）
├── config/
│   ├── f50mgr.toml             主配置
│   ├── secrets.toml            订阅URL/文件服务器密码（chmod 600，不提交git）
│   └── clash/
│       └── config.yaml         生成的 Mihomo 配置（由工具自动生成，不手动编辑）
├── data/
│   ├── GeoIP.dat
│   ├── GeoSite.dat
│   ├── ASN.mmdb
│   └── Country.mmdb
└── run/
    ├── f50mgr.pid
    ├── mihomo.pid
    └── mihomo.log
```

## 关键端口（已确认无冲突）

| 用途 | 端口 | 说明 |
|---|---|---|
| Mihomo tproxy | 14198 | 透明代理拦截 |
| Mihomo DNS | 14153 | fake-ip DNS 监听 |
| Mihomo HTTP proxy | 14191 | HTTP 代理端口 |
| Mihomo external-controller | 14193 | REST API（绑 192.168.0.1，LAN 可达；MetaCubeXD 面板依赖） |
| f50mgr Web UI | **14180** | 管理面板（对 LAN 开放），硬编码于 main.rs |

冲突端口（ZTE 系统服务占用，禁止使用）：9090（com.zte.web）、9999（NetworkStats）、8080（com.zte.web）

> 所有 f50mgr/Mihomo 端口统一使用 14xxx 范围。`f50mgr.toml` 的 `[web] listen` 字段当前未被读取，实际监听地址由 `main.rs` 常量决定。

## 开发规范

- 简体中文注释和日志
- 单次变更 ≤300 行，≤6 文件
- 每个 Phase 在 F50 真机验证后再进下一个 Phase
- iptables 规则必须覆盖 5 个场景测试：enable/disable/迁移/重启/网络切换

## 交付阶段

| Phase | 内容 | 验证方式 |
|---|---|---|
| 1 | Rust 框架 + service.sh + 健康检查 | adb 确认 daemon 启动 |
| 2 | iptables 规则集（iptables-restore 原子应用） | 热点客户端访问测试 |
| 3 | Mihomo fork/exec + watchdog | 手动 kill mihomo 验证自重启 |
| 4 | 订阅下载 + config.yaml 模板 | 订阅更新 + DNS 功能测试 |
| 5 | Web UI（状态页 + MetaCubeX） | 浏览器访问 192.168.0.1:14180 |
| 6 | 自动更新（GitHub → 文件服务器 → 本地兜底） | 模拟各种网络失败场景 |
| 7 | box_for_root 迁移工具 | 在已有 box_for_root 的 F50 上全流程测试 |
