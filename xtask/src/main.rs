//! 职责：提供workspace质量命令分派。
//! 输入输出：读取CLI参数，输出检查结果和非零失败码。
//! 依赖：所属xtask模块及本机工具链。
//! 禁止职责：不运行Host业务、不生成批准或自动修改待审代码。
//! xtask — workspace engineering scripts.
//!
//! Commands:
//! - `lines`            informational per-crate business-line statistics (Q1)
//! - `scrub --check`    porting-hygiene banned-word scan (Q10)
//! - `port`             semi-automatic crate porting with renames + precheck
//! - `snapshot [--check]` public trait signature snapshot of openpaw-core (Q7)
//! - `golden`           golden bidirectional read/write runner (Q6)
//! - `telemetry`        zero-telemetry gate: deps + source symbols (Q3)
//! - `smoke-offline`    offline smoke: hello → doctor → evolve (Q4)
//! - `stability`        Stability doc-tag coverage of openpaw-core (1.0 freeze)

mod comments;
mod lines;
mod port;
mod scrub;
mod smoke;
mod snapshot;
mod stability;
mod telemetry;

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};

pub fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives in the workspace root")
        .to_path_buf()
}

fn usage() -> &'static str {
    "usage: cargo xtask <comments --base <ref> <--head <ref>|--worktree> [--all --report-only]|lines|scrub --check|port --crate <name> --from <dir> [--files <glob>] | snapshot [--check] | golden | telemetry | smoke-offline | stability>"
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("comments") => comments::run(&workspace_root(), &args[1..]),
        Some("lines") => lines::run(&workspace_root()),
        Some("scrub") => {
            let check = args.iter().any(|a| a == "--check");
            scrub::run(&workspace_root(), check)
        }
        Some("port") => port::run(&workspace_root(), &args[1..]),
        Some("snapshot") => {
            let check = args.iter().any(|a| a == "--check");
            snapshot::run(&workspace_root(), check)
        }
        Some("golden") => golden(&workspace_root()),
        Some("telemetry") => telemetry::run(&workspace_root()), // openpaw-telemetry-allow
        Some("smoke-offline") => smoke::run(&workspace_root()),
        Some("stability") => stability::run(&workspace_root()),
        Some(other) => {
            eprintln!("unknown command: {other}\n{}", usage());
            std::process::exit(2);
        }
        None => {
            println!("{}", usage());
            Ok(())
        }
    }
}

/// Golden bidirectional read/write runner (Q6).
fn golden(root: &Path) -> Result<()> {
    let dir = root.join("tests/golden/frontmatter");
    let mut entries: Vec<PathBuf> = std::fs::read_dir(&dir)
        .with_context(|| format!("missing {}", dir.display()))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "md"))
        .collect();
    entries.sort();
    if entries.len() < 3 {
        bail!("expected >=3 golden frontmatter samples");
    }

    let tmp = tempfile_dir()?;
    for src in &entries {
        let name = src.file_name().unwrap().to_string_lossy().to_string();
        let doc: openpaw_store::FrontmatterDoc<serde_yml::Mapping> =
            openpaw_store::read_doc(src).with_context(|| format!("parse {name}"))?;
        let out = tmp.join(&name);
        openpaw_store::write_doc_atomic(&out, &doc).with_context(|| format!("write {name}"))?;
        let reparsed: openpaw_store::FrontmatterDoc<serde_yml::Mapping> =
            openpaw_store::read_doc(&out).with_context(|| format!("reparse {name}"))?;
        if doc.frontmatter != reparsed.frontmatter || doc.body != reparsed.body {
            bail!("{name}: roundtrip mismatch");
        }
        let first = std::fs::read_to_string(&out)?;
        openpaw_store::write_doc_atomic(&out, &reparsed)?;
        let second = std::fs::read_to_string(&out)?;
        if first != second {
            bail!("{name}: writer output not byte-stable");
        }
        println!("golden ok: {name}");
    }

    let session = root.join("tests/golden/sessions/session-001.jsonl");
    let outcome = openpaw_store::SessionReader::read(&session)
        .with_context(|| format!("read {}", session.display()))?;
    println!(
        "golden ok: session-001.jsonl ({} events, torn={})",
        outcome.events.len(),
        outcome.torn_tail_lines
    );

    // D8 (ReflectionOutput) golden samples: schema-shape check. The
    // knowledge-format 1.0 spec (docs/specs/knowledge-format-1.0.md) pins
    // these keys; a sample that drifts from the schema fails here.
    let d8_dir = root.join("tests/golden/knowledge-format");
    let mut d8_entries: Vec<PathBuf> = std::fs::read_dir(&d8_dir)
        .with_context(|| format!("missing {}", d8_dir.display()))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect();
    d8_entries.sort();
    if d8_entries.is_empty() {
        bail!(
            "expected >=1 golden ReflectionOutput sample under {}",
            d8_dir.display()
        );
    }
    for src in &d8_entries {
        let name = src.file_name().unwrap().to_string_lossy().to_string();
        let raw = std::fs::read_to_string(src)?;
        let v: serde_json::Value =
            serde_json::from_str(&raw).with_context(|| format!("parse {name}"))?;
        check_reflection_output(&v).with_context(|| format!("schema {name}"))?;
        println!("golden ok: {name} (ReflectionOutput schema)");
    }

    println!("golden: all green");
    Ok(())
}

/// Minimal schema assertions for a D8 `ReflectionOutput` golden sample.
/// Required keys must exist with the right container type; candidate
/// entries must carry their mandatory fields (defaults cover the rest).
fn check_reflection_output(v: &serde_json::Value) -> Result<()> {
    let obj = v
        .as_object()
        .context("ReflectionOutput must be a JSON object")?;
    if obj
        .get("summary")
        .ok_or_else(|| anyhow::anyhow!("missing key `summary`"))?
        .is_null()
    {
        bail!("`summary` must be a string");
    }
    const ARRAY_KEYS: [&str; 4] = [
        "skill_candidates",
        "memory_candidates",
        "conflicts",
        "task_candidates",
    ];
    for key in ARRAY_KEYS {
        let val = obj
            .get(key)
            .ok_or_else(|| anyhow::anyhow!("missing key `{key}`"))?;
        if !val.is_array() {
            bail!("`{key}` must be an array");
        }
    }
    for mc in obj["memory_candidates"].as_array().unwrap() {
        for key in ["fact", "scope", "confidence", "rationale"] {
            if mc.get(key).is_none() {
                bail!("memory candidate missing `{key}`");
            }
        }
    }
    for sc in obj["skill_candidates"].as_array().unwrap() {
        for key in ["name", "description", "body", "rationale", "confidence"] {
            if sc.get(key).is_none() {
                bail!("skill candidate missing `{key}`");
            }
        }
    }
    Ok(())
}

fn tempfile_dir() -> Result<PathBuf> {
    let dir = std::env::temp_dir().join(format!("openpaw-golden-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}
