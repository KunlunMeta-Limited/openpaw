//! 职责：检查本次改动的Rust公共声明中文说明，历史债仅报告。
//! 输入输出：接收固定Git基线/head或显式工作区，输出缺少栏目并返回失败。
//! 依赖：复用syn语法树、quote指纹、stability的doc_lines和本机Git。
//! 禁止职责：不认证人审、不展开宏、不修改源码、不按行数决定质量。

use anyhow::{bail, Context, Result};
use quote::ToTokens;
use std::{collections::BTreeMap, path::Path, process::Command};
use syn::{spanned::Spanned, visit::Visit, Attribute, Visibility};

const FUNCTION: &[&str] = &[
    "功能",
    "参数",
    "返回",
    "错误",
    "状态",
    "并发与取消",
    "权限",
    "关联",
];
const TYPE: &[&str] = &["用途", "字段", "来源去向", "约束", "稳定性", "关联"];
const MODULE: &[&str] = &["职责", "输入输出", "依赖", "禁止职责"];

#[derive(Clone)]
struct Node {
    fingerprint: String,
    docs: String,
    fields: &'static [&'static str],
    line: usize,
}

#[derive(Default)]
struct Collector {
    nodes: BTreeMap<String, Node>,
    scope: Vec<String>,
}

fn visible(vis: &Visibility) -> bool {
    !matches!(vis, Visibility::Inherited)
}

impl Collector {
    fn add<T: ToTokens + Spanned>(
        &mut self,
        name: String,
        attrs: &[Attribute],
        item: &T,
        fields: &'static [&'static str],
    ) {
        let key = format!("{}::{name}", self.scope.join("::"));
        self.nodes.insert(
            key,
            Node {
                fingerprint: item.to_token_stream().to_string(),
                docs: crate::stability::doc_lines(attrs).join("\n"),
                fields,
                line: item.span().start().line,
            },
        );
    }
}

impl<'ast> Visit<'ast> for Collector {
    fn visit_item_fn(&mut self, item: &'ast syn::ItemFn) {
        if visible(&item.vis) {
            self.add(
                format!("fn:{}", item.sig.ident),
                &item.attrs,
                item,
                FUNCTION,
            );
        }
        // 局部函数不是跨模块API，不递归函数体。
    }
    fn visit_item_mod(&mut self, item: &'ast syn::ItemMod) {
        if visible(&item.vis) {
            // 模块内容不纳入模块指纹，私有实现改动不触发历史模块补注。
            let attrs = &item.attrs;
            let vis = &item.vis;
            let ident = &item.ident;
            let header = quote::quote!(#(#attrs)* #vis mod #ident;);
            self.add(format!("mod:{}", ident), attrs, &header, MODULE);
        }
        self.scope.push(format!("mod:{}", item.ident));
        syn::visit::visit_item_mod(self, item);
        self.scope.pop();
    }
    fn visit_item_struct(&mut self, item: &'ast syn::ItemStruct) {
        if visible(&item.vis) {
            self.add(format!("struct:{}", item.ident), &item.attrs, item, TYPE);
        }
        self.scope.push(format!("struct:{}", item.ident));
        syn::visit::visit_item_struct(self, item);
        self.scope.pop();
    }
    fn visit_item_enum(&mut self, item: &'ast syn::ItemEnum) {
        if visible(&item.vis) {
            self.add(format!("enum:{}", item.ident), &item.attrs, item, TYPE);
            self.scope.push(format!("enum:{}", item.ident));
            for variant in &item.variants {
                self.add(
                    format!("variant:{}", variant.ident),
                    &variant.attrs,
                    variant,
                    TYPE,
                );
            }
            self.scope.pop();
        }
    }
    fn visit_field(&mut self, item: &'ast syn::Field) {
        if visible(&item.vis) {
            let name = item
                .ident
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_else(|| item.ty.to_token_stream().to_string());
            self.add(format!("field:{name}"), &item.attrs, item, TYPE);
        }
    }
    fn visit_item_trait(&mut self, item: &'ast syn::ItemTrait) {
        if visible(&item.vis) {
            self.add(format!("trait:{}", item.ident), &item.attrs, item, TYPE);
            self.scope.push(format!("trait:{}", item.ident));
            for member in &item.items {
                match member {
                    syn::TraitItem::Fn(f) => {
                        self.add(format!("fn:{}", f.sig.ident), &f.attrs, f, FUNCTION)
                    }
                    syn::TraitItem::Type(t) => {
                        self.add(format!("type:{}", t.ident), &t.attrs, t, TYPE)
                    }
                    syn::TraitItem::Const(c) => {
                        self.add(format!("const:{}", c.ident), &c.attrs, c, TYPE)
                    }
                    _ => {}
                }
            }
            self.scope.pop();
        }
    }
    fn visit_item_impl(&mut self, item: &'ast syn::ItemImpl) {
        // trait impl继承trait定义的文档；具体实现改变仍须CHG/语义审核。
        if item.trait_.is_some() {
            return;
        }
        self.scope.push(format!(
            "impl:{}:{}",
            item.generics.to_token_stream(),
            item.self_ty.to_token_stream()
        ));
        for member in &item.items {
            match member {
                syn::ImplItem::Fn(f) if visible(&f.vis) => {
                    self.add(format!("fn:{}", f.sig.ident), &f.attrs, f, FUNCTION)
                }
                syn::ImplItem::Const(c) if visible(&c.vis) => {
                    self.add(format!("const:{}", c.ident), &c.attrs, c, TYPE)
                }
                syn::ImplItem::Type(t) if visible(&t.vis) => {
                    self.add(format!("type:{}", t.ident), &t.attrs, t, TYPE)
                }
                _ => {}
            }
        }
        self.scope.pop();
    }
    fn visit_item_type(&mut self, item: &'ast syn::ItemType) {
        if visible(&item.vis) {
            self.add(format!("type:{}", item.ident), &item.attrs, item, TYPE);
        }
    }
    fn visit_item_const(&mut self, item: &'ast syn::ItemConst) {
        if visible(&item.vis) {
            self.add(format!("const:{}", item.ident), &item.attrs, item, TYPE);
        }
    }
    fn visit_item_static(&mut self, item: &'ast syn::ItemStatic) {
        if visible(&item.vis) {
            self.add(format!("static:{}", item.ident), &item.attrs, item, TYPE);
        }
    }
    fn visit_item_union(&mut self, item: &'ast syn::ItemUnion) {
        if visible(&item.vis) {
            self.add(format!("union:{}", item.ident), &item.attrs, item, TYPE);
        }
        self.scope.push(format!("union:{}", item.ident));
        syn::visit::visit_item_union(self, item);
        self.scope.pop();
    }
}

fn nodes(source: &str) -> Result<BTreeMap<String, Node>> {
    let file = syn::parse_file(source)?;
    let mut collector = Collector::default();
    // 文件的模块级说明不包含正文指纹，已有文件只改私有内容不会受历史债阻断。
    let attrs = &file.attrs;
    collector.add(
        "file-module".into(),
        attrs,
        &quote::quote!(#(#attrs)*),
        MODULE,
    );
    collector.visit_file(&file);
    Ok(collector.nodes)
}

fn missing(node: &Node) -> Vec<&'static str> {
    node.fields
        .iter()
        .copied()
        .filter(|field| {
            let names: &[&str] = if *field == "字段" {
                &["字段", "变体"]
            } else {
                std::slice::from_ref(field)
            };
            !node.docs.lines().any(|line| {
                names.iter().any(|label| {
                    line.trim()
                        .strip_prefix(&format!("{label}："))
                        .is_some_and(|value| {
                            let value = value.trim();
                            !value.is_empty()
                                && value
                                    .chars()
                                    .any(|c| ('\u{3400}'..='\u{9fff}').contains(&c))
                                && !["TODO", "待填写", "待补充"].contains(&value)
                        })
                })
            })
        })
        .collect()
}

fn violations(before: &str, after: &str, all: bool) -> Result<Vec<String>> {
    let old = if before.is_empty() {
        BTreeMap::new()
    } else {
        nodes(before)?
    };
    let new = nodes(after)?;
    Ok(new
        .iter()
        .filter(|(name, node)| {
            all || old
                .get(*name)
                .is_none_or(|prior| prior.fingerprint != node.fingerprint)
        })
        .filter_map(|(name, node)| {
            let fields = missing(node);
            (!fields.is_empty())
                .then(|| format!("{}:{name} 缺少：{}", node.line, fields.join("、")))
        })
        .collect())
}

fn git(root: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let result = Command::new("git").args(args).current_dir(root).output()?;
    if !result.status.success() {
        bail!("git失败：{}", String::from_utf8_lossy(&result.stderr));
    }
    Ok(result.stdout)
}

fn resolve(root: &Path, name: &str) -> Result<String> {
    Ok(String::from_utf8(git(
        root,
        &[
            "rev-parse",
            "--verify",
            "--end-of-options",
            &format!("{name}^{{commit}}"),
        ],
    )?)?
    .trim()
    .into())
}

/// 功能：检查本次新增或变化的跨模块声明说明，不修改代码。
/// 参数：root为仓库根；args为base/head或worktree及可选all/report-only。
/// 返回：检查通过返回空结果。
/// 错误：Git、源码解析或必需注释不满足时返回诊断。
/// 状态：只读仓库。
/// 并发与取消：同步短任务，无共享可变状态；Git结束后返回。
/// 权限：读取当前用户可访问的仓库文件，不执行目标源码。
/// 关联：开发规范中的中文注释规则和增量审核。
pub fn run(root: &Path, args: &[String]) -> Result<()> {
    let mut base = None;
    let mut head = None;
    let mut worktree = false;
    let mut all = false;
    let mut report = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--base" | "--head" => {
                let value = args.get(i + 1).context("缺少引用参数")?;
                if args[i] == "--base" {
                    base = Some(resolve(root, value)?);
                } else {
                    head = Some(resolve(root, value)?);
                }
                i += 1;
            }
            "--worktree" => worktree = true,
            "--all" => all = true,
            "--report-only" => report = true,
            _ => bail!("未知comments参数：{}", args[i]),
        }
        i += 1;
    }
    let base = base.context("comments需要--base")?;
    if worktree == head.is_some() {
        bail!("必须恰好选择--head或--worktree");
    }
    let mut diff = vec![
        "diff",
        "--no-ext-diff",
        "--no-textconv",
        "--name-only",
        "-z",
        "--diff-filter=ACMRT",
        &base,
    ];
    if let Some(head) = head.as_deref() {
        diff.push(head);
    }
    let mut paths = git(root, &diff)?;
    if worktree {
        paths.extend(git(
            root,
            &["ls-files", "--others", "--exclude-standard", "-z"],
        )?);
    }
    if all {
        paths = if let Some(head) = head.as_deref() {
            git(root, &["ls-tree", "-r", "--name-only", "-z", head])?
        } else {
            git(
                root,
                &[
                    "ls-files",
                    "--cached",
                    "--others",
                    "--exclude-standard",
                    "-z",
                ],
            )?
        };
    }
    let mut failures = Vec::new();
    let paths: std::collections::BTreeSet<_> = paths
        .split(|b| *b == 0)
        .filter(|p| !p.is_empty())
        .map(|p| String::from_utf8(p.to_vec()))
        .collect::<std::result::Result<_, _>>()?;
    for path in paths.iter().filter(|p| p.ends_with(".rs")) {
        let before = Command::new("git")
            .args(["show", &format!("{base}:{path}")])
            .current_dir(root)
            .output()?;
        let before = if before.status.success() {
            String::from_utf8(before.stdout)?
        } else {
            String::new()
        };
        let after = if let Some(head) = head.as_deref() {
            String::from_utf8(git(root, &["show", &format!("{head}:{path}")])?)?
        } else {
            let resolved = root.join(path).canonicalize()?;
            if !resolved.starts_with(root.canonicalize()?) {
                bail!("源码链接越出仓库：{path}");
            }
            std::fs::read_to_string(resolved)?
        };
        for failure in violations(&before, &after, all).with_context(|| format!("解析{path}"))? {
            failures.push(format!("{path}:{failure}"));
        }
    }
    for failure in &failures {
        eprintln!("{failure}");
    }
    println!(
        "comments: {} 项缺口；宏生成/重导出语义由人工审核",
        failures.len()
    );
    if !report && !failures.is_empty() {
        bail!("中文注释检查未通过");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn documented(body: &str) -> String {
        let docs = FUNCTION
            .iter()
            .map(|f| format!("/// {f}：无。\n"))
            .collect::<String>();
        format!("{docs}{body}")
    }
    #[test]
    fn private_edit_does_not_trigger_existing_debt() {
        assert!(violations(
            "pub fn old(){} fn hidden(){let a=1;}",
            "pub fn old(){} fn hidden(){let a=2;}",
            false
        )
        .unwrap()
        .is_empty());
    }
    #[test]
    fn changed_body_and_changed_docs_are_checked() {
        assert!(
            !violations("pub fn old(){}", "pub fn old(){let a=1;}", false)
                .unwrap()
                .is_empty()
        );
        let source = documented("pub(crate) async fn fresh(){}");
        assert!(violations(&source, &source.replace("无。", ""), false)
            .unwrap()
            .iter()
            .any(|s| s.contains("功能")));
        assert!(!violations("", &source, false)
            .unwrap()
            .iter()
            .any(|s| s.contains("fn:fresh")));
    }
    #[test]
    fn nested_trait_method_field_and_inherent_method() {
        let code="mod hidden { pub trait Api { fn execute(&self); } pub struct Data { pub field: u8 } impl Data { pub fn read(&self){} } }";
        let result = violations("", code, false).unwrap().join("\n");
        for name in ["fn:execute", "field:field", "fn:read", "struct:Data"] {
            assert!(result.contains(name), "{result}");
        }
    }
    #[test]
    fn multiline_doc_attributes_and_parse_errors() {
        let attrs = FUNCTION
            .iter()
            .map(|f| format!("#[doc = \"{f}：说明\"]"))
            .collect::<String>();
        let result = violations("", &format!("{attrs} pub fn run(){{}}"), false).unwrap();
        assert!(!result.iter().any(|s| s.contains("fn:run")));
        assert!(violations("", "pub fn broken(", false).is_err());
        let english = FUNCTION
            .iter()
            .map(|f| format!("/// {f}：TODO\n"))
            .collect::<String>();
        assert!(
            violations("", &format!("{english}pub fn empty(){{}}"), false)
                .unwrap()
                .iter()
                .any(|s| s.contains("fn:empty"))
        );
    }
}
