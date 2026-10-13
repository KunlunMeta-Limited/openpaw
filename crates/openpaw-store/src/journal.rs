//! 功能：管理会话追加事务、写入器失效、尾行修复及分支文件发布。
//! 输入输出边界：调用方独占 writer 或明确拥有的会话路径；持久化格式保持兼容。
//! 禁止承担：不执行模型，不获取 Host 锁，不发布终态或协议事件。
//! 关联：CHG-2026-0025。
use crate::transcript::{
    RecordedFailure, TurnFinishCommitV1, TurnFinishPrepareV1, TurnOutcome,
    MAX_FINISH_TRANSACTION_MESSAGES,
};
use crate::{SessionError, SessionReader, SessionWriter};
use openpaw_core::{Message, SessionEvent};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// 功能：捕获当前追加文件的回滚位置。
/// 参数：writer 为由调用方独占的可用写入器。
/// 返回：文件路径及当前字节长度。
/// 错误：写入器失效或读取文件长度失败。
/// 状态：只读。
/// 并发与取消：调用方在整个事务内持有独占写入权，同步不可取消。
/// 权限：仅访问写入器拥有的路径。
/// 关联：CHG-2026-0025。
pub fn snapshot(writer: &Option<SessionWriter>) -> Result<(PathBuf, u64), String> {
    let writer = writer
        .as_ref()
        .ok_or_else(|| "会话写入器不可用".to_string())?;
    let path = writer.path().to_path_buf();
    let length = std::fs::metadata(&path)
        .map_err(|error| format!("无法读取会话文件长度：{error}"))?
        .len();
    Ok((path, length))
}

fn truncate(path: &Path, length: u64) -> Result<(), String> {
    let file = std::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .map_err(|error| format!("打开会话文件失败：{error}"))?;
    file.set_len(length)
        .map_err(|error| format!("截断会话文件失败：{error}"))?;
    file.sync_data()
        .map_err(|error| format!("同步截断后的会话文件失败：{error}"))
}

/// 功能：回滚文件并重建追加写入器。
/// 参数：writer 为独占写入器，path/length 为同一事务开始前的快照。
/// 返回：恢复成功返回 Ok。
/// 错误：截断、同步或重开失败；writer 保持失效，不能继续写入。
/// 状态：先关闭原句柄，再恢复字节边界，成功后重新开放追加。
/// 并发与取消：要求同一调用方独占该会话写入权，不跨 await。
/// 权限：只恢复调用方提供的事务路径，不删除历史文件。
/// 关联：CHG-2026-0025。
pub fn restore(writer: &mut Option<SessionWriter>, path: &Path, length: u64) -> Result<(), String> {
    *writer = None;
    truncate(path, length)?;
    *writer = Some(
        SessionWriter::open_append(path).map_err(|error| format!("无法重建会话写入器：{error}"))?,
    );
    Ok(())
}

fn transaction(
    writer: &mut Option<SessionWriter>,
    operation: impl FnOnce(&mut SessionWriter) -> Result<(), String>,
) -> Result<(), String> {
    let (path, length) = snapshot(writer)?;
    let result = operation(writer.as_mut().expect("available writer"));
    if let Err(error) = result {
        restore(writer, &path, length)
            .map_err(|rollback| format!("{error}；会话事务回滚失败：{rollback}"))?;
        return Err(error);
    }
    Ok(())
}

/// 功能：原子追加一条会话事件。
/// 参数：writer 为独占写入器，event 为既有持久事件。
/// 返回：追加并同步成功返回 Ok。
/// 错误：失败时回滚部分字节，回滚失败时写入器失效。
/// 状态：只成功提交完整事件，不改变调用方内存投影。
/// 并发与取消：同步独占事务，调用方负责会话级线性化。
/// 权限：仅写当前会话，不记录事件正文。
/// 关联：CHG-2026-0025。
pub fn append(writer: &mut Option<SessionWriter>, event: &SessionEvent) -> Result<(), String> {
    transaction(writer, |current| {
        current.append(event).map_err(|error| error.to_string())
    })
}

/// 功能：按既有 Message 格式完整保存产出消息，包括工具结果。
/// 参数：writer 为独占写入器，messages 为尚未保存的产出，events 为对应工具完成事实。
/// 返回：全部同步成功返回 Ok。
/// 错误：任何消息失败时回滚整个批次。
/// 状态：保留角色、内容、顺序和 ID，不按 User 角色过滤工具结果。
/// 并发与取消：调用方独占写入器，事务内无 await。
/// 权限：仅持久化调用方提供的会话消息。
/// 关联：CHG-2026-0025。
pub fn append_messages(
    writer: &mut Option<SessionWriter>,
    messages: &[Message],
    events: &[SessionEvent],
) -> Result<(), String> {
    transaction(writer, |current| {
        for message in messages {
            current
                .append(&SessionEvent::Message(message.clone()))
                .map_err(|error| error.to_string())?;
        }
        for event in events {
            current.append(event).map_err(|error| error.to_string())?;
        }
        Ok(())
    })
}

/// 功能：按既有 prepare/Message/commit 合同提交产出与终态。
/// 参数：writer、turn_id、messages、terminal、failure 为同一已执行回合的数据。
/// 返回：批次及终态同步提交成功返回 Ok。
/// 错误：身份、数量或终态不合法时拒绝；写入失败回滚，回滚失败关闭 writer。
/// 状态：旧重放器只看到已提交批次，不改变终态发布时机。
/// 并发与取消：同一 writer 独占临界区内完成事务，取消与终态由调用方闸门线性化。
/// 权限：不执行模型，不发布错误正文，仅保存调用方已脱敏的失败。
/// 关联：CHG-2026-0025。
pub fn finish(
    writer: &mut Option<SessionWriter>,
    turn_id: &str,
    messages: &[Message],
    terminal: TurnOutcome,
    failure: Option<RecordedFailure>,
) -> Result<(), String> {
    if messages.len() > MAX_FINISH_TRANSACTION_MESSAGES
        || terminal.turn_id != turn_id
        || !terminal.state.is_terminal()
    {
        return Err("回合消息事务不符合持久化约束".into());
    }
    let ids: Option<Vec<_>> = messages
        .iter()
        .map(|message| message.uuid.clone())
        .collect();
    let ids = ids.ok_or_else(|| "回合生成消息缺少稳定 UUID".to_string())?;
    if ids.iter().collect::<HashSet<_>>().len() != ids.len() {
        return Err("回合生成消息 UUID 重复".into());
    }
    transaction(writer, |current| {
        current
            .append(
                &TurnFinishPrepareV1 {
                    turn_id: turn_id.to_string(),
                }
                .to_session_event(),
            )
            .map_err(|error| error.to_string())?;
        for message in messages {
            current
                .append(&SessionEvent::Message(message.clone()))
                .map_err(|error| error.to_string())?;
        }
        current
            .append(
                &TurnFinishCommitV1 {
                    turn_id: turn_id.to_string(),
                    generated_message_ids: ids,
                    terminal,
                    open_code_error: failure,
                }
                .to_session_event(),
            )
            .map_err(|error| error.to_string())
    })
}

/// 功能：在失败后重开原会话并补写一条脱敏终态。
/// 参数：writer、path 为调用方拥有的会话，event 为待补写事件。
/// 返回：完整追加成功返回 Ok。
/// 错误：重开或再次提交失败；writer 保持不可用。
/// 状态：部分追加先回滚；显式重开前修复已确认尾行并验证历史，成功后交还 writer。
/// 并发与取消：调用方独占 writer，执行一次重开，不后台重试。
/// 权限：不改变会话身份，不访问其他文件。
/// 关联：CHG-2026-0025。
pub fn append_with_retry(
    writer: &mut Option<SessionWriter>,
    path: &Path,
    event: &SessionEvent,
) -> Result<(), String> {
    if writer.is_some() {
        match append(writer, event) {
            Ok(()) => return Ok(()),
            Err(error) if writer.is_none() => return Err(error),
            Err(_) => {}
        }
    }
    *writer = None;
    let read = read_repaired(path).map_err(|error| error.to_string())?;
    crate::transcript::replay(&read.meta.id, &read.events)
        .map_err(|_| "无法恢复已损坏的会话历史".to_string())?;
    let mut recovered = Some(SessionWriter::open_append(path).map_err(|error| error.to_string())?);
    append(&mut recovered, event)?;
    *writer = recovered;
    Ok(())
}

/// 功能：读取会话并修复 Reader 已确认的最后一条损坏记录。
/// 参数：path 为调用方独占的恢复目标。
/// 返回：完整有效的 ReadOutcome。
/// 错误：读取、身份、文件中段损坏或修复失败；不丢弃有效历史。
/// 状态：仅移除损坏尾行；普通 catalog read 仍保持只读。
/// 并发与取消：必须在打开 append writer 前且没有其他写入者时调用。
/// 权限：仅修复明确提供的会话路径。
/// 关联：CHG-2026-0025。
pub fn read_repaired(path: &Path) -> Result<crate::session::ReadOutcome, SessionError> {
    let read = SessionReader::read(path)?;
    if path.file_stem().and_then(|name| name.to_str()) != Some(read.meta.id.as_str()) {
        return Err(SessionError::MissingMeta {
            path: path.to_path_buf(),
        });
    }
    if read.torn_tail_lines == 0 {
        return Ok(read);
    }
    let bytes = std::fs::read(path).map_err(|source| SessionError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let end = bytes
        .iter()
        .rposition(|byte| !matches!(byte, b'\r' | b'\n' | b' ' | b'\t'))
        .map(|index| index + 1)
        .unwrap_or(0);
    let start = bytes[..end]
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map(|index| index + 1)
        .unwrap_or(0);
    truncate(path, start as u64).map_err(|error| SessionError::Io {
        path: path.to_path_buf(),
        source: std::io::Error::other(error),
    })?;
    SessionReader::read(path)
}

/// 功能：写入临时会话并同步发布独立分支文件。
/// 参数：temporary/final_path 为调用方拥有的不同路径，events 为完整新分支记录。
/// 返回：发布后已打开的 SessionWriter。
/// 错误：创建、写入、同步、发布或重开失败；清理本次创建的文件。
/// 状态：成功之前不暴露运行时对象，保留既有事件顺序及逐事件 fsync。
/// 并发与取消：调用方持有分支创建闸门；本接口不提供跨进程写锁。
/// 权限：不覆盖已有目标文件，不删除创建失败时已存在的临时文件。
/// 关联：CHG-2026-0025。
pub fn publish(
    temporary: &Path,
    final_path: &Path,
    events: &[SessionEvent],
) -> Result<SessionWriter, String> {
    if temporary == final_path || final_path.exists() {
        return Err("会话分支目标已存在或路径冲突".into());
    }
    let mut writer = SessionWriter::create(temporary).map_err(|error| error.to_string())?;
    let written = events
        .iter()
        .try_for_each(|event| writer.append(event).map_err(|error| error.to_string()));
    drop(writer);
    let published = written.and_then(|_| {
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(temporary)
            .and_then(|file| file.sync_all())
            .map_err(|error| error.to_string())?;
        std::fs::rename(temporary, final_path).map_err(|error| error.to_string())
    });
    if let Err(error) = published {
        let cleanup = std::fs::remove_file(temporary);
        return Err(match cleanup {
            Ok(()) => error,
            Err(cleanup) => format!("{error}；临时分支清理失败：{cleanup}"),
        });
    }
    match SessionWriter::open_append(final_path) {
        Ok(writer) => Ok(writer),
        Err(error) => {
            let cleanup = std::fs::remove_file(final_path);
            Err(match cleanup {
                Ok(()) => error.to_string(),
                Err(cleanup) => format!("{error}；分支清理失败：{cleanup}"),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transcript::{replay, TurnState};
    use openpaw_core::{ContentBlock, Session, SessionMeta};

    fn fixture(dir: &Path) -> (PathBuf, Option<SessionWriter>, Vec<u8>) {
        let meta = SessionMeta::new("fixture", "mock");
        let path = dir.join(format!("{}.jsonl", meta.id));
        let mut writer = Some(SessionWriter::create(&path).unwrap());
        append(&mut writer, &SessionEvent::Meta(meta)).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        (path, writer, bytes)
    }

    fn terminal() -> TurnOutcome {
        TurnOutcome {
            turn_id: "turn-1".into(),
            user_message_id: "input-1".into(),
            state: TurnState::Completed,
            queued_at: None,
            started_at: Some(1),
            completed_at: Some(2),
            error: None,
        }
    }

    #[test]
    fn partial_batch_failure_restores_bytes_and_allows_next_append() {
        let dir = tempfile::tempdir().unwrap();
        let (path, mut writer, baseline) = fixture(dir.path());
        let result = transaction(&mut writer, |current| {
            current
                .append(&SessionEvent::Message(Message::assistant_text("partial")))
                .unwrap();
            Err("injected batch failure after first fsync".into())
        });
        assert!(result.is_err());
        assert_eq!(std::fs::read(&path).unwrap(), baseline);
        append(
            &mut writer,
            &SessionEvent::Message(Message::assistant_text("next")),
        )
        .unwrap();
        let read = SessionReader::read(&path).unwrap();
        let history = replay(&read.meta.id, &read.events).unwrap();
        assert_eq!(history.messages.len(), 1);
        assert_eq!(history.messages[0].text(), "next");
    }

    #[test]
    fn failed_rollback_invalidates_writer_and_refuses_further_writes() {
        let dir = tempfile::tempdir().unwrap();
        let (path, mut writer, _) = fixture(dir.path());
        let result = transaction(&mut writer, |current| {
            current
                .append(&SessionEvent::Message(Message::assistant_text("partial")))
                .unwrap();
            std::fs::remove_file(&path).unwrap();
            Err("injected unavailable file".into())
        });
        assert!(result.unwrap_err().contains("回滚失败"));
        assert!(writer.is_none());
        assert!(append(&mut writer, &terminal().to_session_event()).is_err());
        assert!(!path.exists());
    }

    #[test]
    fn explicit_writer_recovery_repairs_tail_but_refuses_corrupt_history() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let (path, writer, baseline) = fixture(dir.path());
        drop(writer);
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"{\"type\":")
            .unwrap();
        let mut writer = None;
        append_with_retry(&mut writer, &path, &terminal().to_session_event()).unwrap();
        assert!(writer.is_some());
        let read = SessionReader::read(&path).unwrap();
        assert_eq!(
            replay(&read.meta.id, &read.events)
                .unwrap()
                .turn_outcomes
                .len(),
            1
        );
        drop(writer.take());
        let mut corrupt = baseline;
        corrupt.extend_from_slice(b"{\"type\":\"turn_finish_commit_v1\"}\n");
        std::fs::write(&path, &corrupt).unwrap();
        assert!(append_with_retry(&mut writer, &path, &terminal().to_session_event()).is_err());
        assert!(writer.is_none());
        assert_eq!(std::fs::read(&path).unwrap(), corrupt);
    }

    #[test]
    fn finish_rejects_invalid_identity_or_state_before_writing() {
        let dir = tempfile::tempdir().unwrap();
        let (path, mut writer, baseline) = fixture(dir.path());
        let message = Message::assistant_text("reply").with_uuid("reply-1");
        let mut running = terminal();
        running.state = TurnState::Running;
        for (turn, messages, outcome) in [
            ("other", vec![message.clone()], terminal()),
            ("turn-1", vec![message.clone()], running),
            ("turn-1", vec![Message::assistant_text("no ID")], terminal()),
            ("turn-1", vec![message.clone(), message.clone()], terminal()),
        ] {
            assert!(finish(&mut writer, turn, &messages, outcome, None).is_err());
            assert_eq!(std::fs::read(&path).unwrap(), baseline);
        }
        finish(&mut writer, "turn-1", &[message], terminal(), None).unwrap();
        let read = SessionReader::read(&path).unwrap();
        let history = replay(&read.meta.id, &read.events).unwrap();
        assert_eq!(history.messages.len(), 1);
        assert_eq!(history.turn_outcomes, vec![terminal()]);
    }

    #[test]
    fn generated_batch_keeps_user_role_tool_result_and_order() {
        let dir = tempfile::tempdir().unwrap();
        let (path, mut writer, _) = fixture(dir.path());
        let input = Message::user_text("accepted");
        append(&mut writer, &SessionEvent::Message(input)).unwrap();
        let generated = vec![
            Message {
                content: vec![ContentBlock::ToolUse {
                    id: "call-1".into(),
                    name: "read_file".into(),
                    input: serde_json::json!({"path":"fixture.txt"}),
                }],
                ..Message::assistant_text("")
            },
            Message {
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: "call-1".into(),
                    content: "fixture result".into(),
                    is_error: false,
                    name: Some("read_file".into()),
                    protected: false,
                    media: Vec::new(),
                }],
                ..Message::user_text("")
            },
            Message::assistant_text("done"),
        ];
        append_messages(&mut writer, &generated, &[]).unwrap();
        let read = SessionReader::read(&path).unwrap();
        let history = replay(&read.meta.id, &read.events).unwrap();
        assert_eq!(history.messages.len(), 4);
        assert_eq!(
            serde_json::to_value(&history.messages[1..]).unwrap(),
            serde_json::to_value(&generated).unwrap()
        );
    }

    #[test]
    fn recovery_only_removes_confirmed_torn_tail_and_checks_identity() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let (path, writer, baseline) = fixture(dir.path());
        drop(writer);
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"{\"type\":\"message\",\"unfinished\":")
            .unwrap();
        assert_eq!(SessionReader::read(&path).unwrap().torn_tail_lines, 1);
        assert_eq!(read_repaired(&path).unwrap().torn_tail_lines, 0);
        assert_eq!(std::fs::read(&path).unwrap(), baseline);
        let wrong_path = dir.path().join("wrong.jsonl");
        std::fs::copy(&path, &wrong_path).unwrap();
        assert!(read_repaired(&wrong_path).is_err());
        assert_eq!(std::fs::read(wrong_path).unwrap(), baseline);
        let mut corrupt_middle = baseline.clone();
        corrupt_middle.extend_from_slice(b"{bad-json}\n{\"type\":\"future_event\"}\n");
        std::fs::write(&path, &corrupt_middle).unwrap();
        assert!(read_repaired(&path).is_err());
        assert_eq!(std::fs::read(path).unwrap(), corrupt_middle);
    }

    #[test]
    fn fork_publication_reopens_and_only_cleans_files_it_created() {
        let dir = tempfile::tempdir().unwrap();
        let temporary = dir.path().join("fork.tmp");
        let session = Session::new("fixture", "mock");
        let final_path = dir.path().join(format!("{}.jsonl", session.meta.id));
        let events = vec![SessionEvent::Meta(session.meta)];
        let mut writer = publish(&temporary, &final_path, &events).unwrap();
        assert!(!temporary.exists());
        writer
            .append(&SessionEvent::Message(Message::assistant_text("branch")))
            .unwrap();
        drop(writer);
        assert_eq!(SessionReader::read(&final_path).unwrap().events.len(), 2);
        let published = std::fs::read(&final_path).unwrap();
        assert!(publish(&temporary, &final_path, &events).is_err());
        assert_eq!(std::fs::read(&final_path).unwrap(), published);
        assert!(!temporary.exists());
        let unavailable = dir.path().join("missing").join("fork.jsonl");
        assert!(publish(&temporary, &unavailable, &events).is_err());
        assert!(!temporary.exists());
        assert!(!unavailable.exists());
        std::fs::write(&temporary, "preexisting").unwrap();
        assert!(publish(&temporary, &unavailable, &events).is_err());
        assert_eq!(std::fs::read_to_string(temporary).unwrap(), "preexisting");
    }
}
