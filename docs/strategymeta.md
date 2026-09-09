# Strategymeta

Bilingual terminology single source of truth (engineering plan §M0; design
doc §9.2). Entries are added the first time a term is used in a deliverable.

| Term | 中文 | Definition / binding source |
|------|------|------------------------------|
| same-source artifact | 同源件 | A file/schema whose authoritative copy lives in OpenPaw; upstream consumes by reference. Registry: `docs/same-source-registry.md` |
| core nine | 核心九项 | The 9 published crates whose business-line budgets sum to exactly 50,000 |
| aux budget | 测试与辅助预算 | 10,000-line cap covering all tests + xtask |
| unverified | 未验证标记 | Frontmatter flag forced on every reflect-lite artifact; provenance of the evolution loop's honesty |
| goal ledger | 目标存折 | Append-only JSONL progress ledger for a goal profile |
| generation gap | 代差红线 | Features deliberately absent from reflect-lite: quality gate, curation merge, phase signals, multi-signal triggers |
| three-phase execution | 三阶段执行 | Tool dispatch: Deny→error / Allow→parallel / Confirm→sequential approval |
| E1 / E2 | 分级移植 | Engine staging: E1 = loop skeleton + tool paths; E2 = compaction/fallback/budget depth |
| cross-replay harness | 对拍测试 | Same dialogue script run on both codebases with the same injected mock provider; structure-only comparison keys |
| scrub | 移植卫生扫描 | Banned-word scan (Q10): upstream identifiers, codenames, task IDs, internal doc names/domains |
| human alignment floor | 人工对齐底线 | `EvolutionPolicy::requires_human_alignment()` defaults to `true`; CI-asserted (Q9) |
| zero telemetry | 零遥测 | No telemetry SDK in the binary; no `[telemetry]` config section — the entry point itself does not exist |
| MCP client host | MCP 客户端宿主 | `openpaw-mcp`: OpenPaw speaks MCP as a client — `[mcp.servers]` config, stdio + plain-http transports, remote tools mount as `mcp__<server>__<tool>`; crate card §3.5 |
| circuit breaker | 熔断器 | Per-server Closed/Open/HalfOpen state machine; Open rejects without a round trip, one HalfOpen probe after the cooldown; tool-level `is_error` never feeds it |
| MSRV floor | MSRV 底线 | Lowest rustc the locked tree compiles on, enforced per-release by the `msrv` CI job; bumps are minor releases with a changelog entry (compatibility.md §2) |
