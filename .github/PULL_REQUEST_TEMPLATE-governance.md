# 受控变更 PR

## 变更单

- CHG 文件：
- CHG 状态：必须是 `approved` 或 `in-progress`
- 设计基线：
- 目标分支：

## 本次实际修改

逐项说明实际 diff 中每个新增、修改、删除或重命名文件与 CHG 表的对应关系；未登记文件必须先退回更新 CHG。

## 验证证据

| 命令 | 退出码 | 结果或报告地址 |
| --- | ---: | --- |
| `cargo fmt --all -- --check` |  |  |
| `cargo clippy --workspace --all-targets -- -D warnings` |  |  |
| `cargo test --workspace` |  |  |
| `cargo run -p xtask -- lines` |  |  |
| diff 范围与中文 Rustdoc 人工核对 |  |  |
| 与本次风险等级对应的测试 |  |  |

## 审核确认

- [ ] 未修改 CHG 禁止路径或未登记文件
- [ ] 公共 API、字段、事件和依赖变化已在 CHG 逐项说明
- [ ] 新增/修改公共 Rust item 使用中文标准 Rustdoc
- [ ] 回滚方案和兼容方案可执行
- [ ] L3/L4 CODEOWNER 已审核
- [ ] 未执行项已说明原因；没有用 skip 冒充通过
