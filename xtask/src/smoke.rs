//! `xtask smoke-offline` — offline-equivalent smoke gate (Q4).
//!
//! Drives the real `openpaw` binary through hello → doctor → evolve
//! status using only offline surfaces: hello runs on the built-in echo
//! provider (no network I/O), doctor's network audit is a local static
//! scan, and evolve status only reads a throwaway temp home. The
//! genuinely network-blocked run is nightly CI's namespace-isolated job;
//! this command is the offline-equivalent check every platform can run.
//!
//! Steps (each prints PASS; any failure exits 1):
//! (a) `cargo build -p openpaw-cli` — cargo incremental makes this a
//!     no-op when the build is fresh (no manual mtime bookkeeping);
//! (b) `openpaw hello` with one piped line — echo reply on stdout, exit 0;
//! (c) `openpaw doctor --workspace <root>` — the network-audit report
//!     ("network call sites") must be present, exit 0;
//! (d) `openpaw evolve status --home <tempdir>` — the knowledge/policy
//!     surface loads from an empty home, exit 0.

use anyhow::{bail, ensure, Context, Result};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub fn run(root: &Path) -> Result<()> {
    // (a) Build the CLI binary.
    print_step(1, "build openpaw-cli");
    build_cli(root)?;
    println!("PASS");

    let exe = exe_path(root);
    ensure!(
        exe.is_file(),
        "openpaw binary not found at {} after the build",
        exe.display()
    );

    // One throwaway home keeps the whole run hermetic (never touches the
    // real ~/.openpaw).
    let home = temp_home()?;

    // (b) hello → echo conversation (no network).
    print_step(2, "hello (echo provider, offline)");
    run_hello(&exe)?;
    println!("PASS");

    // (c) doctor — network-audit report present, exit 0.
    print_step(3, "doctor (network-audit report)");
    let stdout = run_doctor(&exe, root, &home)?;
    ensure!(
        stdout.contains("network call sites"),
        "doctor output is missing the network-audit section:\n{stdout}"
    );
    println!("PASS (network call sites listed)");

    // (d) evolve status — knowledge/policy surface loads.
    print_step(4, "evolve status (temp home)");
    run_evolve_status(&exe, &home)?;
    println!("PASS");

    println!("smoke-offline: all green");
    let _ = std::fs::remove_dir_all(&home);
    Ok(())
}

fn print_step(n: usize, what: &str) {
    println!("[{n}/4] {what} ...");
}

/// (a) Build the CLI binary. Trusts cargo incremental: a fresh build is a
/// fast no-op, a stale one is rebuilt.
fn build_cli(root: &Path) -> Result<()> {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let status = Command::new(&cargo)
        .current_dir(root)
        .args(["build", "-q", "-p", "openpaw-cli"])
        .status()
        .with_context(|| format!("spawn {cargo} build -p openpaw-cli"))?;
    if !status.success() {
        bail!("cargo build -p openpaw-cli failed: {status}");
    }
    Ok(())
}

/// Debug-profile binary path (honors CARGO_TARGET_DIR like cargo does).
fn exe_path(root: &Path) -> PathBuf {
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("target"));
    target
        .join("debug")
        .join(format!("openpaw{}", std::env::consts::EXE_SUFFIX))
}

fn temp_home() -> Result<PathBuf> {
    let dir = std::env::temp_dir().join(format!("openpaw-smoke-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// (b) `openpaw hello` with one piped line: the echo provider replies
/// without any network I/O. Asserts exit 0 and the echo reply on stdout.
fn run_hello(exe: &Path) -> Result<()> {
    let mut child = Command::new(exe)
        .arg("hello")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("spawn openpaw hello")?;
    child
        .stdin
        .take()
        .expect("piped stdin")
        .write_all(b"hello openpaw\nexit\n")
        .context("write hello stdin")?;
    let out = child.wait_with_output().context("wait for hello")?;
    ensure!(
        out.status.success(),
        "hello exited with {:?}, stderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    ensure!(
        stdout.contains("Echo: hello openpaw"),
        "hello stdout is missing the echo reply:\n{stdout}"
    );
    Ok(())
}

/// (c) `openpaw doctor --workspace <root>` — the caller asserts the
/// network-audit section is present. The temp home keeps it hermetic.
fn run_doctor(exe: &Path, workspace: &Path, home: &Path) -> Result<String> {
    let out = Command::new(exe)
        .arg("doctor")
        .arg("--home")
        .arg(home)
        .arg("--workspace")
        .arg(workspace)
        .output()
        .context("spawn openpaw doctor")?;
    ensure!(
        out.status.success(),
        "doctor exited with {:?}, stderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

/// (d) `openpaw evolve status --home <home>` — the knowledge/policy
/// surface (memories, skills, goals) must load from an empty home.
fn run_evolve_status(exe: &Path, home: &Path) -> Result<()> {
    let out = Command::new(exe)
        .arg("evolve")
        .arg("status")
        .arg("--home")
        .arg(home)
        .output()
        .context("spawn openpaw evolve status")?;
    ensure!(
        out.status.success(),
        "evolve status exited with {:?}, stderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    Ok(())
}
