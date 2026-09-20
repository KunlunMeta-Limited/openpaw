# OpenPaw 治理入口

本目录只登记项目适配信息，不保存 B 线业务规则。通用规则位于
`D:/Workspace/_bak/test/framework`，正式使用时通过 `AI_GOVERNANCE_FRAMEWORK` 固定版本。

先执行：

```text
python governance/check.py validate --profile governance/project-profile.toml
python governance/check.py contracts --dir contracts
python governance/check.py index --changes docs/changes --output docs/changes/INDEX.md
```

当前接入处于反向扫描阶段，CHG 为 `草案（draft）`，不能生成编码任务包。以后人审核 CHG
后再由人执行：

```text
python governance/check.py approve --change docs/changes/CHG-2026-0001-治理接入.md --by "真实姓名"
```

Git 建立后，PR 必须执行 `pr-check`、`comments-diff` 和 profile 中对应的格式、测试、依赖
命令；未登记文件、未批准 CHG 或保护路径缺少 CODEOWNER 审查都应阻断合并。
