//! `clipboard` — read from or write to the system clipboard (text only).
//!
//! Every failure path (no clipboard on headless CI, missing helper binary,
//! timeout) returns an error *outcome*, never an `Err`.

use std::path::Path;
use std::time::Duration;

use openpaw_core::{Result, ToolCallOutcome, ToolSpec};

pub fn spec() -> ToolSpec {
    ToolSpec {
        name: "clipboard".into(),
        description: "Read from or write to the system clipboard. Supports text content only."
            .into(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["read", "write"],
                    "description": "Whether to read from or write to the clipboard"
                },
                "content": {
                    "type": "string",
                    "description": "Text content to write (required for write action)"
                }
            },
            "required": ["action"]
        }),
        ..Default::default()
    }
}

pub async fn run(_workspace: &Path, input: serde_json::Value) -> Result<ToolCallOutcome> {
    let action = input["action"].as_str().unwrap_or("");

    match action {
        "read" => {
            #[cfg(target_os = "windows")]
            {
                let script = r#"import sys,ctypes
u=ctypes.windll.user32
k=ctypes.windll.kernel32
u.OpenClipboard(0)
h=u.GetClipboardData(13)
if h:
 p=k.GlobalLock(h)
 s=ctypes.c_wchar_p(p).value or ''
 k.GlobalUnlock(h)
 sys.stdout.write(s)
u.CloseClipboard()"#;
                match tokio::time::timeout(
                    Duration::from_secs(10),
                    tokio::process::Command::new("python")
                        .arg("-c")
                        .arg(script)
                        .kill_on_drop(true)
                        .output(),
                )
                .await
                {
                    Ok(Ok(output)) => {
                        if output.status.success() {
                            let text = String::from_utf8_lossy(&output.stdout);
                            Ok(ToolCallOutcome::ok(text.to_string()))
                        } else {
                            let stderr = String::from_utf8_lossy(&output.stderr);
                            Ok(ToolCallOutcome::error(format!(
                                "Failed to read clipboard: {stderr}"
                            )))
                        }
                    }
                    Ok(Err(e)) => Ok(ToolCallOutcome::error(format!(
                        "Failed to read clipboard: {e}"
                    ))),
                    Err(_) => Ok(ToolCallOutcome::error(
                        "Failed to read clipboard: timed out after 10s",
                    )),
                }
            }
            #[cfg(target_os = "macos")]
            {
                match tokio::time::timeout(
                    Duration::from_secs(10),
                    tokio::process::Command::new("pbpaste")
                        .kill_on_drop(true)
                        .output(),
                )
                .await
                {
                    Ok(Ok(output)) => {
                        let text = String::from_utf8_lossy(&output.stdout);
                        Ok(ToolCallOutcome::ok(text.to_string()))
                    }
                    Ok(Err(e)) => Ok(ToolCallOutcome::error(format!(
                        "Failed to read clipboard: {e}"
                    ))),
                    Err(_) => Ok(ToolCallOutcome::error(
                        "Failed to read clipboard: timed out after 10s",
                    )),
                }
            }
            #[cfg(target_os = "linux")]
            {
                match tokio::time::timeout(
                    Duration::from_secs(10),
                    tokio::process::Command::new("xclip")
                        .args(["-selection", "clipboard", "-o"])
                        .kill_on_drop(true)
                        .output(),
                )
                .await
                {
                    Ok(Ok(output)) => {
                        let text = String::from_utf8_lossy(&output.stdout);
                        Ok(ToolCallOutcome::ok(text.to_string()))
                    }
                    Ok(Err(e)) => Ok(ToolCallOutcome::error(format!(
                        "Failed to read clipboard (is xclip installed?): {e}"
                    ))),
                    Err(_) => Ok(ToolCallOutcome::error(
                        "Failed to read clipboard: timed out after 10s",
                    )),
                }
            }
            #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
            {
                Ok(ToolCallOutcome::error(
                    "Clipboard not supported on this platform",
                ))
            }
        }
        "write" => {
            let content = input["content"].as_str().unwrap_or("");
            if content.is_empty() {
                return Ok(ToolCallOutcome::error(
                    "Error: content is required for write action",
                ));
            }

            #[cfg(target_os = "windows")]
            {
                let script = r#"import sys,ctypes
d=sys.stdin.read()
u=ctypes.windll.user32
k=ctypes.windll.kernel32
buf=ctypes.create_unicode_buffer(d)
u.OpenClipboard(0)
u.EmptyClipboard()
h=k.GlobalAlloc(0x42,ctypes.sizeof(buf))
p=k.GlobalLock(h)
ctypes.memmove(p,ctypes.addressof(buf),ctypes.sizeof(buf))
k.GlobalUnlock(h)
u.SetClipboardData(13,h)
u.CloseClipboard()"#;
                let mut child = match tokio::process::Command::new("python")
                    .arg("-c")
                    .arg(script)
                    .stdin(std::process::Stdio::piped())
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    .kill_on_drop(true)
                    .spawn()
                {
                    Ok(c) => c,
                    Err(e) => {
                        return Ok(ToolCallOutcome::error(format!(
                            "Failed to write clipboard: {e}"
                        )))
                    }
                };

                if let Some(mut stdin) = child.stdin.take() {
                    use tokio::io::AsyncWriteExt;
                    if let Err(e) = stdin.write_all(content.as_bytes()).await {
                        child.kill().await.ok();
                        return Ok(ToolCallOutcome::error(format!(
                            "Failed to write clipboard: {e}"
                        )));
                    }
                    drop(stdin);
                }
                let result =
                    match tokio::time::timeout(Duration::from_secs(10), child.wait_with_output())
                        .await
                    {
                        Ok(r) => match r {
                            Ok(output) => output,
                            Err(e) => {
                                return Ok(ToolCallOutcome::error(format!(
                                    "Failed to write clipboard: {e}"
                                )))
                            }
                        },
                        Err(_) => {
                            return Ok(ToolCallOutcome::error(
                                "Failed to write clipboard: timed out after 10s",
                            ))
                        }
                    };
                if result.status.success() {
                    Ok(ToolCallOutcome::ok("Copied to clipboard"))
                } else {
                    let stderr = String::from_utf8_lossy(&result.stderr);
                    Ok(ToolCallOutcome::error(format!(
                        "Failed to write clipboard: {stderr}"
                    )))
                }
            }
            #[cfg(target_os = "macos")]
            {
                use std::process::Stdio;
                use tokio::io::AsyncWriteExt;
                let mut child = match tokio::process::Command::new("pbcopy")
                    .stdin(Stdio::piped())
                    .kill_on_drop(true)
                    .spawn()
                {
                    Ok(c) => c,
                    Err(e) => {
                        return Ok(ToolCallOutcome::error(format!(
                            "Failed to write clipboard: {e}"
                        )))
                    }
                };
                if let Some(mut stdin) = child.stdin.take() {
                    if let Err(e) = stdin.write_all(content.as_bytes()).await {
                        child.kill().await.ok();
                        return Ok(ToolCallOutcome::error(format!(
                            "Failed to write clipboard: {e}"
                        )));
                    }
                }
                let status = match child.wait().await {
                    Ok(s) => s,
                    Err(e) => {
                        return Ok(ToolCallOutcome::error(format!(
                            "Failed to write clipboard: {e}"
                        )))
                    }
                };
                if status.success() {
                    Ok(ToolCallOutcome::ok("Copied to clipboard"))
                } else {
                    Ok(ToolCallOutcome::error(format!(
                        "Failed to write clipboard (exit {})",
                        status.code().unwrap_or(-1)
                    )))
                }
            }
            #[cfg(target_os = "linux")]
            {
                use std::process::Stdio;
                use tokio::io::AsyncWriteExt;
                let mut child = match tokio::process::Command::new("xclip")
                    .args(["-selection", "clipboard"])
                    .stdin(Stdio::piped())
                    .kill_on_drop(true)
                    .spawn()
                {
                    Ok(c) => c,
                    Err(e) => {
                        return Ok(ToolCallOutcome::error(format!(
                            "Failed to write clipboard: {e}"
                        )))
                    }
                };
                if let Some(mut stdin) = child.stdin.take() {
                    if let Err(e) = stdin.write_all(content.as_bytes()).await {
                        child.kill().await.ok();
                        return Ok(ToolCallOutcome::error(format!(
                            "Failed to write clipboard: {e}"
                        )));
                    }
                }
                let status = match child.wait().await {
                    Ok(s) => s,
                    Err(e) => {
                        return Ok(ToolCallOutcome::error(format!(
                            "Failed to write clipboard: {e}"
                        )))
                    }
                };
                if status.success() {
                    Ok(ToolCallOutcome::ok("Copied to clipboard"))
                } else {
                    Ok(ToolCallOutcome::error(format!(
                        "Failed to write clipboard (exit {})",
                        status.code().unwrap_or(-1)
                    )))
                }
            }
            #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
            {
                let _ = content;
                Ok(ToolCallOutcome::error(
                    "Clipboard not supported on this platform",
                ))
            }
        }
        _ => Ok(ToolCallOutcome::error(format!(
            "Error: unknown clipboard action '{action}'. Use 'read' or 'write'."
        ))),
    }
}
