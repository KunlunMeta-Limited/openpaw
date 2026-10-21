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
- **22 tools** — filesystem (6), exec (3), web (3), and meta (10, including
  session full-text search), each with
  contract + failure-path tests. `bash` commands pass through a
  tree-sitter AST safety chain (dangerous → blocked, uncertain → approval,
  fail-closed).

  | Group | Tools |
  |---|---|
  | Filesystem (6) | `read_file` `write_file` `edit_file` `grep` `glob` `notebook_edit` |
  | Exec (3) | `bash` `clipboard` `sleep` |
  | Web (3) | `web_fetch` `web_search` `web_extract` |
  | Meta (10) | `think` `todo_add` `todo_update` `todo_list` `clarify` `enter_plan_mode` `exit_plan_mode` `structured_output` `session_search` `read_attachment` |

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

### 图片与录音附件

CLI 与 Host 可以把附件交给所选模型理解，并接收文字回答。图片通过 OpenAI Chat
Completions 或 Anthropic Messages；原生音频通过支持音频输入的 Chat Completions 模型。
先声明你实际使用模型的输入能力；未声明的模型按纯文本处理，不会自动更换模型或转写录音。

```powershell
$env:OPENPAW_MODEL_CAPABILITIES = '{"openai":{"your-vision-model":{"input":["text","image"],"context_window":128000},"your-audio-model":{"input":["text","audio"]}}}'
openpaw chat --provider openai --model your-vision-model --attach "C:\images\screen.png"
```

输入文字会同时发送待发送附件；交互中可用 `/attach <path>` 添加、`/send` 单独发送、
`/clear-attachments` 清空。`--attach` 可重复，EOF 不发送待发送附件。音频用相同命令，
选择已声明 `audio` 的模型。`read_file` 支持工作区媒体，`read_attachment` 可按编号回看历史原件。

支持 PNG/JPEG/WebP/GIF（请求仅使用首帧）、WAV/MP3。单图 5 MiB、单音频 20 MiB，
每回合最多 4 个不同附件且原件总量 20 MiB；模型请求另有 32 MiB 上限。
原件内联保存在 JSONL；自动压缩仅改变模型上下文，完整记录保留原件与真实生成消息。
本地检查 MIME、容器签名及大小，不解码、压缩或转码；上游仍可能有额外尺寸或时长限制。

本轮通过本地 HTTP、CLI/Host 和历史契约测试；真实理解效果及第三方工具媒体适配尚待实测。
Host 请求与完整配置见 [配置说明](docs/b-line/configuration-and-connection.md#图片与原生音频配置chg-2026-0026)。

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
cargo run -p xtask -- lines        # per-crate line budgets
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

**Provider keys.** Environment variables only — `ANTHROPIC_API_KEY` /
`OPENAI_API_KEY`. Keys never touch disk and there is no config file to
leak them (PowerShell: `$env:ANTHROPIC_API_KEY = "sk-..."`;
bash: `export ANTHROPIC_API_KEY=sk-...`).

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

CI enforces ten gates, including: line-budget ceilings per crate asserted
via AST parsing (currently 29,262 total business lines of the 50,000
core-nine budget), a dependency whitelist, a zero-telemetry symbol scan,
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
- **Small core** — the workspace carries a hard line budget per crate
  (core nine total: 50,000 business lines), asserted in CI.

## License

Apache-2.0. Contributions are subject to the Developer Certificate of
Origin (see [CONTRIBUTING.md](CONTRIBUTING.md)).
