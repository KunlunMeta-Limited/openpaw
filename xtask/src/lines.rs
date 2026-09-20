//! `xtask lines` — per-crate business-line counting vs budget caps (Q1).
//!
//! Counts business lines with a syn AST: total file lines minus lines
//! belonging to `#[cfg(test)]` items (recursively). `tests/` directories
//! count toward the shared "tests and helpers" budget. Regex or
//! brace-counting is deliberately not used (string literals break them).
use anyhow::Result;
use quote::ToTokens;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use syn::spanned::Spanned;
/// Per-crate business-line caps (engineering plan section 1.3, v1.2).
const CRATE_CAPS: &[(&str, usize)] = &[
    ("openpaw-core", 5_000),
    ("openpaw-engine", 8_200),
    ("openpaw-llm", 5_200),
    ("openpaw-tools", 14_000),
    ("openpaw-mcp", 2_400),
    ("openpaw-store", 2_000),
    ("openpaw-memory", 7_400),
    ("openpaw-plugins", 1_300),
    ("openpaw-cli", 4_500),
    ("openpaw-reflect-lite", 3_000),
    ("openpaw-engine-host", 5_000),
    ("openpaw-goal-ledger", 2_000),
    // Local sandbox (plan section 3.4 budget lives in the tools domain; the
    // crate is standalone for the crate-level unsafe allowance, D-9).
    ("openpaw-sandbox", 4_500),
];

/// Shared budget for xtask + all test directories ("tests and helpers").
const AUX_CAP: usize = 10_000;
/// B 线 engine-host 独立验证预算。
///
/// engine-host 已作为独立 feature 扩展并拥有单独业务代码上限；其 HTTP、并发、持久化和重启
/// 契约测试不能挤占原工程的测试余量，也不能通过删除可靠性测试规避门禁，因此单列 1,500 行。
const ENGINE_HOST_TEST_CAP: usize = 1_500;
pub fn run(root: &Path) -> Result<()> {
    let crates_dir = root.join("crates");
    let mut total_business = 0usize;
    let mut violations = Vec::new();
    let mut rows: Vec<(String, usize, usize)> = Vec::new(); // name, business, cap
    for (name, cap) in CRATE_CAPS {
        let src = crates_dir.join(name).join("src");
        let business = count_business_lines(&src)?;
        total_business += business;
        rows.push((name.to_string(), business, *cap));
        if business > *cap {
            violations.push(format!(
                "{name}: {business} business lines exceed cap {cap}"
            ));
        }
    }

    // Aux budget: xtask src + every crate's tests/ directory.
    let mut aux = count_business_lines(&root.join("xtask/src"))?;
    let mut engine_host_tests = 0usize;
    for entry in std::fs::read_dir(&crates_dir)? {
        let entry = entry?;
        let tests = entry.path().join("tests");
        if tests.is_dir() {
            let test_lines = raw_line_count_dir(&tests)?;
            if entry.file_name() == "openpaw-engine-host" {
                engine_host_tests = test_lines;
            } else {
                aux += test_lines;
            }
        }
    }
    if aux > AUX_CAP {
        violations.push(format!("aux (tests+xtask): {aux} exceeds cap {AUX_CAP}"));
    }
    if engine_host_tests > ENGINE_HOST_TEST_CAP {
        violations.push(format!(
            "engine-host tests: {engine_host_tests} exceeds cap {ENGINE_HOST_TEST_CAP}"
        ));
    }
    println!(
        "{:<24}{:>10}{:>10}{:>10}",
        "crate", "business", "cap", "headroom"
    );
    for (name, business, cap) in &rows {
        let headroom = cap.saturating_sub(*business);
        let flag = if business > cap { " !OVER" } else { "" };
        println!("{name:<24}{business:>10}{cap:>10}{headroom:>10}{flag}");
    }
    println!("{:<24}{:>10}{:>10}", "core nine total budget", "50000", "—");
    println!("{:<24}{:>10}{:>10}", "aux (tests+xtask)", aux, AUX_CAP);
    println!(
        "{:<24}{:>10}{:>10}",
        "engine-host tests", engine_host_tests, ENGINE_HOST_TEST_CAP
    );
    println!("{:<24}{:>10}", "TOTAL business", total_business);

    if violations.is_empty() {
        println!("lines: all budgets respected");
        Ok(())
    } else {
        for v in &violations {
            eprintln!("VIOLATION: {v}");
        }
        std::process::exit(1);
    }
}

/// Walk `src/`, parse each .rs file, subtract cfg(test) item lines.
fn count_business_lines(src: &Path) -> Result<usize> {
    if !src.is_dir() {
        return Ok(0);
    }
    let mut total = 0usize;
    for path in rust_files(src) {
        total += count_business_lines_file(&path)?;
    }
    Ok(total)
}

fn rust_files(dir: &Path) -> Vec<PathBuf> {
    walkdir::WalkDir::new(dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .map(|e| e.path().to_path_buf())
        .filter(|p| p.extension().is_some_and(|e| e == "rs"))
        .collect()
}

fn raw_line_count_dir(dir: &Path) -> Result<usize> {
    let mut total = 0usize;
    for path in rust_files(dir) {
        total += std::fs::read_to_string(&path)?.lines().count();
    }
    Ok(total)
}

fn count_business_lines_file(path: &Path) -> Result<usize> {
    let src = std::fs::read_to_string(path)?;
    let total = src.lines().count();

    let parsed = match syn::parse_file(&src) {
        Ok(p) => p,
        Err(e) => {
            // A file that fails to parse blocks the gate — the build is
            // broken anyway; report clearly.
            anyhow::bail!("{}: syn parse failed: {e}", path.display());
        }
    };

    let mut skipped_ranges: Vec<(usize, usize)> = Vec::new();
    collect_cfg_test_ranges(&parsed.items, &mut skipped_ranges);

    let mut skipped_lines: Vec<usize> = Vec::new();
    for (start, end) in skipped_ranges {
        for line in start..=end {
            skipped_lines.push(line);
        }
    }
    let skipped: BTreeMap<usize, ()> = skipped_lines.into_iter().map(|l| (l, ())).collect();

    // Lines are 1-based in spans; subtract skipped from total.
    Ok(total - skipped.len())
}

/// Recursively find items gated by `#[cfg(test)]` and record their spans
/// (including the attribute lines themselves).
fn collect_cfg_test_ranges(items: &[syn::Item], out: &mut Vec<(usize, usize)>) {
    for item in items {
        let attrs = item_attrs(item);
        let is_cfg_test = attrs.iter().any(is_cfg_test_attr);
        if is_cfg_test {
            let span = item.span();
            let start = span.start().line;
            let end = span.end().line;
            // Include the attribute lines above the item start.
            let attr_start = attrs
                .iter()
                .map(|a| a.span().start().line)
                .min()
                .unwrap_or(start);
            out.push((attr_start.min(start), end));
            continue; // do not descend into cfg(test) modules
        }
        // Descend into inline modules.
        if let syn::Item::Mod(m) = item {
            if let Some((_, inner)) = &m.content {
                collect_cfg_test_ranges(inner, out);
            }
        }
    }
}

/// syn::Item is an enum without a shared `attrs` accessor — fan out.
fn item_attrs(item: &syn::Item) -> &[syn::Attribute] {
    match item {
        syn::Item::Const(i) => &i.attrs,
        syn::Item::Enum(i) => &i.attrs,
        syn::Item::ExternCrate(i) => &i.attrs,
        syn::Item::Fn(i) => &i.attrs,
        syn::Item::ForeignMod(i) => &i.attrs,
        syn::Item::Impl(i) => &i.attrs,
        syn::Item::Macro(i) => &i.attrs,
        syn::Item::Mod(i) => &i.attrs,
        syn::Item::Static(i) => &i.attrs,
        syn::Item::Struct(i) => &i.attrs,
        syn::Item::Trait(i) => &i.attrs,
        syn::Item::TraitAlias(i) => &i.attrs,
        syn::Item::Type(i) => &i.attrs,
        syn::Item::Union(i) => &i.attrs,
        syn::Item::Use(i) => &i.attrs,
        // syn::Item is #[non_exhaustive]; unknown future variants carry no
        // cfg(test) relevance for line counting.
        _ => &[],
    }
}

fn is_cfg_test_attr(attr: &syn::Attribute) -> bool {
    if !attr.path().is_ident("cfg") {
        return false;
    }
    // Token rendering inserts spaces (`not (test)`); normalize before matching.
    let tokens = attr.to_token_stream().to_string().replace(' ', "");
    // Matches #[cfg(test)] and #[cfg(any(test, ...))], not #[cfg(not(test))].
    let not_test = tokens.contains("not(test");
    tokens.contains("test") && !not_test
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cfg_test_detection() {
        let src = r#"
            #[cfg(test)]
            mod tests { fn x() {} }

            #[cfg(not(test))]
            pub fn live() {}

            pub fn plain() {}
        "#;
        let file = syn::parse_file(src).unwrap();
        let mut ranges = Vec::new();
        collect_cfg_test_ranges(&file.items, &mut ranges);
        // Only the cfg(test) mod is skipped.
        assert_eq!(ranges.len(), 1);
    }
}
