//! §3.2 protocol snapshot fixtures (task T2).
//!
//! Every file under `tests/fixtures/` is the representative JSON for one §3.2
//! category. Each is parsed into its frozen `protocol::` type and written back
//! out; the result must equal the fixture exactly, so the snapshot cannot drift
//! on field names, empty strings, optional fields or ID casing.
//!
//! The fixture inventory is part of the contract too: `FIXTURE_FILES` is the
//! single source of truth, so a file added without a case (or a case without a
//! file) fails `fixture_inventory_matches_disk`.
#![cfg(feature = "engine-host")]

use openpaw_engine_host::protocol::{
    CreateSession, Message, MessagePartDeltaEvent, MessagePartUpdatedEvent, MessageUpdatedEvent,
    Part, PermissionAskedEvent, PermissionRepliedEvent, PermissionReply, PermissionRequest, Prompt,
    ProviderList, Session, SessionCreatedEvent, SessionErrorEvent, SessionIdleEvent,
    SessionStatusEvent, SessionUpdatedEvent, Status, TodoItem, TodoUpdatedEvent,
};
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;
use std::any::type_name;
use std::path::PathBuf;

/// Snake-case ID spellings that must never reach the wire.
const FORBIDDEN_KEYS: &[&str] = &[
    "session_id",
    "message_id",
    "part_id",
    "call_id",
    "provider_id",
    "model_id",
    "request_id",
];

/// Read one fixture. `include_str!` embeds the bytes at compile time, so the
/// snapshot never depends on where the source tree happens to be mounted — a
/// cached `target/` reused from a different mount point (say `/src` vs
/// `/workspace`) cannot make the suite read a stale absolute path.
macro_rules! fixture {
    ($file:literal) => {
        include_str!(concat!("fixtures/", $file))
    };
}

macro_rules! fixture_cases {
    ($($test:ident => $file:literal : $ty:ty),+ $(,)?) => {
        /// Every fixture that must exist under `tests/fixtures/`.
        const FIXTURE_FILES: &[&str] = &[$($file),+];

        $(
            #[test]
            fn $test() {
                let value = round_trip::<$ty>($file, fixture!($file));
                assert_no_snake_case_keys($file, &value);
            }
        )+
    };
}

fixture_cases! {
    session_round_trips => "session.json": Session,
    empty_create_body_round_trips => "create-session-empty.json": CreateSession,
    titled_create_body_round_trips => "create-session-title.json": CreateSession,
    prompt_body_round_trips => "prompt-body.json": Prompt,
    user_message_round_trips => "message-user.json": Message,
    assistant_message_round_trips => "message-assistant.json": Message,
    text_part_round_trips => "part-text.json": Part,
    reasoning_part_round_trips => "part-reasoning.json": Part,
    running_tool_part_round_trips => "part-tool-running.json": Part,
    completed_tool_part_round_trips => "part-tool-completed.json": Part,
    error_tool_part_round_trips => "part-tool-error.json": Part,
    todo_list_round_trips => "todo-list.json": Vec<TodoItem>,
    default_provider_list_round_trips => "provider-list-default.json": ProviderList,
    configured_provider_list_round_trips => "provider-list-fully-configured.json": ProviderList,
    permission_request_round_trips => "permission-request.json": PermissionRequest,
    permission_reply_round_trips => "permission-reply.json": PermissionReply,
    status_round_trips => "status.json": Status,
    session_created_event_round_trips => "sse-session-created.json": SessionCreatedEvent,
    session_updated_event_round_trips => "sse-session-updated.json": SessionUpdatedEvent,
    session_status_event_round_trips => "sse-session-status.json": SessionStatusEvent,
    session_idle_event_round_trips => "sse-session-idle.json": SessionIdleEvent,
    message_updated_event_round_trips => "sse-message-updated.json": MessageUpdatedEvent,
    message_part_updated_event_round_trips => "sse-message-part-updated.json": MessagePartUpdatedEvent,
    message_part_delta_event_round_trips => "sse-message-part-delta.json": MessagePartDeltaEvent,
    todo_updated_event_round_trips => "sse-todo-updated.json": TodoUpdatedEvent,
    permission_asked_event_round_trips => "sse-permission-asked.json": PermissionAskedEvent,
    permission_replied_event_round_trips => "sse-permission-replied.json": PermissionRepliedEvent,
    session_error_event_round_trips => "sse-session-error.json": SessionErrorEvent,
}

/// The inventory is asserted against the directory, so fixtures and cases
/// cannot drift apart silently.
#[test]
fn fixture_inventory_matches_disk() {
    let mut on_disk: Vec<String> = std::fs::read_dir(fixtures_dir())
        .expect("tests/fixtures must exist")
        .map(|entry| {
            entry
                .expect("readable directory entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .filter(|name| name.ends_with(".json"))
        .collect();
    on_disk.sort();

    let mut declared: Vec<String> = FIXTURE_FILES
        .iter()
        .map(|name| (*name).to_string())
        .collect();
    declared.sort();

    assert_eq!(
        on_disk, declared,
        "tests/fixtures/ and the case list disagree"
    );
}

/// §3.2 spells every ID in camelCase; the wire must not leak the Rust field
/// names instead.
#[test]
fn snapshot_uses_camel_case_ids() {
    let message =
        round_trip::<Message>("message-assistant.json", fixture!("message-assistant.json"));
    assert!(message["info"]["sessionID"].is_string());
    assert!(message["parts"][0]["messageID"].is_string());
    assert!(message["parts"][0]["sessionID"].is_string());

    let tool = round_trip::<Part>(
        "part-tool-completed.json",
        fixture!("part-tool-completed.json"),
    );
    assert!(tool["callID"].is_string(), "tool part must carry callID");

    let delta = round_trip::<MessagePartDeltaEvent>(
        "sse-message-part-delta.json",
        fixture!("sse-message-part-delta.json"),
    );
    assert!(delta["properties"]["partID"].is_string());
    assert!(delta["properties"]["messageID"].is_string());
    assert!(delta["properties"]["sessionID"].is_string());

    let replied = round_trip::<PermissionRepliedEvent>(
        "sse-permission-replied.json",
        fixture!("sse-permission-replied.json"),
    );
    assert!(replied["properties"]["requestID"].is_string());

    let permission = round_trip::<PermissionRequest>(
        "permission-request.json",
        fixture!("permission-request.json"),
    );
    assert!(permission["sessionID"].is_string());

    let prompt = round_trip::<Prompt>("prompt-body.json", fixture!("prompt-body.json"));
    assert!(prompt["model"]["providerID"].is_string());
    assert!(prompt["model"]["modelID"].is_string());
}

/// §3.2 pins `time` to Unix **milliseconds** ("`time` 使用 Unix 毫秒"). A
/// seconds-scale value would be a silent 1000x drift, so the unit is asserted
/// on the snapshot itself.
#[test]
fn snapshot_timestamps_are_unix_milliseconds() {
    // 1e12 ms is 2001-09-09; any real timestamp is far above it, while a
    // seconds-scale timestamp (~1.7e9) is far below.
    const MILLISECOND_FLOOR: i64 = 1_000_000_000_000;

    let session = round_trip::<Session>("session.json", fixture!("session.json"));
    assert!(session["time"]["created"].as_i64().unwrap() > MILLISECOND_FLOOR);
    assert!(session["time"]["updated"].as_i64().unwrap() > MILLISECOND_FLOOR);

    let message = round_trip::<Message>("message-user.json", fixture!("message-user.json"));
    assert!(message["info"]["time"]["created"].as_i64().unwrap() > MILLISECOND_FLOOR);
    assert!(message["info"]["time"]["completed"].as_i64().unwrap() > MILLISECOND_FLOOR);
}

/// Live agent bytes for a running tool part carry no `output`/`error` keys at
/// all (`{"status":"running","input":{}}`). §3.2's snapshot spells them as `""`;
/// the fixture follows the host and the text discrepancy is tracked as D-6.
#[test]
fn running_tool_part_omits_empty_output_and_error() {
    let value = round_trip::<Part>("part-tool-running.json", fixture!("part-tool-running.json"));
    assert!(
        value["state"].get("output").is_none(),
        "empty output must stay off the wire"
    );
    assert!(
        value["state"].get("error").is_none(),
        "empty error must stay off the wire"
    );
    assert!(
        value["state"].get("durationMs").is_none(),
        "absent durationMs must stay absent"
    );
    assert_eq!(value["state"]["status"], "running");
}

/// The error tool part keeps a non-empty `error` (and `durationMs`) while its
/// empty `output` is omitted — the shape observed live for a failed `read_file`.
#[test]
fn error_tool_part_keeps_error_and_omits_empty_output() {
    let value = round_trip::<Part>("part-tool-error.json", fixture!("part-tool-error.json"));
    assert!(value["state"]["error"]
        .as_str()
        .is_some_and(|e| !e.is_empty()));
    assert!(value["state"].get("output").is_none());
    assert!(value["state"]["durationMs"].is_u64());
}

/// §3.2 pins the four supported provider ids in `all`. `connected` mirrors what
/// the host can actually call, so the default snapshot carries only the
/// credential-free providers and the configured snapshot carries all four.
/// See D-1 in `docs/changes/2026-09-17-t2-protocol-fixtures.md`.
#[test]
fn provider_snapshots_cover_the_four_snapshot_ids() {
    const FOUR: [&str; 4] = ["mock", "echo", "anthropic", "openai"];

    let default = round_trip::<ProviderList>(
        "provider-list-default.json",
        fixture!("provider-list-default.json"),
    );
    let all: Vec<&str> = default["all"]
        .as_array()
        .expect("`all` must be an array")
        .iter()
        .map(|provider| provider["id"].as_str().expect("provider id"))
        .collect();
    assert_eq!(all, FOUR, "`all` must list the four §3.2 ids");
    assert_eq!(default["connected"], serde_json::json!(["mock", "echo"]));

    let configured = round_trip::<ProviderList>(
        "provider-list-fully-configured.json",
        fixture!("provider-list-fully-configured.json"),
    );
    let connected: Vec<&str> = configured["connected"]
        .as_array()
        .expect("`connected` must be an array")
        .iter()
        .map(|id| id.as_str().expect("provider id"))
        .collect();
    assert_eq!(
        connected, FOUR,
        "with both credentials set, `connected` is all four"
    );
}

fn fixtures_dir() -> PathBuf {
    // `CARGO_MANIFEST_DIR` is baked in at compile time and therefore goes stale
    // whenever a cached target directory is reused from a different mount point.
    // Cargo runs integration tests with the package root as the working
    // directory, so prefer paths that resolve right now and only fall back to
    // the baked value.
    if let Ok(cwd) = std::env::current_dir() {
        for candidate in [
            cwd.join("tests/fixtures"),
            cwd.join("crates/openpaw-engine-host/tests/fixtures"),
        ] {
            if candidate.is_dir() {
                return candidate;
            }
        }
    }
    let baked = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    if baked.is_dir() {
        return baked;
    }
    panic!(
        "cannot locate tests/fixtures (cwd={:?}, manifest_dir={})",
        std::env::current_dir(),
        env!("CARGO_MANIFEST_DIR")
    );
}

/// Parse the fixture into `T`, serialize it back, and require the snapshot to
/// come out unchanged.
fn round_trip<T: DeserializeOwned + Serialize>(name: &str, raw: &str) -> Value {
    let expected: Value = serde_json::from_str(raw)
        .unwrap_or_else(|error| panic!("{name}: fixture is not valid JSON: {error}"));
    let typed: T = serde_json::from_str(raw)
        .unwrap_or_else(|error| panic!("{name}: parse into {} failed: {error}", type_name::<T>()));
    let actual = serde_json::to_value(&typed)
        .unwrap_or_else(|error| panic!("{name}: re-serialize failed: {error}"));
    assert_eq!(actual, expected, "{name}: round-trip changed the snapshot");
    actual
}

fn assert_no_snake_case_keys(name: &str, value: &Value) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                assert!(
                    !FORBIDDEN_KEYS.contains(&key.as_str()),
                    "{name}: snake_case key `{key}` reached the wire"
                );
                assert_no_snake_case_keys(name, child);
            }
        }
        Value::Array(items) => {
            for item in items {
                assert_no_snake_case_keys(name, item);
            }
        }
        _ => {}
    }
}
