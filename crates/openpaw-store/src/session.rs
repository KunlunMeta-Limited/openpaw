//! Append-only JSONL session writer + reader.
//!
//! One JSONL event per call, fsync on every write, so a crash leaves a
//! valid transcript up to the last event. The line format
//! (`{"type":"...", ...}`) is shared with the upstream store.

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use openpaw_core::{SessionEvent, SessionMeta};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SessionError {
    #[error("io error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("json error at {path} line {line}: {source}")]
    Json {
        path: PathBuf,
        line: usize,
        #[source]
        source: serde_json::Error,
    },
    #[error("json serialize: {0}")]
    JsonSerialize(
        #[source]
        #[from]
        serde_json::Error,
    ),
    #[error("session at {path} has no meta record on the first valid line")]
    MissingMeta { path: PathBuf },
}

pub type Result<T> = std::result::Result<T, SessionError>;

/// Owns an open file handle and appends one JSONL event per call.
#[derive(Debug)]
pub struct SessionWriter {
    path: PathBuf,
    file: File,
}

impl SessionWriter {
    /// Create a new session file. Errors if it already exists.
    ///
    /// On Unix the file is created with 0o600 permissions so only the owner
    /// can read the conversation transcript.
    pub fn create(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| SessionError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        let file = OpenOptions::new()
            .create_new(true)
            .append(true)
            .open(&path)
            .map_err(|source| SessionError::Io {
                path: path.clone(),
                source,
            })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).map_err(
                |source| SessionError::Io {
                    path: path.clone(),
                    source,
                },
            )?;
        }
        Ok(Self { path, file })
    }

    /// 功能：检测并修复损坏尾后，打开既有账本追加。
    /// 参数：path为调用方拥有独占写权限的JSONL路径。
    /// 返回：可继续追加的Writer；空文件保持旧行为。
    /// 错误：中间损坏、备份/同步/截断失败均拒绝打开。
    /// 状态：仅损坏最后一行会先完整备份再截断；完整尾无换行则补分隔。
    /// 并发与取消：调用方必须持有数据目录或会话独占写锁，不能与其他Writer并发修复。
    /// 权限：备份Unix权限为0600，路径与原会话同目录。
    /// 关联：需求/设计 REQ-OP-004；DES-OP-003。
    pub fn open_append(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        let mut file = OpenOptions::new()
            .read(true)
            .append(true)
            .open(&path)
            .map_err(|source| io_error(&path, source))?;
        let mut data = Vec::new();
        file.read_to_end(&mut data)
            .map_err(|source| io_error(&path, source))?;
        let scanned = scan(&path, &data)?;
        if let Some(offset) = scanned.torn_offset {
            // 备份创建和同步必须先成功；失败时保留原文件全部字节，不能先截断。
            backup(&path, &data)?;
            file.set_len(offset as u64)
                .map_err(|source| io_error(&path, source))?;
            file.sync_all().map_err(|source| io_error(&path, source))?;
        } else if !data.is_empty() && !data.ends_with(b"\n") {
            file.write_all(b"\n")
                .map_err(|source| io_error(&path, source))?;
            file.sync_data().map_err(|source| io_error(&path, source))?;
        }
        Ok(Self { path, file })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append one event and fsync.
    pub fn append(&mut self, event: &SessionEvent) -> Result<()> {
        let line = serde_json::to_string(event)?;
        writeln!(self.file, "{line}").map_err(|source| SessionError::Io {
            path: self.path.clone(),
            source,
        })?;
        self.file.sync_data().map_err(|source| SessionError::Io {
            path: self.path.clone(),
            source,
        })?;
        Ok(())
    }
}

/// Reader over a JSONL transcript. Tolerates a torn final line (crash
/// during write): it is reported via [`ReadOutcome::torn_tail_lines`].
pub struct SessionReader;

/// Result of reading a session file.
#[derive(Debug)]
pub struct ReadOutcome {
    /// The `meta` record from the first valid line.
    pub meta: SessionMeta,
    /// All events in file order, including `Meta`.
    pub events: Vec<SessionEvent>,
    /// Count of trailing lines that failed to parse (torn writes).
    pub torn_tail_lines: usize,
}

impl SessionReader {
    /// 功能：读取完整账本，末尾损坏只报告，不更改原始文件。
    /// 参数：path为已授权账本路径。
    /// 返回：元数据、顺序事实和尾损计数。
    /// 错误：IO、中间损坏或缺少Meta返回诊断。
    /// 状态：无写入。
    /// 并发与取消：同步读取；恢复执行前须协调独占写入。
    /// 权限：由宿主限制数据目录，不接受未授权路径。
    /// 关联：需求/设计 REQ-OP-004；DES-OP-003。
    pub fn read(path: impl AsRef<Path>) -> Result<ReadOutcome> {
        let path = path.as_ref();
        let data = std::fs::read(path).map_err(|source| io_error(path, source))?;
        let scanned = scan(path, &data)?;
        let meta = scanned
            .events
            .iter()
            .find_map(|ev| match ev {
                SessionEvent::Meta(meta) => Some(meta.clone()),
                _ => None,
            })
            .ok_or_else(|| SessionError::MissingMeta { path: path.into() })?;
        Ok(ReadOutcome {
            meta,
            events: scanned.events,
            torn_tail_lines: usize::from(scanned.torn_offset.is_some()),
        })
    }
}

impl ReadOutcome {
    /// 功能：从同一账本恢复模型上下文和累计用量，不执行工具。
    /// 参数：无。
    /// 返回：最后快照叠加后续消息的Session；原始事实仍保留在events。
    /// 错误：无；读取错误已由Reader返回。
    /// 状态：不修改账本，不能将未结束运行自动重放。
    /// 并发与取消：纯内存恢复，最后取消终态更新cancelled字段。
    /// 权限：调用前校验会话和工作区归属。
    /// 关联：需求/设计 REQ-OP-004；DES-OP-003。
    pub fn restore(&self) -> openpaw_core::Session {
        let mut session = openpaw_core::Session::new(&self.meta.model, &self.meta.provider);
        session.meta = self.meta.clone();
        for event in &self.events {
            match event {
                SessionEvent::Message(msg) => session.messages.push(msg.clone()),
                SessionEvent::ContextSnapshot { messages } => session.messages = messages.clone(),
                SessionEvent::Usage(usage) => {
                    session.total_input_tokens = session
                        .total_input_tokens
                        .saturating_add(usage.input_tokens);
                    session.total_output_tokens = session
                        .total_output_tokens
                        .saturating_add(usage.output_tokens);
                }
                SessionEvent::RunStarted { .. } => session.cancelled = false,
                SessionEvent::RunFinished { status, .. } => {
                    session.cancelled = status == "cancelled"
                }
                _ => {}
            }
        }
        session
    }
}

struct Scanned {
    events: Vec<SessionEvent>,
    torn_offset: Option<usize>,
}

// Reader与恢复追加共用一个字节扫描器；UTF-8撕裂也只能在最后一行容忍。
fn scan(path: &Path, data: &[u8]) -> Result<Scanned> {
    let mut scanned = Scanned {
        events: Vec::new(),
        torn_offset: None,
    };
    let mut offset = 0;
    for (index, line) in data.split_inclusive(|byte| *byte == b'\n').enumerate() {
        if !line.iter().all(u8::is_ascii_whitespace) {
            match serde_json::from_slice::<SessionEvent>(line) {
                Ok(event) => scanned.events.push(event),
                Err(source) if offset + line.len() == data.len() => {
                    scanned.torn_offset = Some(offset);
                    let _ = source;
                }
                Err(source) => {
                    return Err(SessionError::Json {
                        path: path.into(),
                        line: index + 1,
                        source,
                    })
                }
            }
        }
        offset += line.len();
    }
    Ok(scanned)
}

fn io_error(path: &Path, source: std::io::Error) -> SessionError {
    SessionError::Io {
        path: path.into(),
        source,
    }
}

fn backup(path: &Path, data: &[u8]) -> Result<()> {
    // create_new避免覆盖旧证据；与原文件同目录，修复失败后可直接人工恢复。
    for attempt in 0..1000 {
        let mut name = path.as_os_str().to_owned();
        name.push(format!(".torn-{}-{attempt}.bak", std::process::id()));
        let target = PathBuf::from(name);
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(&target) {
            Ok(mut file) => {
                file.write_all(data)
                    .and_then(|_| file.sync_all())
                    .map_err(|source| io_error(&target, source))?;
                #[cfg(unix)]
                {
                    let parent = path
                        .parent()
                        .filter(|p| !p.as_os_str().is_empty())
                        .unwrap_or(Path::new("."));
                    File::open(parent)
                        .and_then(|f| f.sync_all())
                        .map_err(|source| io_error(parent, source))?;
                }
                return Ok(());
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(io_error(&target, e)),
        }
    }
    Err(io_error(
        path,
        std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "tail backup names exhausted",
        ),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use openpaw_core::{Message, Usage};

    #[test]
    fn append_and_read_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s1").join("session.jsonl");
        let mut w = SessionWriter::create(&path).unwrap();
        let meta = SessionMeta::new("echo-1", "echo");
        let id = meta.id.clone();
        w.append(&SessionEvent::Meta(meta)).unwrap();
        w.append(&SessionEvent::Message(Message::user_text("hi")))
            .unwrap();
        w.append(&SessionEvent::Message(Message::assistant_text("hello")))
            .unwrap();
        w.append(&SessionEvent::Usage(Usage {
            input_tokens: 3,
            output_tokens: 2,
            ..Default::default()
        }))
        .unwrap();

        let out = SessionReader::read(&path).unwrap();
        assert_eq!(out.meta.id, id);
        assert_eq!(out.events.len(), 4);
        assert_eq!(out.torn_tail_lines, 0);
    }

    #[test]
    fn create_errors_on_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        SessionWriter::create(&path).unwrap();
        assert!(SessionWriter::create(&path).is_err());
        // But open_append works.
        assert!(SessionWriter::open_append(&path).is_ok());
    }

    #[test]
    fn unknown_event_types_are_preserved() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        std::fs::write(
            &path,
            concat!(
                r#"{"type":"meta","id":"x","created_at":"2026-08-30T00:00:00Z","model":"m","provider":"p"}"#,
                "\n",
                r#"{"type":"hologram","depth":3}"#,
                "\n"
            ),
        )
        .unwrap();
        let out = SessionReader::read(&path).unwrap();
        assert_eq!(out.events.len(), 2);
        assert!(matches!(out.events[1], SessionEvent::Unknown(_)));
    }

    #[test]
    fn torn_tail_is_counted_not_fatal() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        std::fs::write(
            &path,
            concat!(
                r#"{"type":"meta","id":"x","created_at":"2026-08-30T00:00:00Z","model":"m","provider":"p"}"#,
                "\n",
                r#"{"type":"mess"#, // torn write
            ),
        )
        .unwrap();
        let out = SessionReader::read(&path).unwrap();
        assert_eq!(out.torn_tail_lines, 1);
    }

    #[test]
    fn missing_meta_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        std::fs::write(
            &path,
            "{\"type\":\"usage\",\"input_tokens\":1,\"output_tokens\":1}\n",
        )
        .unwrap();
        let err = SessionReader::read(&path).unwrap_err();
        assert!(matches!(err, SessionError::MissingMeta { .. }));
    }
}
