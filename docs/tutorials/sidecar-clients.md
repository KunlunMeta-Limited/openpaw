# Wiring OpenPaw's sidecar into MCP clients

This tutorial connects MCP clients — **Claude Code**, **Cursor**,
**OpenWork**, and **Trae** — to the OpenPaw sidecar server
(`openpaw --sidecar`), which exposes five tools over the Model Context
Protocol.

> **Status note.** The server side is complete and covered by unit + e2e
> tests (9 sidecar unit tests, 3 JSON-RPC e2e tests); both transports were
> additionally exercised end-to-end by scripted JSON-RPC clients — stdio
> (all five tools, plus a concurrent-dispatch regression) and HTTP
> (initialize / POST-only semantics incl. the spec-allowed `405` on GET).
> A real-client wiring session (2026-08-31) probed Claude Code / Cursor /
> OpenWork on the target machine (none installed, npm absent) and
> completed the wiring against **Trae** instead: the stdio protocol
> surface passed a full five-tool probe (initialize, tools/list,
> memory_set → memory_get → memory_search → evolve_status → ask, disk
> round-trip), which also exposed and fixed a concurrency defect —
> concurrent `tools/call` frames could reorder a `memory_get` ahead of an
> in-flight `memory_set`; the server now serializes tool calls. The
> remaining Trae steps are user-side GUI actions (create `.trae/mcp.json`,
> flip the project-level MCP switch) — see §6. Where a step depends on
> the third-party client's UI, verify against that client's current
> documentation.

## 1. Prerequisites

### 1.1 Install the CLI with the sidecar feature

The sidecar is **not** in the default feature set (it pulls in `rmcp`):

```sh
cargo install --path crates/openpaw-cli --features sidecar --locked
openpaw --help   # `--sidecar` and `--sidecar-transport` must appear
```

### 1.2 Understand the two transports

| Transport | Flag | Wire behavior |
|---|---|---|
| `http` (default) | `--sidecar-transport http` | Binds `127.0.0.1:0` (loopback, dynamic port), prints `sidecar http listening on http://127.0.0.1:<port>/mcp` to stdout. `POST /mcp` with a JSON-RPC body returns a JSON response; notifications get `202`; `GET`/`DELETE` get `405`. |
| `stdio` | `--sidecar-transport stdio` | Newline-delimited JSON-RPC on stdin/stdout. The transport owns stdout; startup chatter goes to stderr. |

Two facts shape every wiring below:

1. **No daemon.** The sidecar runs in-process with `--sidecar`. There is
   nothing to `status` or `stop` — the client that launches the process
   owns it (red line; `openpaw sidecar status` only prints a note).
2. **Dynamic port.** The HTTP shell never pins a port. Clients that
   require a fixed URL must either parse the stdout line at launch or use
   the stdio transport instead.

### 1.3 Where your data lives

The sidecar uses the same home root as the CLI: `OPENPAW_HOME` or
`~/.openpaw`. Memory tools read/write the **user scope** only —
`<home>/memories`, identical to `openpaw memory` without `--project`.
Run `openpaw init` once to create the skeleton.

## 2. The five tools

Schemas are declared as static JSON in the handler
(`crates/openpaw-cli/src/sidecar.rs`).

| Tool | Arguments | Behavior |
|---|---|---|
| `ask` | `input` (string, required); `provider` (`mock` \| `echo`, optional, default `mock`) | One agent-loop turn on an offline provider. Fresh session per call; no tool host, no session persistence. |
| `memory_set` | `id` (required), `content` (required), `tags` (string[], optional) | Upsert one user-scope memory by id (markdown body, tags in frontmatter). |
| `memory_get` | `id` (required) | Fetch one memory by id. |
| `memory_search` | `query` (required), `limit` (number, optional, default 20) | Scored search over user memories; empty query matches everything. |
| `evolve_status` | — | Unverified-artifact ratio (reflect-lite `scan_unverified`) plus a goal-ledger overview digest. |

`ask` is intentionally offline-only in this phase (`mock`/`echo`
providers). Real providers plug into the same seam later; do not expect
live LLM answers through the sidecar yet.

## 3. Claude Code

Claude Code launches stdio MCP servers itself, which sidesteps the
dynamic-port issue entirely — this is the recommended wiring.

### 3.1 Register the server

```sh
claude mcp add openpaw -- openpaw --sidecar --sidecar-transport stdio
```

Scope notes: add `--scope user` to make it available in every project
(default is local to the current project); add `-e OPENPAW_HOME=/path` to
point at a non-default home.

### 3.2 Verify

1. Start Claude Code and run `/mcp` — `openpaw` should list five tools.
2. Ask: *"Use the openpaw memory_set tool to save a memory with id
   `tutorial-smoke` and content `hello from claude code`."*
3. Confirm on disk:

```sh
openpaw memory list | grep tutorial-smoke
```

### 3.3 Windows note

Claude Code spawns the command directly, so `openpaw` must be on `PATH`.
If it is not, give `claude mcp add` the absolute path to the installed
binary, or use `cmd /c openpaw ...` as the command.

## 4. Cursor

Cursor's `mcp.json` supports both stdio command servers and HTTP URLs.

### 4.1 Recommended: stdio (auto-launch)

`~/.cursor/mcp.json` (Windows: `%USERPROFILE%\.cursor\mcp.json`):

```json
{
  "mcpServers": {
    "openpaw": {
      "command": "openpaw",
      "args": ["--sidecar", "--sidecar-transport", "stdio"]
    }
  }
}
```

Reload Cursor; the MCP panel should show `openpaw` with five tools.

### 4.2 Alternative: HTTP (manual launch)

Because the HTTP shell binds a dynamic port, manual launch means reading
the port from stdout each time:

```sh
openpaw --sidecar --sidecar-transport http
# stdout: sidecar http listening on http://127.0.0.1:54321/mcp
```

Then in `mcp.json`:

```json
{
  "mcpServers": {
    "openpaw": {
      "url": "http://127.0.0.1:54321/mcp"
    }
  }
}
```

> **Interoperability caveat (D-12 / M5b).** The HTTP shell implements
> Streamable-HTTP POST semantics only. Clients that insist on opening a
> GET/SSE stream first will see `405` — the MCP spec allows a server to
> reject GET, but some clients treat that as fatal. If Cursor fails to
> list tools over HTTP, fall back to the stdio wiring in 4.1. Validating
> this interaction against real clients is exactly the Phase 2 item.

## 5. OpenWork

OpenWork accepts a generic MCP server config (command + args, or URL),
so both transports work:

- **stdio config** — command `openpaw`, args `--sidecar
  --sidecar-transport stdio` (same shape as Cursor 4.1).
- **HTTP config** — launch the sidecar first (4.2), paste the printed
  `http://127.0.0.1:<port>/mcp` URL.

OpenWork is the plan's reference client for the sidecar; after wiring,
drive the full loop as a smoke test:

1. `memory_set` — save `id: openwork-smoke`, some content, tag `tutorial`.
2. `memory_search` — query `openwork`; expect the new memory with a
   nonzero score.
3. `evolve_status` — expect a JSON digest with `unverified` ratios
   (initially 0) and a goal-ledger overview.
4. `ask` with `provider: echo` — expect your input echoed back.
5. Confirm the round-trip landed as plain markdown:

```sh
openpaw memory show openwork-smoke
```

## 6. Trae (TraeCode)

Trae supports project-level MCP servers declared in
`<project-root>/.trae/mcp.json`, plus a manual GUI entry point
(Settings → MCP → Add → Manual). The project-level file is the
recommended wiring and is what the real-client session exercised.

### 6.1 Project-level config

Create `.trae/mcp.json` in the project root (the file is **restricted to
user edits** — Trae refuses agent-written changes to it, which is a
safety property: a model must not extend its own tool surface. Create it
by hand):

```json
{
  "mcpServers": {
    "openpaw": {
      "command": "${workspaceFolder}\\target\\debug\\openpaw.exe",
      "args": ["--sidecar", "--sidecar-transport", "stdio"],
      "env": {
        "START_MCP_TIMEOUT_MS": "60000",
        "RUN_MCP_TIMEOUT_MS": "60000"
      }
    }
  }
}
```

Notes:

- `${workspaceFolder}` is the only variable Trae expands today; whether
  it resolves inside `command` depends on the Trae build — if the server
  fails to start, replace it with the absolute path (no spaces allowed
  in `command`) or install the CLI (`cargo install --path
  crates/openpaw-cli --features sidecar --locked`) and use plain
  `openpaw`, which must then be on `PATH`.
- The command form above points at a workspace build; for a released
  install prefer the `openpaw` form.

### 6.2 Enable and verify

1. Settings → MCP → enable **project-level MCP** (Trae asks for
   confirmation).
2. The MCP panel should list `openpaw` with five tools.
3. Smoke-test in a chat that has MCP tools enabled: save a memory with
   `memory_set` (`id: trae-smoke`), then confirm on disk:

```sh
openpaw memory list | findstr trae-smoke
```

Memory files are stored as `<created-date>-<id>.md` under
`<home>/memories`; lookups match by frontmatter `id`, not by filename.

## 7. Troubleshooting

| Symptom | Cause | Fix |
|---|---|---|
| `--sidecar` flag not recognized | Installed without the feature | Reinstall with `--features sidecar` (§1.1) |
| Client lists no tools over HTTP | Client opened GET/SSE first, shell answered `405` | Use stdio transport (3.1 / 4.1), or confirm client tolerates POST-only servers |
| `405` on `GET /mcp` in a browser | Expected — the shell has no SSE stream | Not an error; use POST for requests |
| Port changed after restart | Dynamic port by design (`127.0.0.1:0`) | Parse the stdout line, or switch to stdio |
| Tools error with "cannot find home" | Home skeleton missing | `openpaw init`, or set `OPENPAW_HOME` for the client-launched process |
| Memory writes vanish or `memory_get` misses a just-saved id | Another `OPENPAW_HOME` in the environment (a stale value from earlier test sessions wins over `~/.openpaw`) | Check `echo $env:OPENPAW_HOME` (PowerShell) before diagnosing; clear it for the client-launched process |
| Trae agent cannot create `.trae/mcp.json` / the editor rejects writes | The file is user-edit-only by Trae's safety design | Create it manually, then reload; agents must not extend their own tool surface |
| Startup text corrupts stdio session | Anything else writing to stdout | stdio transport owns stdout; all chatter goes to stderr by design — do not wrap the command in scripts that echo to stdout |

## 8. Security posture

- The HTTP shell binds **loopback only** (`127.0.0.1:0`); it is never
  exposed to the network.
- Every tool derives its store from the home root — the sidecar can
  read/write only `<home>/memories` (user scope) and the goal ledger.
- `ask` runs offline providers only; the sidecar makes no network calls.
  `openpaw doctor` prints the network-audit report if you want the
  evidence yourself.
