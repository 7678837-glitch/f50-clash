## 变更说明

<!-- 说明解决的问题、实现边界和用户可见影响。 -->

## 验证证据

<!-- 列出真实执行的命令与结果；未运行的项目明确写“未运行”。 -->

- [ ] `cargo clippy --locked --target <host> --all-targets -- -D warnings`
- [ ] `cargo test --locked --target <host> -- --test-threads=1`
- [ ] `bash tests/version-contract.sh`
- [ ] `bash tests/easytier-pin.sh`
- [ ] `bash tests/migration-contract.sh`
- [ ] `bash tests/release-build-contract.sh`
- [ ] `bash tests/open-source-contract.sh`
- [ ] 涉及设备网络/安装/迁移时，已附 F50 与热点客户端真机结果

## 风险与恢复

<!-- 说明失败时如何恢复；不涉及则写“不涉及持久数据或网络规则”。 -->

## 提交确认

- [ ] PR 只包含一个可独立审查、回滚和部署的关注点。
- [ ] 没有提交真实 secrets、设备备份、Release ZIP 或敏感日志。
- [ ] 日志和截图已删除 IMEI、ICCID、IMSI、手机号、订阅 URL、节点和密钥。
- [ ] 新增行为先有失败测试；纯文档、配置或 UI 变更已说明测试不适用原因。
- [ ] 第三方版本或资产变更已同步许可证、来源、哈希和契约测试。
