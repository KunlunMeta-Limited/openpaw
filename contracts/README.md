# OpenPaw B 线机器契约

本目录保存跨模块或跨项目接口的 JSON Schema。Schema 不是需求文档的替代物；字段的中文含义、
数据流、失败语义和版本兼容规则必须同时出现在 `docs/governance/项目设计基线.md` 和有效 CHG 中。

当前契约：

- `host-session-status-v1.schema.json`：Host `GET /session/status` 的最小响应形状。

修改契约的顺序固定为：先改需求/设计基线和 CHG -> 再改 Schema/fixtures -> 再改 Rust/TypeScript
实现 -> 运行契约和集成测试。Schema 中 `$id`、`title`、`description` 和字段 description 必须
包含中文说明；CI 会检查。
