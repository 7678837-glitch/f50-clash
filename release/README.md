# Release 制品说明

`release/` 保存本机构建和 GitHub Release 的输出；生成物默认不进入 Git。

## 标准输出

运行 `./build.sh` 后生成：

```text
f50mgr-<VERSION>.zip
f50mgr-<VERSION>.zip.sha256
```

GitHub 标签发布还会附加：

```text
f50mgr-<VERSION>.spdx.json
mihomo-a84724665eb7-source.tar.gz
easytier-4c4d172e2f0b-source.tar.gz
third-party-sources.sha256
```

其中 SPDX JSON 是对实际 Magisk ZIP 的软件物料清单；两个源码归档对应安装包中
Mihomo GPL object code 和 EasyTier LGPL object code 的精确上游提交。

## 构建与校验

```bash
./build.sh
VERSION="$(tr -d '[:space:]' < VERSION)"
# Linux；macOS 使用 shasum -a 256 -c
(cd release && sha256sum -c "f50mgr-${VERSION}.zip.sha256")
```

正式构建会拒绝脏工作树、版本不一致、旧提交二进制、第三方哈希不匹配或缺少许可证
文件。ZIP 内时间戳和文件顺序固定，同一提交和输入应产生相同归档哈希。

## 发布规则

- 标签必须严格等于 `v<VERSION>`，例如 `v20260718.1`。
- Release 工作流先运行 Rust、shell、OSV 和完整 Git 历史密钥扫描。
- 构建后再次扫描实际 ZIP 的嵌套内容，并校验 ZIP 与对应源码归档 SHA-256。
- GitHub Actions 和 SBOM 工具均固定到不可变提交或版本。
- Release ZIP、SBOM 和源码归档是生成物，不应通过 `git add -f` 写入源码历史。

安装、回滚和真机验收见 [部署与运维手册](../docs/deployment.md)。
