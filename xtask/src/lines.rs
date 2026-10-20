//! `xtask lines` — informational per-crate business-line statistics.
//!
//! T0 removes numeric size gates. Keep the existing AST-based counter so review
//! can see growth without encouraging duplicate modules or moving compatibility
//! code into core crates to satisfy a cap. Parse/read errors still fail.

use anyhow::Result;
use quote::ToTokens;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use syn::spanned::Spanned;

pub fn run(root: &Path) -> Result<()> {
    let crates_dir = root.join("crates");
    let mut crates: Vec<PathBuf> = std::fs::read_dir(&crates_dir)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<std::io::Result<_>>()?;
    crates.retain(|path| path.join("Cargo.toml").is_file());
    crates.sort();

    let mut total_business = 0usize;
    let mut aux = count_business_lines(&root.join("xtask/src"))?;
    println!("{:<28}{:>12}", "crate", "business");
    for path in crates {
        let name = path
            .file_name()
            .expect("crate path has a name")
            .to_string_lossy();
        let business = count_business_lines(&path.join("src"))?;
        total_business += business;
        aux += raw_line_count_dir(&path.join("tests"))?;
        println!("{name:<28}{business:>12}");
    }
    println!("{:<28}{:>12}", "tests + xtask", aux);
    println!("{:<28}{:>12}", "TOTAL business", total_business);
    println!("lines: informational only; no file or crate size limits");
    Ok(())
}

fn count_business_lines(src: &Path) -> Result<usize> {
    if !src.is_dir() {
        return Ok(0);
    }
    let mut total = 0usize;
    for path in rust_files(src)? {
        total += count_business_lines_file(&path)?;
    }
    Ok(total)
}

fn rust_files(dir: &Path) -> Result<Vec<PathBuf>> {
    if !dir.try_exists()? {
        return Ok(Vec::new());
    }
    let mut paths = Vec::new();
    for entry in walkdir::WalkDir::new(dir) {
        let entry = entry?;
        if entry.file_type().is_file() && entry.path().extension().is_some_and(|e| e == "rs") {
            paths.push(entry.into_path());
        }
    }
    Ok(paths)
}

fn raw_line_count_dir(dir: &Path) -> Result<usize> {
    let mut total = 0usize;
    for path in rust_files(dir)? {
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
