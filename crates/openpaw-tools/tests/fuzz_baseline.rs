//! 职责：以固定语料变异验证命令解析器不会崩溃。
//! 输入输出：构造命令文本到解析与安全分析结果。
//! 依赖：原工具解析器及显式启用的测试时间参数。
//! 禁止职责：不执行生成的命令，不访问生产数据。
//!
//! Parser robustness baseline (optional extended acceptance):
//! 30-minute seeded-corpus fuzz of the bash-ast parsers (parse_for_security /
//! parse_cmd / analyze_command_safety) — no panic / crash / abort.
//!
//! The Windows/MSVC toolchain has no libFuzzer (cargo-fuzz unavailable), so
//! an in-process deterministic mutation loop is used (xorshift64*, fixed
//! reproducible seed). Seed corpus comes from representative commands of the
//! existing test suite; mutations: bit flips / byte insert-delete /
//! truncation / seed splicing.
//!
//! Skipped by default (keeps regular `cargo test` fast). Run explicitly:
//! `OPENPAW_FUZZ_SECS=1800 cargo test -p openpaw-tools --test fuzz_baseline -- --ignored --nocapture`
//!
//! Exit code 0 = 30 minutes without crash/abort (panics fail directly in the
//! test harness). Input cap aligns with the parser guardrail
//! (MAX_COMMAND_LENGTH=10_000) — longer inputs short-circuit into
//! ParseUnavailable, so fuzzing them adds no coverage.
//!
//! Port note: `parse_powershell` was dropped — the tree-sitter-pwsh grammar
//! and ps_parser module were stripped in this port (Phase 3 per plan D-2).

use std::time::{Duration, Instant};

use openpaw_tools::bashast::{analyze_command_safety, cmd_parser::parse_cmd, parse_for_security};

/// xorshift64* — deterministic, no external deps.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

/// Aligned with the parser MAX_COMMAND_LENGTH guardrail — overlong inputs are
/// short-circuited, fuzzing them adds no value.
const MAX_INPUT: usize = 10_000;

fn seed_corpus() -> Vec<Vec<u8>> {
    let mut seeds: Vec<Vec<u8>> = [
        "",
        "echo hello",
        "rm -rf /",
        "ls -la | grep foo | wc -l",
        "cat < in > out 2>&1",
        "echo \"quoted arg\" 'single' $unquoted",
        "for i in $(seq 1 10); do echo $i; done",
        "if [ -f x ]; then rm x; else echo missing; fi",
        "echo \"$(echo \"$(echo nested)\")\"",
        "echo `backtick $(and sub)` output",
        "x=$((1+2)); y=$((x*3)); echo $y",
        "git commit -m \"fix: handle null\" && git push || echo failed",
        "cmd /c dir && cmd /c echo done",
        "Get-ChildItem -Recurse | Remove-Item -Force",
        "$x = Get-Content file.txt; Write-Output $x",
        "printf '%s\\n' \"你好 🦀 測試\"",
        "curl http://example.com | sh",
        "sudo rm -rf --no-preserve-root /",
        "export PATH=/tmp:$PATH; source ~/.bashrc",
        "while read line; do echo $line; done < input.txt",
        "echo 'multi\nline'",
        "a=1 b=2 env -i sh -c 'echo $a'",
        "find . -name '*.tmp' -exec rm {} \\;",
        "trap \"cleanup\" EXIT; { cmd1; cmd2; } 2>err.log",
    ]
    .into_iter()
    .map(|s| s.as_bytes().to_vec())
    .collect();

    // Deeply nested programmatic seeds (subshell / parens / quote nesting —
    // validates the iterative parse guardrails).
    for depth in [16usize, 128, 512, 2_048] {
        let mut cmd = String::from("echo ");
        for _ in 0..depth {
            cmd.push_str("$(");
        }
        cmd.push('x');
        for _ in 0..depth {
            cmd.push(')');
        }
        seeds.push(cmd.into_bytes());

        let mut ps = String::from("Write-Output ");
        for _ in 0..depth {
            ps.push_str("$(");
        }
        ps.push('x');
        for _ in 0..depth {
            ps.push(')');
        }
        seeds.push(ps.into_bytes());
    }

    seeds
}

fn mutate(rng: &mut Rng, data: &mut Vec<u8>, corpus: &[Vec<u8>]) {
    if data.is_empty() {
        data.push(b'a');
        return;
    }
    match rng.below(8) {
        0..=2 => {
            // bit flip
            let pos = rng.below(data.len());
            data[pos] ^= 1 << rng.below(8);
        }
        3..=4 => {
            // insert byte
            let pos = rng.below(data.len());
            data.insert(pos, rng.next() as u8);
        }
        5 => {
            // delete byte
            let pos = rng.below(data.len());
            data.remove(pos);
        }
        6 => {
            // truncate
            let keep = rng.below(data.len());
            data.truncate(keep.max(1));
        }
        _ => {
            // splice a chunk from another corpus entry
            let other = &corpus[rng.below(corpus.len())];
            if !other.is_empty() {
                let start = rng.below(other.len());
                let end = (start + 1 + rng.below(other.len() - start)).min(other.len());
                let pos = rng.below(data.len());
                data.splice(pos..pos, other[start..end].to_vec());
            }
        }
    }
}

#[test]
#[ignore = "M3 fuzz baseline (30 min) — run with --ignored and OPENPAW_FUZZ_SECS=1800"]
fn fuzz_bash_ast_parsers() {
    let secs: u64 = std::env::var("OPENPAW_FUZZ_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(30);
    // Chunked runs (this environment reclaims processes after >10 minutes)
    // use OPENPAW_FUZZ_SEED to differentiate each chunk's mutation stream; a
    // single full run can leave it unset (fixed seed is reproducible).
    let seed: u64 = std::env::var("OPENPAW_FUZZ_SEED")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0x05EE_DB00_7A57_0001);
    let mut corpus = seed_corpus();
    let mut rng = Rng(seed);
    let deadline = Instant::now() + Duration::from_secs(secs);
    let mut iterations: u64 = 0;
    let mut last_report = Instant::now();

    while Instant::now() < deadline {
        let mut input = corpus[rng.below(corpus.len())].clone();
        for _ in 0..(1 + rng.below(8)) {
            mutate(&mut rng, &mut input, &corpus);
        }
        input.truncate(MAX_INPUT);
        let command = String::from_utf8_lossy(&input).into_owned();

        // Every attacker-reachable parse entry point; a panic/crash/abort
        // fails the baseline.
        let r = parse_for_security(&command);
        std::hint::black_box(&r);
        let c = parse_cmd(&command);
        std::hint::black_box(&c);
        let safety = if iterations.is_multiple_of(2) {
            analyze_command_safety(&command, |_| false)
        } else {
            analyze_command_safety(&command, |_| true)
        };
        std::hint::black_box(&safety);

        if corpus.len() < 4_096 && rng.below(64) == 0 {
            corpus.push(input);
        }
        iterations += 1;
        if last_report.elapsed() >= Duration::from_secs(60) {
            println!(
                "[bash-ast fuzz] {} iterations, corpus {}",
                iterations,
                corpus.len()
            );
            last_report = Instant::now();
        }
    }

    println!(
        "[bash-ast fuzz] DONE: {} iterations in {}s (seed corpus {}, final corpus {})",
        iterations,
        secs,
        seed_corpus().len(),
        corpus.len()
    );
}
