//! `xtask stability` — Stability doc-tag coverage gate (1.0 freeze set).
//!
//! Every public item of openpaw-core must carry a `Stability: stable` or
//! `Stability: unstable` line in its doc comment (design plan section 7.8).
//! Untagged public items are listed and the gate exits 1.
//!
//! Scope, mirroring the crate's public surface:
//! - module docs of every `pub mod` declared in `lib.rs` (inner `//!` docs
//!   of the target file),
//! - top-level `pub` structs / enums / traits / fns / type aliases /
//!   consts / statics / inline modules in every source file,
//! - `pub use` re-exports are skipped — the tag lives at the definition
//!   site, and impl-block methods are covered by their type's tag.

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use syn::spanned::Spanned;

const CORE_SRC: &str = "crates/openpaw-core/src";
const TAG: &str = "Stability:";

/// One public item missing its `Stability:` doc tag.
pub struct Missing {
    pub file: String,
    pub line: usize,
    pub item: String,
}

/// Running tally of tagged items, split by level.
#[derive(Default)]
pub struct Tally {
    pub checked: usize,
    pub stable: usize,
    pub unstable: usize,
}

pub fn run(root: &Path) -> Result<()> {
    let src = root.join(CORE_SRC);
    let files = rust_files(&src);
    if files.is_empty() {
        bail!("no rust sources under {}", src.display());
    }

    let mut missing: Vec<Missing> = Vec::new();
    let mut tally = Tally::default();

    for path in &files {
        let rel = rel_path(root, path);
        let code =
            std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
        let file = syn::parse_file(&code).with_context(|| format!("parse {}", path.display()))?;
        check_file(&file, path, &rel, &mut missing, &mut tally);
    }

    println!(
        "stability: checked {} public item(s) — stable {}, unstable {}",
        tally.checked, tally.stable, tally.unstable
    );

    if missing.is_empty() {
        println!("stability: all public items carry a Stability tag");
        Ok(())
    } else {
        for m in &missing {
            eprintln!("MISSING: {}:{}: {}", m.file, m.line, m.item);
        }
        eprintln!(
            "stability: {} public item(s) without a `{}` doc tag",
            missing.len(),
            TAG
        );
        std::process::exit(1);
    }
}

fn rust_files(src: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = walkdir::WalkDir::new(src)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .map(|e| e.path().to_path_buf())
        .filter(|p| p.extension().is_some_and(|x| x == "rs"))
        .collect();
    files.sort();
    files
}

fn rel_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

/// Check one parsed source file: inner module docs plus every top-level
/// public item.
fn check_file(
    file: &syn::File,
    path: &Path,
    rel: &str,
    missing: &mut Vec<Missing>,
    tally: &mut Tally,
) {
    // Module-level docs (`//!` at the top of the file).
    tally.checked += 1;
    if has_tag(&file.attrs) {
        count_level(&file.attrs, tally);
    } else if path.file_name().is_some_and(|n| n == "lib.rs") {
        // The crate root documents the convention itself; its module doc
        // doubles as the tagging policy text and carries no tag.
        tally.checked -= 1;
    } else {
        missing.push(Missing {
            file: rel.to_string(),
            line: 1,
            item: "module doc".to_string(),
        });
    }
    check_items(&file.items, rel, missing, tally);
}

/// Recursively check items. External `pub mod name;` declarations are not
/// followed here — their docs live in `name.rs`, which the file walk checks
/// on its own pass.
fn check_items(items: &[syn::Item], rel: &str, missing: &mut Vec<Missing>, tally: &mut Tally) {
    for item in items {
        match item {
            syn::Item::Struct(s) if is_pub(&s.vis) => check_item_attrs(
                &s.attrs,
                &s.ident.to_string(),
                s.span(),
                rel,
                missing,
                tally,
            ),
            syn::Item::Enum(e) if is_pub(&e.vis) => check_item_attrs(
                &e.attrs,
                &e.ident.to_string(),
                e.span(),
                rel,
                missing,
                tally,
            ),
            syn::Item::Trait(t) if is_pub(&t.vis) => check_item_attrs(
                &t.attrs,
                &t.ident.to_string(),
                t.span(),
                rel,
                missing,
                tally,
            ),
            syn::Item::Fn(f) if is_pub(&f.vis) => {
                check_item_attrs(&f.attrs, &sig_name(&f.sig), f.span(), rel, missing, tally)
            }
            syn::Item::Type(t) if is_pub(&t.vis) => check_item_attrs(
                &t.attrs,
                &t.ident.to_string(),
                t.span(),
                rel,
                missing,
                tally,
            ),
            syn::Item::Const(c) if is_pub(&c.vis) => check_item_attrs(
                &c.attrs,
                &c.ident.to_string(),
                c.span(),
                rel,
                missing,
                tally,
            ),
            syn::Item::Static(s) if is_pub(&s.vis) => check_item_attrs(
                &s.attrs,
                &s.ident.to_string(),
                s.span(),
                rel,
                missing,
                tally,
            ),
            syn::Item::Mod(m) if is_pub(&m.vis) => match &m.content {
                Some((_, inner)) => {
                    check_item_attrs(
                        &m.attrs,
                        &format!("mod {}", m.ident),
                        m.span(),
                        rel,
                        missing,
                        tally,
                    );
                    let nested_rel = rel.to_string();
                    check_items(inner, &nested_rel, missing, tally);
                }
                None => {
                    // External `pub mod name;` — its docs are the inner
                    // docs of `name.rs` (checked when that file is walked).
                    tally.checked += 0;
                }
            },
            // Re-exports: tagged at the definition site.
            syn::Item::Use(_) => {}
            _ => {}
        }
    }
}

fn check_item_attrs(
    attrs: &[syn::Attribute],
    item: &str,
    span: proc_macro2::Span,
    rel: &str,
    missing: &mut Vec<Missing>,
    tally: &mut Tally,
) {
    tally.checked += 1;
    if has_tag(attrs) {
        count_level(attrs, tally);
    } else {
        missing.push(Missing {
            file: rel.to_string(),
            line: span.start().line,
            item: item.to_string(),
        });
    }
}

fn is_pub(vis: &syn::Visibility) -> bool {
    matches!(vis, syn::Visibility::Public(_))
}

fn sig_name(sig: &syn::Signature) -> String {
    sig.ident.to_string()
}

/// True when any doc attribute carries the `Stability:` marker.
pub fn has_tag(attrs: &[syn::Attribute]) -> bool {
    doc_lines(attrs).iter().any(|l| l.contains(TAG))
}

/// Classify a tagged item as stable (default) or unstable.
pub fn count_level(attrs: &[syn::Attribute], tally: &mut Tally) {
    let unstable = doc_lines(attrs).iter().any(|l| {
        let t = l.trim();
        t.starts_with(TAG) && t[TAG.len()..].trim_start().starts_with("unstable")
    });
    if unstable {
        tally.unstable += 1;
    } else {
        tally.stable += 1;
    }
}

/// Collect the text of `#[doc = "..."]` attributes (doc comments).
pub fn doc_lines(attrs: &[syn::Attribute]) -> Vec<String> {
    let mut out = Vec::new();
    for attr in attrs {
        if !attr.path().is_ident("doc") {
            continue;
        }
        if let syn::Meta::NameValue(nv) = &attr.meta {
            if let syn::Expr::Lit(lit) = &nv.value {
                if let syn::Lit::Str(s) = &lit.lit {
                    out.push(s.value());
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(code: &str) -> syn::File {
        syn::parse_file(code).expect("test source must parse")
    }

    /// syn::Item is an enum without a shared `attrs` accessor — fan out.
    fn item_attrs(item: &syn::Item) -> &[syn::Attribute] {
        match item {
            syn::Item::Const(i) => &i.attrs,
            syn::Item::Enum(i) => &i.attrs,
            syn::Item::Fn(i) => &i.attrs,
            syn::Item::Macro(i) => &i.attrs,
            syn::Item::Mod(i) => &i.attrs,
            syn::Item::Static(i) => &i.attrs,
            syn::Item::Struct(i) => &i.attrs,
            syn::Item::Trait(i) => &i.attrs,
            syn::Item::TraitAlias(i) => &i.attrs,
            syn::Item::Type(i) => &i.attrs,
            syn::Item::Union(i) => &i.attrs,
            syn::Item::Use(i) => &i.attrs,
            _ => &[],
        }
    }

    #[test]
    fn doc_lines_extracts_doc_comments() {
        let file = parse("/// hello\n/// Stability: stable.\npub struct A;");
        let lines = doc_lines(item_attrs(&file.items[0]));
        assert_eq!(lines.len(), 2);
        assert!(lines[1].contains("Stability: stable."));
    }

    #[test]
    fn has_tag_matches_marker_and_ignores_plain_docs() {
        let tagged = parse("/// Stability: unstable — evolving.\npub struct A;");
        assert!(has_tag(item_attrs(&tagged.items[0])));

        let plain = parse("/// just a comment\npub struct A;");
        assert!(!has_tag(item_attrs(&plain.items[0])));

        let none = parse("pub struct A;");
        assert!(!has_tag(item_attrs(&none.items[0])));
    }

    #[test]
    fn module_inner_doc_counts_as_tag() {
        let file = parse("//! Stability: stable.\npub struct A;");
        assert!(has_tag(&file.attrs));

        let bare = parse("//! crate docs\npub struct A;");
        assert!(!has_tag(&bare.attrs));
    }

    #[test]
    fn unstable_level_detected_only_for_unstable_word() {
        let mut tally = Tally::default();
        count_level(
            item_attrs(&parse("/// Stability: stable.\npub struct A;").items[0]),
            &mut tally,
        );
        assert_eq!((tally.stable, tally.unstable), (1, 0));

        let mut tally = Tally::default();
        count_level(
            item_attrs(&parse("/// Stability: unstable — evolving.\npub struct A;").items[0]),
            &mut tally,
        );
        assert_eq!((tally.stable, tally.unstable), (0, 1));
    }

    #[test]
    fn check_items_reports_only_untagged_public_items() {
        let code = r#"
            /// Stability: stable.
            pub struct Tagged;

            pub struct Untagged;

            pub(crate) fn hidden() {}

            fn private() {}

            pub use crate::foo::Bar;

            impl Tagged {
                /// Method tags are covered by the type's tag.
                pub fn method(&self) {}
            }

            /// Stability: unstable — evolving.
            pub mod inline_pub {
                pub struct Nested;
            }
        "#;
        let file = parse(code);
        let mut missing = Vec::new();
        let mut tally = Tally::default();
        check_items(&file.items, "test.rs", &mut missing, &mut tally);
        // Tagged struct + inline pub mod counted; untagged struct listed.
        // (`pub use` skipped, non-public items skipped, impl methods
        // skipped.) The nested struct inside `inline_pub` is untagged and
        // also listed.
        let names: Vec<&str> = missing.iter().map(|m| m.item.as_str()).collect();
        assert!(names.contains(&"Untagged"), "missing list: {names:?}");
        assert!(names.contains(&"Nested"), "missing list: {names:?}");
        assert_eq!(names.len(), 2);
        assert!(tally.checked >= 3);
    }

    #[test]
    fn check_items_lists_line_numbers() {
        let code = r#"
            pub struct Untagged;
        "#;
        let file = parse(code);
        let mut missing = Vec::new();
        let mut tally = Tally::default();
        check_items(&file.items, "test.rs", &mut missing, &mut tally);
        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0].item, "Untagged");
        assert!(missing[0].line >= 1);
    }
}
