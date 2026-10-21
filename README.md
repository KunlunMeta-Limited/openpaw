# OpenPaw

An open, evolvable personal AI agent runtime in Rust.

OpenPaw is a lean, open-source agent platform: a single binary that talks to
LLM providers, runs tools in a guarded sandbox, remembers what matters in
plain markdown files, and improves itself through a transparent reflection
loop — with zero telemetry, by design.

## Status

All milestones M0–M6 are complete and frozen for 1.0 readiness. The one
deferred M2 item — the MCP client host in `openpaw-mcp` — has since
landed (config, stdio/HTTP transports, circuit breaker; sessions 38–40
in [docs/devlog.md](docs/devlog.md)), with CLI wiring to follow. The
MSRV floor is now Rust 1.88
([docs/compatibility.md](docs/compatibility.md) §2). See the devlog for
the full development log, the per-milestone exit reports
(`docs/m*-exit-report.md`), and compatibility.md for the stability
contract.

## Quick start

```sh
cargo install --path crates/openpaw-cli --locked
openpaw hello
```

`openpaw hello` starts an echo-model conversation with no API key required —
your first response lands within seconds, fully offline. New to OpenPaw?
Follow the [beginner walkthrough](#first-session-a-beginners-walkthrough)
below, then browse the [tutorials](#tutorials--cli-reference).

## Feature highlights

- **Agent engine** — the agent loop with streaming, retries, compaction,
  and approval branches, ported from a production codebase and verified by
  golden-session round-trips.
- **21 tools** — filesystem (6), exec (3), web (3), and meta (9, including
  session full-text search), each with
  contract + failure-path tests. `bash` commands pass through a
  tree-sitter AST safety chain (dangerous → blocked, uncertain → approval,
  fail-closed).

  | Group | Tools |
  |---|---|
  | Filesystem (6) | `read_file` `write_file` `edit_file` `grep` `glob` `notebook_edit` |
  | Exec (3) | `bash` `clipboard` `sleep` |
  | Web (3) | `web_fetch` `web_search` `web_extract` |
  | Meta (9) | `think` `todo_add` `todo_update` `todo_list` `clarify` `enter_plan_mode` `exit_plan_mode` `structured_output` `session_search` |

- **OS-level sandbox** — Windows restricted tokens + job objects, Linux
  bubblewrap, macOS Seatbelt; unsupported platforms fail closed. Exposed
  to the tools through the `Environment` seam.
- **Resilient LLM layer** — Anthropic + OpenAI over a unified SSE stack;
  error classification drives exponential-backoff retries (±25% jitter,
  `Retry-After` honored), context-overflow → model fallback, CJK-aware
  context compaction, and a token budget that nudges long tasks instead of
  dying at 90% usage.
- **Plain-file memory & skills** — user/project dual-scope markdown stores
  with YAML frontmatter, five-state skill lifecycle, TF-IDF + CJK-bigram
  relevance scoring, SM-2 spaced review, and a `manage` tool as the single
  write entry.
- **Self-evolution loop** — `reflect-lite` produces unverified knowledge
  artifacts that stay flagged until human review (`evolve status` shows
  the ratios); a goal ledger tracks multi-session work.
- **24-hook plugin system** — static trust-level security scanning, three
  discovery sources (`bundled` / user / opt-in project), before/after
  hooks on the tool path.
- **MCP sidecar** — `openpaw --sidecar` exposes five tools to MCP clients
  (Claude Code / Cursor / OpenWork / Trae) over stdio or loopback HTTP. See
  [docs/tutorials/sidecar-clients.md](docs/tutorials/sidecar-clients.md).

## Build from source

Prerequisites: **Rust 1.88+** (the MSRV floor; the toolchain channel is
pinned to `stable` via `rust-toolchain.toml`) and Git. Windows is the
first-class platform; Linux and macOS are exercised in CI.

```sh
git clone <this repository>
cd openpaw
cargo build --release -p openpaw-cli
# binary lands at target/release/openpaw (openpaw.exe on Windows)
```

Feature flags of the CLI crate:

| Feature | Default | Effect |
|---|---|---|
| *(none)* | on | all 12 subcommands; no MCP server |
| `sidecar` | off | adds the `--sidecar` MCP server (pulls in `rmcp`) |

```sh
cargo build -p openpaw-cli --features sidecar   # with the MCP sidecar
```

Verify your build the way CI does:

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
cargo run -p xtask -- lines        # informational AST line statistics
cargo run -p xtask -- golden       # knowledge-format round-trips
```

Platform notes: the sandbox backends need Bubblewrap on Linux
(`bwrap`); macOS uses the built-in `sandbox-exec`; Windows uses native
restricted tokens. A missing backend fails closed — it never degrades to
unsandboxed execution. The `bash` tool runs commands through `sh -c` on
Unix and `cmd /C` on Windows; the AST safety chain gates both.

## Install & deployment

**Install from source** (signed binary releases are CI-wired but the first
publish cut is pending — see below):

```sh
cargo install --path crates/openpaw-cli --locked                  # core CLI
cargo install --path crates/openpaw-cli --features sidecar --locked  # + MCP sidecar
```

**Releases.** Pushing a `v*` tag triggers `.github/workflows/release.yml`:
reproducible builds (`--locked`, `SOURCE_DATE_EPOCH`, LTO) for
windows-x86_64 / linux-x86_64 / macos-aarch64, SHA-256 manifests, Sigstore
keyless signing, and GitHub Release attachment. The crates.io publish path
is dry-run-validated in dependency order; the real publish awaits the
registry token.

**Data layout.** Everything lives under `OPENPAW_HOME` (default
`~/.openpaw`; create the skeleton with `openpaw init`):

```
~/.openpaw/
├── sessions/   per-session JSONL transcripts (the source of truth)
├── memories/   user-scope memories (markdown + YAML frontmatter)
├── skills/     user-scope skills (<name>/SKILL.md)
├── goals/      goal profiles + append-only JSONL ledgers
├── plugins/    plugin discovery root
└── index.db    SQLite FTS5 search index (best-effort, rebuildable)
```

Relocate the whole deployment by setting `OPENPAW_HOME`; project-scope
knowledge goes to `<project>/.openpaw/` instead.

**模型配置 / Model configuration.** OpenPaw现在原生读取用户数据目录下的`.env`：
默认Linux为`~/.openpaw/.env`，Windows为`$HOME\.openpaw\.env`；设置了`OPENPAW_HOME`则使用该目录。
只读加载，不向上搜索项目目录，不执行shell，也不把文件密钥加入工具进程环境。

首次从仓库复制模板（已有文件请直接编辑，不覆盖）：

```bash
mkdir -p ~/.openpaw
cp -n .env.example ~/.openpaw/.env
chmod 600 ~/.openpaw/.env
```

Windows PowerShell（无需WSL）：

```powershell
$openpawConfigDir = if ($env:OPENPAW_HOME) { $env:OPENPAW_HOME } else { Join-Path $HOME '.openpaw' }
New-Item -ItemType Directory -Force $openpawConfigDir | Out-Null
$openpawEnvPath = Join-Path $openpawConfigDir '.env'
if (-not (Test-Path $openpawEnvPath)) { Copy-Item .env.example $openpawEnvPath }
notepad $openpawEnvPath
```

将模板中的DeepSeek密钥占位值替换为自己的密钥：

```dotenv
OPENPAW_PROVIDER=openai
OPENPAW_MODEL=deepseek-chat
OPENPAW_BASE_URL=https://api.deepseek.com/v1
OPENAI_API_KEY=your_deepseek_api_key
```

此处`openai`表示OpenAI兼容协议，实际服务为DeepSeek。随后执行：

```bash
openpaw config show          # 只显示有效模型、来源和密钥有无，不发模型请求
openpaw chat                 # 不再每次输入模型、端点和密钥
openpaw chat --env-file /absolute/path/model.env
```

优先级为：明确CLI参数或Host TOML模型字段 → 系统环境变量 → `.env` → 原默认值。
未配置时chat仍为离线mock，Host仍为echo；模型为空、文件格式错误或显式文件不存在会失败，不静默降级。
`--env-file`是全局选项，也可用于`config show`和`engine-host`；`chat --home`仅改变会话存储位置。
默认文件不存在时保持旧用法。配置只在启动时读取，修改后重启。
`.env`支持UTF-8（含BOM）、CRLF、注释、引号及dotenvy变量引用；重复字段拒绝。
使用`OPENPAW_HOME`时Linux复制目标也应改成该目录。系统环境中已有同名变量会覆盖文件，可用`config show`核对来源。

Host继续用原TOML指定数据目录、授权工作区和工具：

```bash
openpaw engine-host --config /absolute/path/host.toml
```

希望Host沿用`.env`时，省略TOML的`provider`、`model`、`base_url`和`api_key_env`；
明确写了`provider = "echo"`就继续离线运行。自定义`api_key_env`仍可从系统环境或选定.env读取。
原`HostConfig::load`和`Host::start`嵌入式Rust入口保持原行为；新加载仅在CLI启动或显式调用新回退接口时启用。
**配置模型并不代表Office聊天已接通，Office适配仍按T4计划单独验收。**

原环境变量用法继续支持：`OPENAI_API_KEY` / `ANTHROPIC_API_KEY`配合`chat --provider ... --model ... --base-url ...`。
密钥不进Git、浏览器、日志或错误原始行；`.env.example`只含占位值。

**Run as an MCP sidecar.** There is no daemon: the editor/agent client
launches `openpaw --sidecar --sidecar-transport stdio` itself and owns the
process. The alternative `http` transport binds loopback-only with a
dynamic port printed to stdout. Client-by-client wiring:
[docs/tutorials/sidecar-clients.md](docs/tutorials/sidecar-clients.md).

**Operating posture.** `hello`/`mock`/`echo` and every non-web tool work
with networking unavailable; there is no telemetry of any kind (the
`doctor` command prints a static network-audit report as evidence).
Uninstall = remove the binary plus (optionally) your `OPENPAW_HOME`.

## First session: a beginner's walkthrough

About ten minutes, end to end. PowerShell and bash both work; commands are
identical unless noted.

1. **Create the home skeleton**

   ```sh
   openpaw init
   ```

2. **Check your environment** — home writability, provider keys present or
   absent (both fine offline), index.db opens, network-audit report:

   ```sh
   openpaw doctor
   ```

3. **First conversation, fully offline** — an echo model, no key needed.
   Type `exit` (or `quit`) to leave:

   ```sh
   openpaw hello
   ```

4. **Chat with the tool-enabled agent loop** — `mock` replies are
   deterministic counters, so nothing leaves your machine:

   ```sh
   openpaw chat --provider mock
   ```

5. **Switch to a real model** — set a key, then pick the provider:

   ```sh
   $env:ANTHROPIC_API_KEY = "sk-ant-..."        # PowerShell
   export ANTHROPIC_API_KEY="sk-ant-..."        # bash
   openpaw chat --provider anthropic            # or: --provider openai
   ```

   Ask it to do something real: *"read Cargo.toml and summarize the
   workspace members"* — tool calls print as `· tool — preview` lines, and
   the final reply lands when the turn completes. The session is saved to
   `<home>/sessions/<id>.jsonl` and indexed into `index.db`.

6. **Find your sessions again**

   ```sh
   openpaw session list
   openpaw session search "workspace members"
   ```

7. **Memory that persists** — ask the model in `chat` to save a memory
   ("remember that I prefer lowercase commit messages"); it writes plain
   markdown under `<home>/memories`. Inspect it from the CLI:

   ```sh
   openpaw memory list
   openpaw memory search "commit messages"
   openpaw memory show <id>
   ```

8. **Watch the evolution loop** — reflection candidates land flagged
   `unverified` until you review them:

   ```sh
   openpaw evolve status   # unverified ratios + goal overview
   openpaw evolve reset --yes   # clear flags after review (files kept)
   ```

9. **Track a longer effort**

   ```sh
   openpaw goal new "migrate the blog to the new theme" --project
   openpaw goal status
   ```

10. **Plug it into your editor** — register the sidecar with Claude Code,
    Cursor, OpenWork, or Trae:
    [docs/tutorials/sidecar-clients.md](docs/tutorials/sidecar-clients.md).

Tips: `chat` reads one message per line from stdin, so piping a script
works too (`Get-Content prompt.txt | openpaw chat --provider echo`) —
non-interactive output drops the `Paw:` prefix. `--home` / `--workspace`
flags relocate storage and tool scope per command; `OPENPAW_HOME` sets it
globally.

## Tutorials & CLI reference

| Guide | Content |
|---|---|
| [docs/tutorials/sidecar-clients.md](docs/tutorials/sidecar-clients.md) | Wire the five-tool MCP sidecar into Claude Code / Cursor / OpenWork / Trae (stdio + HTTP, troubleshooting, security posture) |
| [docs/engine-host/master.md](docs/engine-host/master.md) | OpenWork and AI Office wiring (not built yet) |
| [docs/specs/knowledge-format-1.0.md](docs/specs/knowledge-format-1.0.md) | The frozen on-disk formats: memory/skill frontmatter, `unverified` flag, D8 reflection schema |
| [docs/compatibility.md](docs/compatibility.md) | SemVer policy, MSRV floor, platform commitments, quality-gate inventory |
| [docs/devlog.md](docs/devlog.md) | The full development log: decisions, deviations, per-session evidence |
| [docs/boundary-decisions/ADR-001-engine-host-archived.md](docs/boundary-decisions/ADR-001-engine-host-archived.md) | Why engine-host is out of the 1.0 crate list |

The 12 subcommands (plus the informational `sidecar` and the global
`--sidecar` server flag):

| Command | Purpose |
|---|---|
| `hello` | Zero-config offline echo conversation (smoke entry) |
| `chat` | Full agent loop: tools + JSONL session + FTS5 index (`--provider mock\|echo\|anthropic\|openai`) |
| `session list\|show\|search` | List, inspect, and full-text-search past sessions |
| `memory list\|search\|show\|delete` | Browse and search user/project memories |
| `evolve status\|reset` | Unverified-knowledge ratios; clear flags after human review |
| `doctor` | Environment checks + static network-audit report |
| `goal new\|status\|resume` | Create goal profiles; progress digest; single-step resume decision |
| `config show\|doctor` | Effective home + provider-key presence (values always masked) |
| `model list\|default` | Static provider/model table (`default` is a placeholder for now) |
| `tool list` | Print the 21 registered tool names |
| `completions <shell>` | Placeholder until the clap_complete wiring lands |
| `init` | Create the home skeleton; safe to re-run |
| `sidecar status\|stop` | Informational only — the sidecar runs in-process via `--sidecar` |

## Quality gates

CI retains AST-based line statistics for review, without numeric size caps.
The other gates include a dependency whitelist, a zero-telemetry symbol scan,
an offline smoke test, golden-file round-trips, trait-snapshot checks, a
cold-build time budget, human-alignment invariants, and a
secrets/internal-terms scrub.

## Design principles

- **Zero telemetry** — there is no `[telemetry]` section in the config
  because there is no telemetry. Not "off by default": absent.
- **Plain files are the database** — memories, skills, and goals are
  markdown files with YAML frontmatter you can read, edit, and sync.
- **Human alignment is non-negotiable** — every knowledge-writing policy
  must keep `requires_human_alignment() == true`; CI enforces it.
- **Reuse the core** — keep protocol adapters in the Host and reuse existing
  engine, tools and storage. Review responsibilities and dependencies rather
  than file or crate line counts.

## License

Apache-2.0. Contributions are subject to the Developer Certificate of
Origin (see [CONTRIBUTING.md](CONTRIBUTING.md)).
