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

## D22：既有许可证与政策不一致（待 PR 审核）

扫描发现两个早已进入基线的传递依赖不在 deny.toml 允许列表内。建议保留现有依赖，增加精确包版本的许可证例外，不把许可证加入全局允许列表：

| 包 | 依赖来源 | 例外范围 | 交付义务 |
|---|---|---|---|
| option-ext 0.2.0 | dirs 5 → dirs-sys 0.4.1 | 仅该版本的 MPL-2.0 | 保留许可证和上游源码获取说明；若修改该包须按许可证提供相应源码 |
| webpki-roots 1.0.9 | reqwest → hyper-rustls | 仅该版本的 CDLA-Permissive-2.0 | 随分发保留数据许可证全文 |

完整原文保存在 third-party-licenses/，THIRD-PARTY-NOTICES.md 记录包、来源及校验值。T8 发布必须将 notices 和该目录随二进制一起分发。本 PR 不修改这两个包的源码。不为消除许可证提示重写 home 目录发现或 TLS 信任根。

这是显式的政策修改提案，需审核人认可后随 T0 合入；不扩大到其他包/版本，不忽略安全公告，不降低置信度或关闭检查。若不接受该范围，应保持 PR Draft 并另行设计依赖替换，不能删除失败门禁。

## T2/D35：2026-10-21原TLS依赖最小补丁

全量扫描实际命中[RUSTSEC-2026-0382](https://rustsec.org/advisories/RUSTSEC-2026-0382.html)与[RUSTSEC-2026-0381](https://rustsec.org/advisories/RUSTSEC-2026-0381.html)：旧验证器可能接受被撤信的emSign中间公钥路径，旧根数据仍包含相关根。按CHG-2026-0003先登记再修复，使用Cargo生成的包条目，仅更新rustls-webpki **0.103.15→0.103.16**、webpki-roots **1.0.9→1.0.10**及其checksum；不连带更新其他依赖边、不切TLS后端、不忽略公告、不关闭证书验证。原Provider、HTTP工具和Host共用同一TLS链。

已从实际下载的0.103.16包核对ISC式许可证；roots1.0.10实际LICENSE为CDLA-Permissive-2.0，全文SHA-256仍为`e271993808fec50ab29350b39539cdec611a9103f827e0aa26d61da70e2d33f8`，与旧原文相同。新原文单独保存，旧版本资料保留；deny.toml仅将原精确例外移至1.0.10，通知同步新包来源，不增加全局允许项。新版本例外仍随T2接受非作者审核，不自填批准。

无用户数据或公共Rust类型迁移。完整workspace、MSRV1.88和安全扫描必须针对最终补丁重新运行；结果以CHG专用验证记录为准。不要仅回退这两个补丁来获得旧构建，回退会重新暴露上述公告；旧二进制退出使用后保留数据备份。
