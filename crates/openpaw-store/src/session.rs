//! Append-only JSONL session writer + reader.
//!
//! One JSONL event per call, fsync on every write, so a crash leaves a
//! valid transcript up to the last event. The line format
//! (`{"type":"...", ...}`) is shared with the upstream store.

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
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

    /// Open an existing session for appending (resume): subsequent events
    /// land at the end of the file alongside the original transcript.
    pub fn open_append(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        let file = OpenOptions::new()
            .append(true)
            .open(&path)
            .map_err(|source| SessionError::Io {
                path: path.clone(),
                source,
            })?;
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
    /// Read the whole transcript.
    pub fn read(path: impl AsRef<Path>) -> Result<ReadOutcome> {
        let path = path.as_ref().to_path_buf();
        let f = File::open(&path).map_err(|source| SessionError::Io {
            path: path.clone(),
            source,
        })?;
        let mut meta: Option<SessionMeta> = None;
        let mut events = Vec::new();
        let mut torn = 0usize;

        for (idx, line) in BufReader::new(f).lines().enumerate() {
            let line = match line {
                Ok(l) => l,
                Err(source) => {
                    return Err(SessionError::Io {
                        path: path.clone(),
                        source,
                    })
                }
            };
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<SessionEvent>(&line) {
                Ok(ev) => {
                    if meta.is_none() {
                        if let SessionEvent::Meta(m) = &ev {
                            meta = Some(m.clone());
                        }
                    }
                    events.push(ev);
                }
                Err(source) => {
                    if idx + 1 == count_lines(&path)? {
                        torn += 1; // torn tail from a crash mid-write
                    } else {
                        return Err(SessionError::Json {
                            path: path.clone(),
                            line: idx + 1,
                            source,
                        });
                    }
                }
            }
        }

        meta.map(|meta| ReadOutcome {
            meta,
            events,
            torn_tail_lines: torn,
        })
        .ok_or(SessionError::MissingMeta { path })
    }
}

fn count_lines(path: &Path) -> Result<usize> {
    let f = File::open(path).map_err(|source| SessionError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(BufReader::new(f).lines().count())
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
