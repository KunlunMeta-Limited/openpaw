//! `xtask snapshot [--check]` — public trait signature snapshot of
//! openpaw-core (Q7). Signatures drift → the check fails.

use anyhow::{bail, Context, Result};
use quote::ToTokens;
use std::path::Path;

pub fn run(root: &Path, check: bool) -> Result<()> {
    let core_src = root.join("crates/openpaw-core/src");
    let mut traits: Vec<TraitSig> = Vec::new();
    let mut types: Vec<String> = Vec::new();

    for entry in walkdir::WalkDir::new(&core_src)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter(|e| e.path().extension().is_some_and(|x| x == "rs"))
    {
        let src = std::fs::read_to_string(entry.path())?;
        let file =
            syn::parse_file(&src).with_context(|| format!("parse {}", entry.path().display()))?;
        collect(&file.items, &mut traits, &mut types);
    }

    traits.sort_by(|a, b| a.name.cmp(&b.name));
    for t in &mut traits {
        t.methods.sort();
    }
    types.sort();
    types.dedup();

    let snapshot = serde_json::json!({
        "version": 1,
        "crate": "openpaw-core",
        "traits": traits.iter().map(|t| serde_json::json!({
            "name": t.name,
            "methods": t.methods,
        })).collect::<Vec<_>>(),
        "types": types,
    });
    let rendered = serde_json::to_string_pretty(&snapshot)?;

    let snapshot_path = root.join("openpaw-core.api.json");
    if check {
        let existing = std::fs::read_to_string(&snapshot_path).unwrap_or_default();
        if existing != rendered {
            bail!(
                "trait snapshot drift detected — re-run `cargo xtask snapshot` and review the diff"
            );
        }
        println!(
            "snapshot: {} trait(s), {} type(s) — no drift",
            traits.len(),
            types.len()
        );
    } else {
        std::fs::write(&snapshot_path, &rendered)?;
        println!(
            "snapshot: wrote {} ({} trait(s), {} type(s))",
            snapshot_path.display(),
            traits.len(),
            types.len()
        );
    }
    Ok(())
}

struct TraitSig {
    name: String,
    methods: Vec<String>,
}

fn collect(items: &[syn::Item], traits: &mut Vec<TraitSig>, types: &mut Vec<String>) {
    for item in items {
        match item {
            syn::Item::Trait(t) => {
                let name = t.ident.to_string();
                let mut methods = Vec::new();
                for ti in &t.items {
                    if let syn::TraitItem::Fn(f) = ti {
                        methods.push(f.sig.to_token_stream().to_string());
                    }
                }
                traits.push(TraitSig { name, methods });
            }
            syn::Item::Struct(s) => types.push(format!("struct {}", s.ident)),
            syn::Item::Enum(e) => types.push(format!("enum {}", e.ident)),
            syn::Item::Type(ty) => types.push(format!("type {}", ty.ident)),
            syn::Item::Mod(m) => {
                if let Some((_, inner)) = &m.content {
                    collect(inner, traits, types);
                }
            }
            _ => {}
        }
    }
}
