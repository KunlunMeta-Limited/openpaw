# Compatibility Commitments

Status: **1.0 freeze draft** (third item of the 1.0 freeze set, design plan
§7.8). This document is the public API/compat contract for consumers and
plugin authors. Before 1.0: experiment boldly. After 1.0: credibility beats
convenience.

## 1. SemVer

- All workspace crates currently version **0.1.0** (`workspace.package`).
  Everything may break until the 1.0 freeze; we tag breaking changes in
  changelogs as they happen, not retroactively.
- At 1.0, each crate version independently under SemVer. `openpaw-core`'s
  major version is the compatibility baseline for the whole ecosystem: if
  `openpaw-core` is 1.x, every official crate guarantees compatibility
  with that line, and cross-crate dependencies stay inside their declared
  version ranges (checked in CI).
- **1.0 frozen surface**:
  - every public item of `openpaw-core` tagged `Stability: stable` (the
    `xtask stability` gate enforces the tagging; untagged public items are
    a red build);
  - the knowledge file formats pinned by
    `docs/specs/knowledge-format-1.0.md` (additive-only evolution);
  - the session JSONL event schema and the message/content-block wire
    format (additive; unknown variants fall to forward-compat catch-alls).
- Items tagged `Stability: unstable` may change in any release until they
  are promoted to stable (RFC + changelog entry required for promotion).
- Deprecations (stable items scheduled for removal) follow the two-minor
  window in §3 — they never break a build inside the window.

## 2. MSRV

- **MSRV floor: Rust 1.88** (`workspace.package.rust-version`, inherited by
  every crate via `rust-version.workspace = true`).
- The toolchain channel is pinned to `stable` in `rust-toolchain.toml`
  (currently 1.98.0). Reproducible builds are anchored by `Cargo.lock` +
  `--locked` rather than a hard date-version pin.
- Policy: an MSRV bump is a **minor** release, never a patch; the floor
  only moves when a needed feature does not exist in the current floor,
  and the change is called out in the release changelog.
- **Enforcement**: the `msrv` CI job compiles the whole workspace
  (including test targets) on Rust 1.88.0 — a locked dependency or a
  workspace change that needs a newer rustc is a red build, keeping the
  declared floor honest.
- Change log for the floor itself:
  - **1.80 → 1.88 (pre-1.0, MSRV-floor decision)**: the locked dependency
    tree had come to carry edition2024 crates (clap 4.6, rand 0.10, rmcp
    1.8 — the MCP SDK publishes no edition2021 release line), which a 1.80
    cargo cannot even parse; an intermediate 1.85 floor then failed the
    next real check, because the rmcp macro stack hard-depends on darling
    0.23, which declares rustc 1.88. The floor settled at 1.88 — also
    rmcp's own declared floor. No released version ever shipped against
    an older floor.

## 3. Breaking-change process (stable items)

A breaking change to any `Stability: stable` item after 1.0 requires, in
order:

1. **RFC** — a public RFC describing the change, the motivation, and the
   migration path; merged only after the review window. No RFC, no break.
2. **Changelog** — the deprecation and the migration path are documented in
   the affected crate's changelog at the release that introduces the
   deprecation, with `[deprecated]` marking.
3. **Two-minor transition window** — the old form keeps working (with a
   deprecation warning where the language allows one) for at least two
   minor releases; removal may land at the earliest in the next major
   release (or the minor after the window when the item is 0.x).

The same RFC discipline covers the knowledge format: non-additive format
changes are breaking changes (see `docs/specs/knowledge-format-1.0.md`,
§6).

## 4. Platform commitments

- **Windows first.** The primary development and smoke-test platform is
  Windows: rustls-based HTTP keeps builds self-contained, the local sandbox
  implements Windows Restricted Token isolation, and path/platform edge
  cases are tested on Windows first.
- **CI matrix.** Every push/PR runs `cargo clippy --workspace` on
  ubuntu + windows and `cargo test --workspace` on ubuntu + macos +
  windows (`.github/workflows/ci.yml`). A platform regression in the
  matrix is a release blocker.
- Platform-specific behavior is quarantined behind explicit modules
  (e.g. `openpaw-sandbox/src/platform/`); unconditional platform-specific
  code paths are not accepted.

## 5. Quality gates (the enforcement layer)

The commitments above are enforced by CI, not by trust. Gate inventory
(dev plan §12, gate table):

| Gate | What it asserts |
|---|---|
| Q1 `xtask lines` | per-crate business-line budgets (AST-counted). |
| Q2 `cargo-deny` | dependency whitelist / banned dependencies. |
| Q3 `xtask telemetry` | zero telemetry: dependency names + source symbols. |
| Q4 `xtask smoke-offline` | offline smoke: hello → doctor → evolve end-to-end. |
| Q5 reflect assertions | reflect artifacts always carry `unverified: true` (100% ratio over persisted stores). |
| Q6 `xtask golden` | golden bidirectional read/write for frontmatter + session JSONL + D8 schema samples. |
| Q7 `xtask snapshot --check` | public trait signature snapshot of `openpaw-core`; drift is a red build. |
| Q8 build/scale budgets | cold build < 5 min, onboarding success rate, ≤ 50k core lines. |
| Q9 human-alignment assertions | `requires_human_alignment` defaults to `true` and overrides must preserve it. |
| Q10 `xtask scrub --check` | porting hygiene: banned identifiers / task-id patterns / internal doc names, zero hits. |

Plus the 1.0-freeze additions wired alongside them:

- **`xtask stability`** — every public item of `openpaw-core` carries a
  `Stability: stable` / `Stability: unstable` doc tag (this document, §1).
- **`xtask telemetry` / `xtask smoke-offline`** — maintained as the Q3/Q4
  enforcement commands; they run in the same CI quality-gates job.

A release is only cut when the full gate set is green.

## 6. Known limitations at 1.0

Documented decisions from the plan's §15.4 decision points — each is a
deliberate freeze-time call, not an oversight:

- **Session COT fields are read-compatible, not rewrite-preserving.**
  Reading upstream sessions that contain `thought_snapshots` /
  `thought_triggers` is safe: serde ignores unknown fields and unknown
  events fall to `SessionEvent::Unknown`. Rewriting such a session
  through OpenPaw drops those two fields (`injection_records` is
  preserved — it was backfilled into `Session` in 1.0). Backfilling the
  remaining fields is deferred until a real provider with thinking
  streams lands; with offline providers (`mock`/`echo`) there is no
  write-side producer.
- **The `embed` feature of `openpaw-memory` is a name-only placeholder.**
  The feature flag exists (default off, per the porting plan §3.7
  "feature-ize embed") but 1.0 ships no embedding implementation —
  relevance is lexical (TF-IDF + CJK bigram). The name is kept so the
  future `fastembed` integration lands as an additive, opt-in feature
  instead of a new flag.
- **The sidecar `ask` tool is offline-only.** It serves the `mock` and
  `echo` providers; no network LLM calls and no session persistence.
  Real providers plug into the same seam after 1.0.
- **The MCP HTTP client ships without TLS — `https://` MCP servers are
  rejected at config validation.** Plain `http://` endpoints (loopback
  and LAN) work; stdio servers are unaffected. This keeps the
  aws-lc-sys C/cmake build chain out of the dependency tree (the rmcp
  HTTP stack declares reqwest without default features). Re-enabling
  TLS later is a pure additive change: add rmcp's `"reqwest"` feature
  and relax the config check.
