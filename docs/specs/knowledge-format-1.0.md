# Knowledge Format 1.0 — Frontmatter & D8 Reflection Schema

Status: **1.0 freeze draft** (second item of the 1.0 freeze set, design plan
§7.8). This document pins the on-disk knowledge file formats: memory and
skill frontmatter, the `unverified` provenance flag, and the D8
`ReflectionOutput` wire schema. Enforcement lives in the `xtask golden`
gate (Q6) plus the `unverified` assertions of the reflect pipeline (Q5).

Authoritative serde definitions:

- Memory frontmatter: `crates/openpaw-memory/src/memory.rs` (`MemoryFrontmatter`)
- Skill frontmatter: `crates/openpaw-memory/src/skills/skill.rs` (`SkillFrontmatter`)
- Reflection output: `crates/openpaw-reflect-lite/src/output.rs` (`ReflectionOutput`)

---

## 1. File layout

A knowledge artifact is a UTF-8 text file: an optional-BOM YAML frontmatter
block delimited by `---` lines, followed by a Markdown body:

```
---
<yaml frontmatter>
---
<markdown body>
```

Layout rules (implemented in `openpaw-store::frontmatter`):

- The file MUST start with `---` for frontmatter-bearing artifacts; a
  missing block is a parse error, not a silent empty-doc.
- CRLF is normalized to LF on read; reads and writes are bidirectionally
  100% compatible (golden gate, Q6).
- Writers are atomic (temp file + rename) and byte-stable across
  read→write→write cycles.

## 2. Memory frontmatter (`MemoryFrontmatter`)

### 2.1 Required fields

| Field | Type | Notes |
|---|---|---|
| `id` | string | Stable identifier, e.g. `mem_<hex>`. Allocated by `MemoryFrontmatter::new`. |
| `created` | RFC3339 timestamp | Creation time (UTC). |
| `source` | `reflection` \| `user` \| `imported` \| `agent` | Provenance. Agent-written memories must never masquerade as `user`. |
| `confidence` | `low` \| `medium` \| `high` | Writer's confidence in the fact. |

### 2.2 Optional fields (serde defaults; absent = default)

| Field | Type | Default | Notes |
|---|---|---|---|
| `pinned` | bool | `false` | `true` ⇒ loaded into every session context. |
| `tags` | string[] | `[]` | Free-form labels. |
| `supersedes` | string[] | `[]` | IDs of older memories replaced by this one; listed by `list_active`. |
| `zone` | string | `"general"` | Coarse bucket (e.g. `goals`, `thinking-paths`). |
| `description` | string | `""` | One-line summary; derived from the body when absent. |
| `memory_type` | `fact` \| `procedure` \| `preference` \| `episode` \| `context` \| `insight` \| `reasoning` \| `goal` \| `intention` | `fact` | Classification; drives the decay half-life. |
| `sm2_repetition` | u32 | `0` | SM-2 successful repetitions. |
| `sm2_ease_factor` | f64 | `2.5` | SM-2 ease factor (min 1.3). |
| `sm2_interval_days` | u32 | `0` | SM-2 current interval in days. |
| `sm2_next_review` | string \| null | `null` | Next review date (RFC3339 / `YYYY-MM-DD`). |
| `task_id` | string \| null | absent | Linked board task (task-scoped recall). |
| `goal_id` | string \| null | absent | Linked long-horizon goal. |

### 2.3 The `extra` flatten channel

`MemoryFrontmatter` ends with `#[serde(flatten)] extra: serde_yml::Mapping`.
Every YAML key that is not a known field lands here verbatim and survives a
store round-trip. The channel is load-bearing:

- **`unverified`** (bool) — the provenance flag for reflect-generated
  artifacts. The reflect-lite persist path (`force_unverified`) **forces
  `unverified: true` on every artifact it writes**; there is no code path
  that writes a reflect artifact with `unverified: false`. Consumers
  (curator, curation UI) use the flag to route artifacts through human
  approval before they count as verified knowledge. Gate Q5 asserts the
  ratio over persisted stores.
- **COT fields** — reasoning-path provenance that has no dedicated schema
  field: `thinking_path`, `concrete_instance`, `problem_type` (written by
  the persist path for `memory_type: reasoning` artifacts).
- **Unknown keys** — any other key is preserved untouched. Downstream
  versions may read keys this version does not know; nothing is dropped.

## 3. Skill frontmatter (`SkillFrontmatter`)

### 3.1 Required fields

| Field | Type | Notes |
|---|---|---|
| `name` | string | kebab-case identifier; also the skill directory name. |
| `description` | string | What the skill does. |

### 3.2 Optional fields (serde defaults; absent = default)

| Field | Type | Default | Notes |
|---|---|---|---|
| `triggers` | string[] | `[]` | Activation phrases. |
| `version` | string \| null | absent | Semver of the skill. |
| `license` | string \| null | absent | SPDX identifier. |
| `status` | `active` \| `stale` \| `archived` \| `merged` \| `reactivated` | `active` | Lifecycle state. |
| `superseded_by` | string \| null | absent | Replacing skill name. |
| `priority` | u32 \| null | absent | Ranking hint. |
| `tags` | string[] | `[]` | Free-form labels. |
| `author` | string \| null | absent | Attribution. |
| `category` | string \| null | absent | Grouping hint. |
| `pinned` | bool | `false` | Always-on skill. |
| `source` | string \| null | absent | Where it came from (e.g. `reflect-lite`, `hub`). |
| `prerequisites` | string[] | `[]` | Skill names that must load first. |
| `parent` | string \| null | absent | Parent skill in a hierarchy. |
| `difficulty` | f64 \| null | absent | 0.0–1.0 estimate. |
| `concepts` | string[] | `[]` | Concepts taught/used. |
| `when_to_use` | string \| null | absent | AI matching hint (alias `when-to-use`). |
| `argument_hint` | string \| null | absent | Prompt hint (alias `argument-hint`). |
| `arguments` | string[] | `[]` | Declarative `$ARGUMENTS` names (alias `arguments`). |
| `allowed_tools` | string[] | `[]` | Tool whitelist injected at run time (alias `allowed-tools`). |
| `context` | `inline` \| `fork` | inline | Execution mode. |
| `agent` | string \| null | absent | Fork-mode agent type. |
| `disable_model_invocation` | bool | `false` | User-triggered only (alias `disable-model-invocation`). |
| `user_invocable` | bool | `true` | Callable by the user (alias `user-invocable`). |
| `paths` | string[] | `[]` | gitignore-style conditional activation patterns. |

The Anthropic `SKILL.md` kebab-case aliases are accepted on read and
normalized to the snake_case canonical names on write, so skills are
exchangeable with Claude Code conventions.

### 3.3 The `extra` flatten channel

Same contract as memory: `#[serde(flatten)] extra: serde_yml::Mapping`
preserves unknown keys, and `unverified: true` is force-written by the
reflect-lite persist path for every skill it generates.

## 4. D8 schema — `ReflectionOutput`

The reflection LLM's structured output. Reflect-Lite products and the
curator input share this schema (same-source registry item S2; JSON on the
wire, parsed with serde defaults everywhere).

| Key | Type | Notes |
|---|---|---|
| `summary` | string | One-paragraph run summary. |
| `skill_candidates` | `SkillCandidate[]` | New skills to persist. |
| `memory_candidates` | `MemoryCandidate[]` | New memories to persist. |
| `conflicts` | `ConflictCandidate[]` | Tensions with existing knowledge (curator's job). |
| `task_candidates` | `TaskCandidate[]` | Proposed tasks. |
| `reflection_process` | string \| absent | COT trace: observe → hypothesize → validate → conclude. |

Candidate shapes:

- `SkillCandidate`: `name`, `description`, `triggers[]`, `body`,
  `rationale`, `confidence`.
- `MemoryCandidate`: `fact`, `tags[]`, `scope` (`user`\|`project`),
  `confidence`, `rationale`, `supersedes[]`, `zone`, and the COT trio
  `memory_type` / `thinking_path` / `concrete_instance` / `problem_type`
  (the latter three only for `memory_type: "reasoning"`).
- `ConflictCandidate`: `with`, `kind`, `explain`, `options[]`.
- `TaskCandidate`: `title`, `description`, `priority`
  (`low`\|`normal`\|`high`\|`critical`, default `normal`), `labels[]`,
  `reason`.

Golden example (also stored as
`tests/golden/knowledge-format/reflection-output-001.json` and schema-checked
by the golden gate):

```json
{
  "summary": "Debugging session distilled into one reusable thinking path.",
  "skill_candidates": [],
  "memory_candidates": [
    {
      "fact": "Reproduce before theorizing: rerun the failing command and capture the exact panic.",
      "tags": ["debugging"],
      "scope": "user",
      "confidence": "medium",
      "rationale": "Applied successfully twice in this session.",
      "supersedes": [],
      "zone": "thinking-paths",
      "memory_type": "reasoning",
      "thinking_path": "reproduce → isolate → hypothesize → test → conclude",
      "concrete_instance": "Reproduced the panic, isolated the index math, patched the bound check.",
      "problem_type": "debugging"
    }
  ],
  "conflicts": [],
  "task_candidates": [],
  "reflection_process": "observed repeated manual debugging → hypothesized a reusable path → validated against two incidents → concluded it is worth persisting"
}
```

## 5. Golden sample library

Location: `tests/golden/`. Samples are generated from the real structs by
serde serialization (one-shot generator run against
`MemoryFrontmatter` / `SkillFrontmatter` / `ReflectionOutput`; the dumped
output is committed verbatim — hand-maintained to match the serde shape).
The golden gate (Q6) round-trips every frontmatter sample and schema-checks
every D8 JSON sample:

| Sample | Covers |
|---|---|
| `tests/golden/frontmatter/memory-001..003.md` | Baseline memory frontmatter (full field set). |
| `tests/golden/frontmatter/skill-001..003.md` | Baseline skill frontmatter. |
| `tests/golden/frontmatter/unverified-memory-001.md` | Memory with `unverified: true` + COT fields (`thinking_path`, `concrete_instance`, `problem_type`) in the flatten channel. |
| `tests/golden/frontmatter/unverified-skill-001.md` | Skill with `unverified: true` (reflect-lite persist shape, `source: reflect-lite`). |
| `tests/golden/knowledge-format/reflection-output-001.json` | D8 `ReflectionOutput` schema example. |
| `tests/golden/sessions/session-001.jsonl` | Session JSONL with torn-tail tolerance. |

When the serde shape changes, regenerate a sample through the structs — do
not hand-edit it into a shape the structs cannot produce.

## 6. Versioning commitments (1.0)

- **Additive only.** Fields are only ever added, and added as optional
  (serde default) so old writers and old readers keep working. Existing
  fields are never removed, renamed, or re-typed within the 1.0 line.
- **Unknown keys are preserved.** The flatten `extra` channel guarantees
  forward compatibility both ways: newer files read by older code keep
  their unknown keys on rewrite; older files read by newer code parse.
- **`unverified` is forced, not optional, for reflect products.** Any
  producer writing reflect-generated artifacts MUST set `unverified: true`;
  a consumer may treat a missing flag as unverified (safe default).
- **Breaking changes go through RFC.** A format change that is not
  additive (removal, rename, retype, semantic change of a frozen field)
  requires an RFC, a major version bump of the affected crate, and a
  deprecation window of two minor releases — see `docs/compatibility.md`.
- **Schema drift fails CI.** The golden gate re-validates the samples
  above on every run; a serde change without a regenerated sample is a red
  build.
