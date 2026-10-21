//! 公共读取恢复已有执行事实；fixture 表示中断时已可靠保存的日志前缀。
#![cfg(feature = "engine-host")]
#[allow(dead_code)]
mod support;
use openpaw_core::{ContentBlock, Message, SessionEvent, SessionMeta};
use openpaw_engine_host::{build_router, AppState, HostConfig};
use openpaw_store::transcript::{TurnOutcome, TurnState};
use openpaw_store::{execution_attempts::ExecutionAttemptV1, SessionWriter};
use support::{get_json, request, response_json};

fn interrupted(home: &std::path::Path, workspace: &std::path::Path, tail: bool) -> String {
    let id = "recovery_fixture".to_string();
    let mut meta = SessionMeta::new("echo-1", "echo");
    meta.id = id.clone();
    let path = home.join("sessions").join(format!("{id}.jsonl"));
    let mut writer = SessionWriter::create(&path).unwrap();
    writer.append(&SessionEvent::Meta(meta)).unwrap();
    writer
        .append(&SessionEvent::Metadata {
            title: None,
            mode: None,
            model_override: None,
            cwd: Some(workspace.to_string_lossy().into()),
            workspace_id: None,
            workspace_name: None,
            task_id: None,
        })
        .unwrap();
    let mut input = Message::user_text("original input");
    input.uuid = Some("original-input".into());
    writer.append(&SessionEvent::Message(input)).unwrap();
    let turn = TurnOutcome {
        turn_id: "interrupted".into(),
        user_message_id: "original-input".into(),
        state: TurnState::Running,
        queued_at: Some(1),
        started_at: Some(2),
        completed_at: None,
        error: None,
    };
    writer.append(&turn.to_session_event()).unwrap();
    for (sequence, kind, payload) in [
        (1, "response_start", serde_json::json!({})),
        (
            2,
            "response",
            serde_json::json!({"content":[ContentBlock::ToolUse {id:"unknown-call".into(),name:"write_file".into(),input:serde_json::json!({"path":"effect.txt","content":"actual effect"})},ContentBlock::ToolUse{id:"known-call".into(),name:"read_file".into(),input:serde_json::json!({"path":"known.txt"})}]}),
        ),
        (
            3,
            "tool_start",
            serde_json::json!({"call_id":"unknown-call","name":"write_file","input":{"path":"effect.txt","content":"actual effect"},"authorization":"approved"}),
        ),
        (
            4,
            "tool_start",
            serde_json::json!({"call_id":"known-call","name":"read_file","input":{"path":"known.txt"},"authorization":"approved"}),
        ),
        (
            5,
            "tool_result",
            serde_json::json!({"call_id":"known-call","name":"read_file","outcome":openpaw_core::ToolCallOutcome::ok("durable known result"),"disposition":"completed","duration_ms":12}),
        ),
    ] {
        let record = ExecutionAttemptV1 {
            turn_id: "interrupted".into(),
            sequence,
            response_index: 0,
            occurred_at: sequence as i64,
            kind: kind.into(),
            payload,
        };
        writer
            .append(&openpaw_store::transcript::encode(
                &record,
                "execution_attempt_v1",
            ))
            .unwrap();
    }
    drop(writer);
    std::fs::write(workspace.join("effect.txt"), "actual effect").unwrap();
    if tail {
        use std::io::Write;
        std::fs::OpenOptions::new()
            .append(true)
            .open(path)
            .unwrap()
            .write_all(b"{\"type\":\"execution_attempt_v1\"")
            .unwrap();
    }
    id
}

#[tokio::test]
async fn restart_persists_unknown_once_repairs_only_tail_and_keeps_effects() {
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let id = interrupted(home.path(), workspace.path(), true);
    let config = HostConfig {
        home: home.path().into(),
        workspace: workspace.path().into(),
        provider: "echo".into(),
        ..Default::default()
    };
    let app = build_router(AppState::new(config.clone()));
    let history = get_json(&app, format!("/session/{id}/message")).await;
    let tool = &history
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|m| m["parts"].as_array().unwrap())
        .find(|p| p["callID"] == "unknown-call")
        .unwrap()["state"];
    assert_eq!(tool["disposition"], "unknown");
    assert_eq!(
        tool["error"],
        "Host 重启后无法确认工具结果；可能已经产生副作用，未自动重试。"
    );
    let known = &history
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|m| m["parts"].as_array().unwrap())
        .find(|p| p["callID"] == "known-call")
        .unwrap()["state"];
    assert_eq!(known["output"], "durable known result");
    assert_eq!(known["disposition"], "completed");
    assert_eq!(known["durationMs"], 12);
    let bytes = std::fs::read(home.path().join("sessions").join(format!("{id}.jsonl"))).unwrap();
    let restarted = build_router(AppState::new(config));
    assert_eq!(
        get_json(&restarted, format!("/session/{id}/message")).await,
        history
    );
    assert_eq!(
        std::fs::read(home.path().join("sessions").join(format!("{id}.jsonl"))).unwrap(),
        bytes
    );
    assert_eq!(
        std::fs::read_to_string(workspace.path().join("effect.txt")).unwrap(),
        "actual effect"
    );
    let turns = get_json(&restarted, format!("/session/{id}/turns")).await;
    assert_eq!(turns[0]["state"], "failed");
    assert_eq!(turns[0]["error"]["code"], "host_restarted");
    let mid = history
        .as_array()
        .unwrap()
        .iter()
        .find(|m| {
            m["parts"]
                .as_array()
                .unwrap()
                .iter()
                .any(|p| p["callID"] == "unknown-call")
        })
        .unwrap()["info"]["id"]
        .as_str()
        .unwrap();
    let fork = request(
        &restarted,
        "POST",
        format!("/session/{id}/fork"),
        serde_json::json!({"messageID":mid}).to_string(),
    )
    .await;
    assert!(fork.status().is_success());
    let fork = response_json(fork).await;
    assert!(fork["parentID"].is_null());
    let fork_history = get_json(
        &restarted,
        format!("/session/{}/message", fork["id"].as_str().unwrap()),
    )
    .await;
    assert!(!fork_history.to_string().contains("unknown-call"));
    let revert = request(
        &restarted,
        "POST",
        format!("/session/{id}/revert"),
        serde_json::json!({"messageID":mid}).to_string(),
    )
    .await;
    assert!(revert.status().is_success());
}

#[tokio::test]
async fn restart_with_saved_request_but_no_start_does_not_claim_tool_never_ran() {
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let id = interrupted(home.path(), workspace.path(), false);
    let path = home.path().join("sessions").join(format!("{id}.jsonl"));
    let saved = std::fs::read_to_string(&path).unwrap();
    let prefix = saved
        .lines()
        .take_while(|line| !line.contains("tool_start"))
        .map(|line| format!("{line}\n"))
        .collect::<String>();
    std::fs::write(&path, prefix).unwrap();
    let app = build_router(AppState::new(HostConfig {
        home: home.path().into(),
        workspace: workspace.path().into(),
        provider: "echo".into(),
        ..Default::default()
    }));
    let history = get_json(&app, format!("/session/{id}/message")).await;
    for tool in history
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|m| m["parts"].as_array().unwrap())
        .filter(|p| p["type"] == "tool")
    {
        assert_eq!(tool["state"]["disposition"], "unknown");
    }
}

#[tokio::test]
async fn corrupt_middle_is_rejected_without_empty_replacement_or_losing_saved_prefix() {
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let id = interrupted(home.path(), workspace.path(), false);
    let path = home.path().join("sessions").join(format!("{id}.jsonl"));
    let original = std::fs::read_to_string(&path).unwrap();
    let (first, rest) = original.split_once('\n').unwrap();
    let corrupt = format!("{first}\ninvalid-middle-json\n{rest}");
    std::fs::write(&path, &corrupt).unwrap();
    let app = build_router(AppState::new(HostConfig {
        home: home.path().into(),
        workspace: workspace.path().into(),
        provider: "echo".into(),
        ..Default::default()
    }));
    let response = request(&app, "GET", format!("/session/{id}/message"), "").await;
    assert_eq!(response.status(), axum::http::StatusCode::CONFLICT);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), corrupt);
    assert_eq!(
        std::fs::read_dir(home.path().join("sessions"))
            .unwrap()
            .count(),
        1
    );
    assert_eq!(
        std::fs::read_to_string(workspace.path().join("effect.txt")).unwrap(),
        "actual effect"
    );
}

#[tokio::test]
async fn cancelled_attempt_boundary_can_fork_and_revert_without_copying_tool_attempts() {
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let id = interrupted(home.path(), workspace.path(), false);
    let mut writer =
        SessionWriter::open_append(home.path().join("sessions").join(format!("{id}.jsonl")))
            .unwrap();
    writer
        .append(
            &TurnOutcome {
                turn_id: "interrupted".into(),
                user_message_id: "original-input".into(),
                state: TurnState::Cancelled,
                queued_at: Some(1),
                started_at: Some(2),
                completed_at: Some(4),
                error: None,
            }
            .to_session_event(),
        )
        .unwrap();
    drop(writer);
    let app = build_router(AppState::new(HostConfig {
        home: home.path().into(),
        workspace: workspace.path().into(),
        provider: "echo".into(),
        ..Default::default()
    }));
    let history = get_json(&app, format!("/session/{id}/message")).await;
    let mid = history
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["info"]["execution"]["state"] == "cancelled")
        .unwrap()["info"]["id"]
        .as_str()
        .unwrap();
    let fork = request(
        &app,
        "POST",
        format!("/session/{id}/fork"),
        serde_json::json!({"messageID":mid}).to_string(),
    )
    .await;
    assert!(
        fork.status().is_success(),
        "cancelled attempt is a valid public boundary"
    );
    let fork = response_json(fork).await;
    assert!(!get_json(
        &app,
        format!("/session/{}/message", fork["id"].as_str().unwrap())
    )
    .await
    .to_string()
    .contains("unknown-call"));
    let revert = request(
        &app,
        "POST",
        format!("/session/{id}/revert"),
        serde_json::json!({"messageID":mid}).to_string(),
    )
    .await;
    assert!(revert.status().is_success());
}
