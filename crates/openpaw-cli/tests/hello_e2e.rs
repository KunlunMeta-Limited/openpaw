//! E2E: `openpaw hello` first-response timing (M1 exit criterion).
//!
//! Drives the real binary with piped stdin and asserts the first response
//! arrives within 30 seconds. With the echo provider the budget is orders
//! of magnitude under the limit; the assertion exists to catch accidental
//! network or key-blocking regressions before they reach users.

use std::io::Write;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const FIRST_RESPONSE_BUDGET: Duration = Duration::from_secs(30);

#[test]
fn hello_first_response_within_30_seconds() {
    let start = Instant::now();

    let mut child = Command::new(env!("CARGO_BIN_EXE_openpaw"))
        .arg("hello")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn openpaw hello");

    {
        let stdin = child.stdin.as_mut().expect("piped stdin");
        stdin
            .write_all(b"hello openpaw\nexit\n")
            .expect("write to stdin");
    }

    let output = child.wait_with_output().expect("wait for hello");
    let elapsed = start.elapsed();

    assert!(
        output.status.success(),
        "hello exited with {:?}, stderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Echo: hello openpaw"),
        "expected echo reply, got: {stdout}"
    );

    assert!(
        elapsed < FIRST_RESPONSE_BUDGET,
        "first response took {elapsed:?}, budget {FIRST_RESPONSE_BUDGET:?}"
    );
}

#[test]
fn hello_processes_multiple_turns_in_order() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_openpaw"))
        .arg("hello")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn openpaw hello");

    {
        let stdin = child.stdin.as_mut().expect("piped stdin");
        stdin
            .write_all(b"one\ntwo\nthree\nquit\n")
            .expect("write to stdin");
    }
    let output = child.wait_with_output().expect("wait");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let idx_one = stdout.find("Echo: one").expect("reply to 'one'");
    let idx_two = stdout.find("Echo: two").expect("reply to 'two'");
    let idx_three = stdout.find("Echo: three").expect("reply to 'three'");
    assert!(
        idx_one < idx_two && idx_two < idx_three,
        "out of order: {stdout}"
    );
}

#[test]
fn no_args_prints_help_and_exits_zero() {
    let output = Command::new(env!("CARGO_BIN_EXE_openpaw"))
        .output()
        .expect("run openpaw without args");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("hello"),
        "help must mention hello: {stdout}"
    );
}

#[test]
fn version_flag_works() {
    let output = Command::new(env!("CARGO_BIN_EXE_openpaw"))
        .arg("--version")
        .output()
        .expect("run openpaw --version");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.starts_with("openpaw"), "got: {stdout}");
}
