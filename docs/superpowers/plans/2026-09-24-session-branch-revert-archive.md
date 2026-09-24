# Session Branch, Revert, and Archive Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make OpenWork Branch, Revert/Unrevert, Rename, and Archive/Unarchive work against an attached OpenPaw Engine Host, with append-only crash recovery.

**Architecture:** Keep mutation events private to `openpaw-engine-host` and fold them with the existing turn transaction projection. `runtime.rs` remains the only owner of session locks, writers, caches, and publication; `http.rs` only validates wire requests and maps typed failures. OpenWork product code remains unchanged and its existing attached-engine journey becomes the cross-repository acceptance proof.

**Tech Stack:** Rust 2021, Axum, Tokio, serde/serde_json, append-only JSONL, Vitest/TypeScript journey tests.

**Spec:** `docs/changes/CHG-2026-0019-会话分支回退与归档.md`

## Global Constraints

- Revert changes conversation history and future model context only; it never restores workspace files.
- JSONL remains append-only. No physical rewrite is permitted except existing failed-append rollback to a captured writer length.
- A replacement prompt commits a staged revert in the same JSONL record as its new turn acceptance; unrevert is impossible after that commit.
- Branch sessions are visible roots and must not expose `parentID`; private fork origin metadata is permitted.
- Fork and archive reject active sessions with HTTP 409. Revert rejects active sessions; unrevert is serialized with prompt acceptance.
- `partID` is unsupported and returns HTTP 400 `unsupported_part_revert`.
- Branch titles append direct-source ordinals: `Title (1)`, `Title (2)`, and branch-of-branch `Title (1) (1)`.
- OpenWork changes are limited to `evals/specs/openpaw-engine-attach.test.ts`.
- Do not modify `openpaw-core`, `openpaw-store`, `openpaw-engine`, Cargo manifests, approval behavior, Office behavior, or CI.

---

### Task 1: Private Mutation Events and Pure Projection

**Files:**
- Create: `crates/openpaw-engine-host/src/session_mutation.rs`
- Modify: `crates/openpaw-engine-host/src/lib.rs`
- Test: `crates/openpaw-engine-host/src/session_mutation.rs`

**Interfaces:**
- Consumes: `openpaw_core::{ContentBlock, Message, SessionEvent}` and `crate::protocol::TurnAcceptV1`.
- Produces: `SessionMetadataV1`, `SessionForkOriginV1`, `SessionRevertStagedV1`, `SessionRevertCommittedV1`, `SessionMutationEvent::from_session_event`, `visible_prefix_len`, `clone_messages_for_fork`.

- [ ] **Step 1: Register the module and write failing roundtrip/projection tests**

```rust
#[test]
fn private_events_roundtrip_and_unknown_versions_are_ignored() {
    let staged = SessionMutationEvent::RevertStaged(SessionRevertStagedV1 {
        message_id: "m2".into(),
    });
    assert_eq!(SessionMutationEvent::from_session_event(&staged.to_session_event()), Some(staged));
    assert!(SessionMutationEvent::from_session_event(&SessionEvent::Unknown(Box::new(
        serde_json::json!({"type":"session_revert_staged_v2","messageID":"m2"}),
    ))).is_none());
}

#[test]
fn fork_clone_rekeys_messages_and_tool_links() {
    let source = tool_conversation_fixture();
    let cloned = clone_messages_for_fork(&source);
    assert_ne!(source[0].uuid, cloned[0].uuid);
    assert_tool_result_points_at_cloned_tool_use(&cloned);
}
```

- [ ] **Step 2: Run the focused unit target and verify RED**

Run: `cargo test -p openpaw-engine-host --features engine-host session_mutation`

Expected: FAIL because the module and event/projection types do not exist.

- [ ] **Step 3: Implement exact private event shapes and pure helpers**

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SessionMetadataV1 {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SessionForkOriginV1 {
    #[serde(rename = "sourceSessionID")]
    pub source_session_id: String,
    pub ordinal: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SessionRevertStagedV1 {
    #[serde(rename = "messageID")]
    pub message_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct SessionRevertCommittedV1 {
    #[serde(rename = "messageID")]
    pub message_id: String,
    pub accept: TurnAcceptV1,
}
```

Use distinct `type` values `session_metadata_v1`, `session_fork_origin_v1`, `session_revert_staged_v1`, `session_revert_cleared_v1`, and `session_revert_committed_v1`. `clone_messages_for_fork` must generate new message UUIDs, remap `ToolUse.id` and matching `ToolResult.tool_use_id`, and remap `CompactBoundary.last_user_message_uuid` when present.

- [ ] **Step 4: Re-run the focused target and verify GREEN**

Run: `cargo test -p openpaw-engine-host --features engine-host session_mutation`

Expected: PASS for roundtrip, visible-prefix validation, staged/cleared/committed folding, and ID remapping.

- [ ] **Step 5: Commit the pure state layer**

```text
git add crates/openpaw-engine-host/src/lib.rs crates/openpaw-engine-host/src/session_mutation.rs
git commit -s -m "feat(engine-host): add session mutation projection"
```

### Task 2: Public Session Wire Contract and SSE Envelope

**Files:**
- Modify: `crates/openpaw-engine-host/src/protocol.rs`
- Modify: `crates/openpaw-engine-host/src/events.rs`
- Modify: `crates/openpaw-engine-host/tests/fixtures/session.json`
- Test: `crates/openpaw-engine-host/src/protocol.rs`
- Test: `crates/openpaw-engine-host/src/events.rs`

**Interfaces:**
- Consumes: private runtime mutation state.
- Produces: `SessionTime.archived`, `SessionWire.revert`, `UpdateSessionRequest`, `ForkSessionRequest`, `RevertSessionRequest`, and `events::session_changed`.

- [ ] **Step 1: Add failing serde and SSE tests**

```rust
#[test]
fn session_wire_serializes_optional_archive_and_revert_without_parent_id() {
    let value = serde_json::to_value(session_fixture(Some(42), Some("m2"))).unwrap();
    assert_eq!(value["time"]["archived"], 42);
    assert_eq!(value["revert"]["messageID"], "m2");
    assert!(value.get("parentID").is_none());
}

#[test]
fn session_event_uses_info_envelope() {
    let event = session_changed("session.updated", &session_fixture(None, None));
    assert_eq!(event.properties["sessionID"], "s1");
    assert_eq!(event.properties["info"]["id"], "s1");
}
```

- [ ] **Step 2: Verify RED**

Run: `cargo test -p openpaw-engine-host --features engine-host session_wire session_event`

Expected: FAIL because optional fields, request bodies, and the shared event constructor are absent.

- [ ] **Step 3: Implement the wire types**

```rust
pub struct SessionRevert {
    #[serde(rename = "messageID")]
    pub message_id: String,
}

pub struct UpdateSessionRequest {
    pub title: Option<String>,
    pub time: Option<UpdateSessionTime>,
}

pub struct ForkSessionRequest {
    #[serde(rename = "messageID")]
    pub message_id: Option<String>,
}

pub struct RevertSessionRequest {
    #[serde(rename = "messageID")]
    pub message_id: String,
    #[serde(rename = "partID")]
    pub part_id: Option<String>,
}
```

All new response fields use `skip_serializing_if = "Option::is_none"`. `events::session_changed(kind, session)` always returns `properties={sessionID,info}` and replaces every ad hoc `session.created`/`session.updated` constructor.

- [ ] **Step 4: Verify GREEN and fixture compatibility**

Run: `cargo test -p openpaw-engine-host --features engine-host protocol::tests events::tests`

Expected: PASS; the old fixture without optional fields still deserializes.

- [ ] **Step 5: Commit the public wire contract**

```text
git add crates/openpaw-engine-host/src/protocol.rs crates/openpaw-engine-host/src/events.rs crates/openpaw-engine-host/tests/fixtures/session.json
git commit -s -m "feat(engine-host): define session mutation wire contract"
```

### Task 3: Metadata Persistence and PATCH

**Files:**
- Modify: `crates/openpaw-engine-host/src/runtime.rs`
- Modify: `crates/openpaw-engine-host/src/http.rs`
- Modify: `crates/openpaw-engine-host/tests/http.rs`

**Interfaces:**
- Consumes: `SessionMetadataV1`, `UpdateSessionRequest`, `events::session_changed`.
- Produces: `AppState::update_session(id, patch) -> Result<SessionWire, SessionMutationError>` and `PATCH /session/:id`.

- [ ] **Step 1: Add failing HTTP tests for rename/archive/unarchive/restart/errors**

```rust
#[tokio::test]
async fn patch_session_persists_title_and_archive_atomically_across_restart() {
    let response = request(&app, "PATCH", format!("/session/{id}"),
        r#"{"title":"Renamed","time":{"archived":123}}"#).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["title"], "Renamed");
    assert_eq!(body["time"]["archived"], 123);
    drop(app);
    assert_eq!(get_json(&restarted, format!("/session/{id}")).await["title"], "Renamed");
}
```

Also assert empty patch/blank title/negative archive are 400, missing session is 404, active archive is 409 with no title side effect, and archive `0` removes the response field.

- [ ] **Step 2: Verify RED**

Run: `cargo test -p openpaw-engine-host --features engine-host --test http patch_session`

Expected: FAIL with PATCH 405 or missing fields.

- [ ] **Step 3: Fold metadata during load/list and implement atomic update**

Under `turn_gate`, reject active archive updates, capture the writer snapshot, append exactly one `session_metadata_v1`, then publish title/archive/updated time. On append failure restore the writer snapshot and leave memory unchanged. Title-only updates remain allowed while active.

- [ ] **Step 4: Register PATCH and map typed errors**

Register `.route("/session/{id}", get(get_session).patch(update_session).delete(delete_session))`. Map validation to 400, missing to 404, active to 409, and persistence failure to 500.

- [ ] **Step 5: Verify GREEN and commit**

Run: `cargo test -p openpaw-engine-host --features engine-host --test http patch_session`

```text
git add crates/openpaw-engine-host/src/runtime.rs crates/openpaw-engine-host/src/http.rs crates/openpaw-engine-host/tests/http.rs
git commit -s -m "feat(engine-host): persist session rename and archive"
```

### Task 4: Atomic Session Fork

**Files:**
- Modify: `crates/openpaw-engine-host/src/runtime.rs`
- Modify: `crates/openpaw-engine-host/src/http.rs`
- Modify: `crates/openpaw-engine-host/tests/http.rs`

**Interfaces:**
- Consumes: `clone_messages_for_fork`, source visible prefix, fork request boundary.
- Produces: `AppState::fork_session(source_id, message_id) -> Result<Arc<SessionHandle>, SessionMutationError>` and real `POST /session/:id/fork`.

- [ ] **Step 1: Add failing fork behavior tests**

Test full-history fork, strict-before boundary fork, source unchanged, new message/tool IDs, root wire shape without `parentID`, staged-revert prefix, titles `(1)/(2)` and branch-of-branch `(1) (1)`, unknown/hidden boundary 400, and active source 409.

- [ ] **Step 2: Verify RED**

Run: `cargo test -p openpaw-engine-host --features engine-host --test http fork_session`

Expected: FAIL because the current route returns the source session.

- [ ] **Step 3: Implement fork publication transaction**

Hold the source `turn_gate`; compute the visible prefix and next direct-source ordinal; build a fresh session/agent. Write meta, workspace metadata, `session_fork_origin_v1`, and rekeyed messages to `<new-id>.jsonl.tmp`; close and fsync, atomically rename to `<new-id>.jsonl`, reopen append, then insert in `sessions` and emit `session.created`. Cleanup any temporary/final file on failure before publication.

- [ ] **Step 4: Replace the no-op HTTP route and verify GREEN**

Run: `cargo test -p openpaw-engine-host --features engine-host --test http fork_session`

Expected: PASS for boundary, title, identity, 400/404/409, and source isolation cases.

- [ ] **Step 5: Commit**

```text
git add crates/openpaw-engine-host/src/runtime.rs crates/openpaw-engine-host/src/http.rs crates/openpaw-engine-host/tests/http.rs
git commit -s -m "feat(engine-host): create durable session branches"
```

### Task 5: Revert and Unrevert State Machine

**Files:**
- Modify: `crates/openpaw-engine-host/src/runtime.rs`
- Modify: `crates/openpaw-engine-host/src/http.rs`
- Modify: `crates/openpaw-engine-host/tests/http.rs`

**Interfaces:**
- Produces: `AppState::revert_session`, `AppState::unrevert_session`, real revert/unrevert routes, and visible message snapshots.

- [ ] **Step 1: Add failing staged/cleared/restart/error tests**

Assert revert hides the boundary and tail from `/message`, sets `Session.revert.messageID`, survives restart, and emits `session.updated`. Assert unrevert restores the tail, survives restart, and is idempotent without a staged cursor. Assert missing/hidden message is 400, supplied `partID` is 400 with `unsupported_part_revert`, missing session is 404, and active revert is 409.

- [ ] **Step 2: Verify RED**

Run: `cargo test -p openpaw-engine-host --features engine-host --test http revert_session unrevert_session`

Expected: FAIL because revert is a no-op and unrevert is 404.

- [ ] **Step 3: Implement append-only staged/cleared mutations**

Serialize with `turn_gate`. Append the private event before publishing the cursor/cache change. Keep the full uncommitted message list in the in-memory `Session`; expose only `messages[..visible_prefix_len]` through `message_cache`. Unrevert appends cleared and republishes the full list. Roll back the writer on persistence failure.

- [ ] **Step 4: Verify GREEN and commit**

Run: `cargo test -p openpaw-engine-host --features engine-host --test http revert_session unrevert_session`

```text
git add crates/openpaw-engine-host/src/runtime.rs crates/openpaw-engine-host/src/http.rs crates/openpaw-engine-host/tests/http.rs
git commit -s -m "feat(engine-host): stage and clear conversation reverts"
```

### Task 6: Commit Revert with the Next Prompt

**Files:**
- Modify: `crates/openpaw-engine-host/src/runtime.rs`
- Modify: `crates/openpaw-engine-host/tests/http.rs`

**Interfaces:**
- Consumes: `SessionRevertCommittedV1 { message_id, accept: TurnAcceptV1 }`.
- Produces: crash-atomic revert commit integrated into `accept_turn`.

- [ ] **Step 1: Add failing prompt commit tests**

Assert request replay/conflict leaves staged revert unchanged; accepted prompt writes a single `session_revert_committed_v1`, removes reverted messages/outcomes/request admissions, preserves the turn sequence high-water mark, makes unrevert a no-op, and after restart sends only retained prefix plus the replacement prompt to the echo provider. Add failed-append injection coverage proving file and memory stay unchanged.

- [ ] **Step 2: Verify RED**

Run: `cargo test -p openpaw-engine-host --features engine-host --test http revert_commit`

Expected: FAIL because `accept_turn` still appends `turn_accept_v1` without committing the cursor.

- [ ] **Step 3: Replace the accept record only when staged**

After requestID replay/conflict and scheduler admission succeed, build the initial `TurnAcceptV1`. If staged, append one `session_revert_committed_v1` containing that accept record; otherwise append the existing `turn_accept_v1`. Publish message, outcome, admission, cursor, cache, and session messages only after fsync. Preserve `turn_sequence` rather than deriving it from the truncated outcomes.

- [ ] **Step 4: Verify GREEN and commit**

Run: `cargo test -p openpaw-engine-host --features engine-host --test http revert_commit`

```text
git add crates/openpaw-engine-host/src/runtime.rs crates/openpaw-engine-host/tests/http.rs
git commit -s -m "feat(engine-host): commit reverts with replacement prompts"
```

### Task 7: Host Contract Matrix and OpenWork Journey

**Files:**
- Modify: `crates/openpaw-engine-host/tests/http.rs`
- Modify: `openwork-ui/evals/specs/openpaw-engine-attach.test.ts`

**Interfaces:**
- Consumes: completed Host REST/SSE behavior.
- Produces: observable attached-engine regression proof without OpenWork product changes.

- [ ] **Step 1: Add the failing SSE/auth/concurrency matrix**

Extend Host tests so all four write routes require Basic Auth; `session.created` and `session.updated` carry `{sessionID,info}`; fork/archive/revert races with prompt/delete return one linearized outcome; restart restores archived, fork, staged, cleared, and committed states.

- [ ] **Step 2: Run the Host matrix**

Run: `cargo test -p openpaw-engine-host --features engine-host --test http`

Expected before final fixes: at least one newly added observable contract fails. Expected after fixes: PASS.

- [ ] **Step 3: Replace OpenWork's fork/revert no-op assertions**

The existing journey must create a source conversation and assert archive/restart/unarchive, full and boundary forks with new IDs, source immutability, staged revert/unrevert, committed revert after replacement prompt, and the same operations through `bootAttachedWorld`. Keep `command`, `shell`, and `summarize` in the documented degradation loop.

- [ ] **Step 4: Run OpenWork type and journey checks**

Run from `openwork-ui`:

```text
pnpm typecheck
pnpm evals:typecheck
pnpm evals:pr specs/openpaw-engine-attach.test.ts
```

Expected: type checks pass; journey reports zero skips and all assertions pass when pointed at the newly built Host.

- [ ] **Step 5: Commit each repository independently**

```text
# OpenPaw
git add crates/openpaw-engine-host/tests/http.rs
git commit -s -m "test(engine-host): cover session mutation contracts"

# OpenWork
git add evals/specs/openpaw-engine-attach.test.ts
git commit -s -m "test(openwork): verify attached session mutations"
```

### Task 8: Schema, Baselines, Budgets, and Final Verification

**Files:**
- Create: `contracts/host-session-v1.schema.json`
- Modify: `contracts/README.md`
- Modify: `xtask/src/lines.rs`
- Modify: `docs/governance/项目设计基线.md`
- Modify: `docs/governance/需求验收基线.md`
- Modify: `docs/b-line/protocol.md`
- Modify: `docs/b-line/testing.md`
- Modify: `docs/b-line/traceability.md`
- Modify: `docs/b-line/verification-report.md`
- Modify: `docs/changes/OpenPaw-A-B合并与联通验收报告.md`
- Modify: `docs/changes/CHG-2026-0019-会话分支回退与归档.md`

**Interfaces:**
- Produces: machine-readable Session response contract, authoritative docs, actual evidence record, and Host 5700/test 1900 budgets.

- [ ] **Step 1: Write the schema and update baselines from implemented behavior**

The schema requires `id/title/slug/directory/time.created/time.updated`, permits optional positive `time.archived`, and optional `revert.messageID`; it does not define `parentID`. Move fork/revert out of degradation docs, add PATCH/unrevert/error codes/SSE envelopes, and document the incompatible direct downgrade after committed reverts.

- [ ] **Step 2: Raise only the approved line budgets**

```rust
("openpaw-engine-host", 5_700),
const ENGINE_HOST_TEST_CAP: usize = 1_900;
```

- [ ] **Step 3: Run focused and full Rust verification**

```text
cargo fmt --all -- --check
cargo test -p openpaw-engine-host --features engine-host session_mutation
cargo test -p openpaw-engine-host --features engine-host --test http
cargo test -p openpaw-engine-host --features engine-host
cargo clippy -p openpaw-engine-host --features engine-host --all-targets -- -D warnings
cargo run -p xtask -- lines
cargo tree -p openpaw-cli --no-default-features
cargo build -p openpaw-cli --features engine-host --locked
```

Expected: all commands exit 0; the no-default tree contains no `axum`; line counts are within 5700/1900.

- [ ] **Step 4: Run governance checks and record exact evidence**

```text
python governance/check.py validate --profile governance/project-profile.toml
python governance/check.py contracts --dir contracts
python governance/check.py comments --root . --language rust --all --report-only
python governance/check.py pr-check
python governance/check.py comments-diff
```

Record every command, exit code, count, and environment blocker in CHG section 12. Do not record a blocked command as passed and do not fill CODEOWNER or release approval fields.

- [ ] **Step 5: Final diff review and documentation commit**

```text
git diff --check
git status --short
git diff --stat origin/merge/openpaw-engine_host...HEAD
git add contracts docs xtask/src/lines.rs
git commit -s -m "docs: publish session mutation contract and evidence"
```
