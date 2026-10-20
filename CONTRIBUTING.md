# Contributing to OpenPaw

Thank you for considering a contribution.

## Developer Certificate of Origin (DCO)

All contributions must be signed off under the DCO. By signing off you
certify that you wrote the change or have the right to submit it under the
project license. Add a sign-off line to each commit message:

```
Signed-off-by: Your Name <you@example.com>
```

Or use `git commit -s`.

## Ground rules

- **No telemetry, ever.** PRs adding network calls must pass the network
  audit review; the binary must stay fully functional offline.
- **Line counts are informational.** `cargo xtask lines` reports AST-based
  statistics without numeric caps. Review reuse, responsibilities, dependency
  direction and tests; do not move or duplicate code to satisfy a size metric.
- **`unsafe` is forbidden** workspace-wide, with a single crate-level
  exemption: `openpaw-sandbox`, whose OS-FFI backends (restricted tokens,
  job objects) are the only sanctioned `unsafe` (every block carries a
  SAFETY comment; see the deviation register D-9 in `docs/devlog.md`).
- **Porting hygiene.** Ported code must be free of upstream-internal
  references; `cargo xtask scrub --check` runs in CI.

## 项目开发与验证

先读 [AGENTS.md](AGENTS.md)、[产品需求](docs/requirements/产品需求说明.md)、
[软件设计](docs/design/软件设计说明.md) 和 [开发规范](docs/governance/开发与变更规范.md)。
GitHub是当前PR/Actions入口，沿用现有CI扩展，Linux为主开发、Windows原生支持；
不适配macOS，nightly与release同样只保留Linux/Windows。

每个新增功能、缺陷及文档/工程变更在docs/changes登记CHG，准确列文件、复用决策、
需求/设计/测试、文档影响及证据。完整命令见governance/commands.json；本地依赖准备后运行
`python governance/verify.py --suite all --output <仓库外新目录>`，报告保留真实退出码。
新增/修改公共Rust声明按中文注释规范；私有不变量扩展原inline测试，不为目录形式扩大API。

T0—T8：一Tx一主CHG、独立PR到merge/ai-ofice-react-overagent，上一阶段当前提交获人审并合入
才开始下一阶段。全部阶段约定范围验收后转普通CHG开发，取消Tx串行规则；正常PR、人审、DCO、
文档同步和完整测试继续有效。作者不能自批或自动合入，CI不能认证人工批准。
Office任何文件变更先提交Rx原因/方案/影响/范围/验证/回退，经用户明确批准后才可实施。

平台required checks与CODEOWNERS需真实审核账号和仓库设置，尚未配置不宣称已生效。

## Security

`SECURITY.md` and the governance/data-policy pages are planned with the
first public release batch (design plan §7.11/§7.12). Until they land,
report exploitable problems privately to the repository maintainers
(Gitee private message to the owner) — do not open public issues.
