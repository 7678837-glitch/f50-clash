# 开源前审计记录

首次审计：2026-07-19 02:21:49 +08
发布前复审：2026-07-20 13:39:00 +08
复审基线提交：`9ee12039525570fca8bf1631400b06a83ad4aed1`

## 秘密扫描

使用官方 Gitleaks v8.30.1 macOS ARM64 发布包，下载包 SHA-256：

```text
b40ab0ae55c505963e365f271a8d3846efbc170aa17f2607f13df610a9aeb6a5
```

完整 Git 历史扫描结果：

```text
79 commits scanned
no leaks found
```

同一锁定扫描器递归检查复审基线生成的 `release/f50mgr-20260720.1.zip`：
扫描约 7.41 MB，
没有发现泄漏；ZIP SHA-256 为
`766d2dacc9ab82b55d1c65d342f038d8aa517acce76ebc9d02e2cdf727a3373d`。

工作目录扫描发现 20 项候选，全部位于被 `.gitignore` 明确排除的
`device-backups/` 真机备份中。它们不属于项目源码或公开交付物，不删除、
不移动、不提交。公开构建上下文必须继续排除：

- `device-backups/`
- 任意目录下的 `secrets.toml`
- 本地 `release/*.zip`

发布 CI 通过 `scripts/ci-secret-scan.sh` 重新扫描完整 Git 历史，并在构建后启用
归档递归扫描实际 Release ZIP；只有两者均为 0 项有效泄漏时才允许创建 Release。

## Git 作者隐私

现有私有 Gitea 历史的作者元数据包含个人邮箱。用户禁止 force push，因此
不改写该历史。GitHub 公共主仓采用清洁的首个公开快照，并使用 GitHub
noreply 作者地址；旧 Gitea 仅作为私有历史归档。后续公开提交在两个远端
同步，但不会把旧私有历史推到 GitHub。

## 第三方来源核验

- Mihomo：官方 v1.19.24 压缩包哈希与 GitHub Release digest 一致，解压后
  与本地 `mihomo.fallback` SHA-256 完全一致。
- EasyTier：官方 v2.4.5 压缩包哈希与 GitHub Release digest 一致，其中
  `easytier-core`、`easytier-cli` 与本地文件逐字节一致；F50 真机报告
  `easytier-core 2.4.5-4c4d172e`。
- MetaCubeXD：官方 v1.269.0 压缩包哈希与 GitHub Release digest 一致；
  解包目录比较仅 `config.js` 不同，本地改动是同源默认连接配置。
- 当前没有分发 geodata；不能把未知来源数据补进 Release。

完整字段和 SHA-256 见 `third-party/assets.toml`。
