# F50 重启稳定性测试

## 测试目标

验证 F50 多次重启后 f50mgr、Mihomo、14180 面板、Mihomo API、国内外代理探测
是否能自动恢复到可用状态。

## 验证项目

每轮重启后检查：

- `sys.boot_completed = 1`
- `/data/adb/f50mgr/bin/f50mgr status` 显示 daemon 和 Mihomo 均运行
- `http://192.168.0.1:14180/api/status` 返回 200
- `mihomo_api.available = true`
- `proxy_health.summary = 国内外正常`
- 国内站点和国外站点探测均为 `true`

## 当前记录

| 时间 | 轮次 | 结果 | 备注 |
|---|---:|---|---|
| 2026-05-12 12:24 | 1 | 通过 | `boot_completed=1`，`bootanim=stopped`，daemon/Mihomo/API 正常，国内外正常 |
| 2026-05-12 12:25 | 2 | 通过 | `boot_completed=1`，`bootanim=stopped`，daemon/Mihomo/API 正常，国内外正常 |
| 2026-05-12 12:25 | 3 | 通过 | `boot_completed=1`，`bootanim=stopped`，daemon/Mihomo/API 正常，国内外正常 |

最终复查：

```text
f50mgr 0.1.0
daemon: running
pid: 973
mihomo: running
mihomo_pid: 2520
```

14180 API 摘要：

```text
daemon=true
mihomo=true
mihomo_api=true
proxy_health=国内外正常
domestic=true
foreign=true
```

## 异常处理

- 若 ADB 已恢复但 14180 暂不可达，先等待 10-20 秒再复查。
- 若设备本机有监听但电脑访问失败，优先排查电脑到 `192.168.0.1` 的链路。
- 若 `proxy_health` 首次短暂失败，等待 Mihomo DNS 和代理组热起来后复查。
