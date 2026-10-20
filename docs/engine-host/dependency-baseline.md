# T0 依赖基线修复及政策评审

第二轮 CI（run 39360307473）成功加载公告库后发现以下原有依赖问题。本文件与独立提交一起审核，不将扫描工具升级等同于依赖已安全。

## D20：LRU 与 TLS

- 原 lru 0.12.5：RUSTSEC-2026-0002、2026-0253，涉及 IterMut 和 pop 的内存安全；升级至锁定 0.18.5（要求 >=0.18.5），沿用原 web_fetch 缓存，不复制实现。
- 原 rustls 0.23.43：RUSTSEC-2026-0285、0346、0348、0349；锁定 0.23.46，保留 reqwest/ring 的现有配置，不另增 TLS backend，也不关闭证书验证。
- 复用 web_fetch/Provider 测试、全 workspace 测试、Clippy 和 MSRV 1.88 验证。最终公告覆盖以当前 CI cargo-deny 的实际结果为准。

## D21：归档的 YAML 后端

serde_yml 0.0.12 / libyml 已归档且存在 RUSTSEC-2025-0068，没有安全升级版本。改用公告推荐的维护分支 serde_norway 0.9.42 / unsafe-libyaml-norway 0.2.15，许可证 MIT OR Apache-2.0；不新增手写解析器或第二套 Store。

保留 workspace 的 serde_yml 依赖键作为 Cargo package alias，既有模块不做纯命名搬迁。实际包、锁文件和审计均为 serde_norway，不能宣称仍在使用原包。frontmatter 的业务字段、Markdown 分隔符和 JSONL 格式不变；现有数据无需批量重写，只有正常保存时重新序列化。

**公开类型兼容影响：**Memory/Skill/Goal 的 extra Mapping、frontmatter 的 Error 等类型身份变为 serde_norway 的类型。Rust 下游若直接依赖旧 serde_yml 并传这些类型，需要将其 Cargo 依赖改为同一 package alias，或使用新的类型；这是需要评审的源码兼容调整，不能以“键名不变”宣称二进制/类型完全兼容。core trait 本身不变。回退必须同时回退 manifest 和锁文件。

复用原 Store/Memory/Skill/Goal/Reflect 测试和所有原 golden（含未知字段、BOM/CRLF、写读写稳定性）。不更新 golden 来迎合新后端；实际结果见 validation.md。

