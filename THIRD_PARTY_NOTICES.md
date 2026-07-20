# 第三方软件声明

Copyright (C) 2026 xjoker

f50-clash 自身代码采用 GNU General Public License v3.0 only（SPDX：
`GPL-3.0-only`），完整条款见根目录 `LICENSE`。

Magisk 安装包是多个相互独立程序和静态资源的集合。各第三方组件继续适用
其自身许可证；将它们放入同一安装包不改变各自的许可证。

## Mihomo

- 项目：https://github.com/MetaCubeX/mihomo
- 固定版本：v1.19.24
- 对应源码提交：`a84724665eb7f989809abe463c05f5723bd24975`
- 许可证：GNU GPL v3.0
- 本地用途：Android ARM64 透明代理内核
- 修改：无；本地文件与官方 Release 压缩包解压结果 SHA-256 完全一致
- 对应源码：https://github.com/MetaCubeX/mihomo/tree/a84724665eb7f989809abe463c05f5723bd24975

GNU GPL v3.0 完整条款见根目录 `LICENSE`。发布 Mihomo object code 时，
f50-clash Release 必须同时提供上述精确提交的机器可读源码归档，不能只依赖
上游链接长期存在。

## EasyTier

- 项目：https://github.com/EasyTier/EasyTier
- 固定版本：v2.4.5（运行时报告 `2.4.5-4c4d172e`）
- 对应源码提交：`4c4d172e2f0b345b42102969ce4a98e936ca1d81`
- 许可证：GNU LGPL v3.0
- 本地用途：独立的 `easytier-core` 与 `easytier-cli` 可执行文件
- 修改：无；两个本地文件均与官方 Release 压缩包对应成员逐字节一致
- 对应源码：https://github.com/EasyTier/EasyTier/tree/4c4d172e2f0b345b42102969ce4a98e936ca1d81

GNU LGPL v3.0 条款见 `third-party/licenses/LGPL-3.0.txt`；其引用的
GNU GPL v3.0 条款见根目录 `LICENSE`。发布 EasyTier object code 时同时
提供上述精确提交的源码归档。

## MetaCubeXD

- 项目：https://github.com/MetaCubeX/metacubexd
- 固定版本：v1.269.0
- 对应源码提交：`6802999f204fdb7a97b41ade2f49693ed5eab8c9`
- 许可证：MIT
- 本地用途：Mihomo 高级管理面板静态资源
- 修改：仅将 `config.js` 的 `defaultBackendURL` 从空字符串改为
  `window.location.origin`，使面板默认连接当前 F50；其余文件与官方
  `compressed-dist.tgz` 完全一致

MetaCubeXD 的许可证原文如下：

```text
MIT License Copyright (c) 2023 MetaCubeX

Permission is hereby granted, free of
charge, to any person obtaining a copy of this software and associated
documentation files (the "Software"), to deal in the Software without
restriction, including without limitation the rights to use, copy, modify, merge,
publish, distribute, sublicense, and/or sell copies of the Software, and to
permit persons to whom the Software is furnished to do so, subject to the
following conditions:

The above copyright notice and this permission notice
(including the next paragraph) shall be included in all copies or substantial
portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF
ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF
MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO
EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR
OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING
FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN
THE SOFTWARE.
```

## GeoIP、GeoSite 与 MMDB 数据

当前 Git 跟踪树和当前安装脚本没有分发 GeoIP、GeoSite、ASN.mmdb 或
Country.mmdb。后续若加入这些数据，必须先单独记录生成项目、原始数据源、
版本、许可证和 SHA-256；来源条款未核清前不得进入公开 Release。

## 可机器核验的来源

所有已确认的下载地址、压缩包哈希、安装后文件哈希和修改状态记录在
`third-party/assets.toml`。构建阶段必须先校验上游压缩包，再校验提取后的
目标文件；任一不一致都应立即失败。
