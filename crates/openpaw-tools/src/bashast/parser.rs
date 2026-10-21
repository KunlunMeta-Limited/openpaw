//! Tree-sitter-bash based command parser.
//!
//! Parses bash commands into [`SimpleCommand`] lists for security analysis.
//! Uses a node-type whitelist approach: only known-safe node types are allowed;
//! anything else returns [`ParseResult::TooComplex`].

use super::platform::{detect_shell_type, ShellType};

/// Command length limit (characters)
const MAX_COMMAND_LENGTH: usize = 10_000;
/// AST node budget limit
const MAX_NODES: usize = 50_000;
/// Parse timeout (milliseconds) — uses tree-sitter safe API set_timeout_micros
const PARSE_TIMEOUT_MS: u64 = 50;

/// AST parse result — three-state union.
///
/// Mirrors Claude Code's `ParseForSecurityResult`.
/// Design: fail-safe (parse failure never auto-allows).
#[derive(Debug, Clone, PartialEq)]
pub enum ParseResult {
    /// Successfully parsed into simple command list
    Simple { commands: Vec<SimpleCommand> },
    /// Parsed but contains structures too complex for static analysis
    TooComplex { reason: String, node_type: String },
    /// tree-sitter unavailable or parse failed entirely
    ParseUnavailable,
    /// parse aborted before AST parsing (input pre-check failed)
    ParseAborted { reason: String },
}

/// A simple command (quotes/env-vars/redirects resolved).
///
/// One `SimpleCommand` = one atomic command without control flow.
#[derive(Debug, Clone, PartialEq)]
pub struct SimpleCommand {
    /// Original source snippet (for logging/audit)
    pub text: String,
    /// Parsed argument array (quotes expanded)
    /// argv[0] is the command name (env var prefix stripped)
    pub argv: Vec<String>,
    /// Environment variable assignments prefix
    /// e.g. `FOO=bar ls` → env_vars = [("FOO", "bar")]
    pub env_vars: Vec<(String, String)>,
    /// Redirects
    pub redirects: Vec<Redirect>,
}

/// Redirect information
#[derive(Debug, Clone, PartialEq)]
pub struct Redirect {
    pub kind: RedirectKind,
    pub target: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RedirectKind {
    Stdout,       // >
    StdoutAppend, // >>
    Stdin,        // <
    Stderr,       // 2>
    StderrAppend, // 2>>
    Both,         // &> or 2>&1
}

/// Variable value resolution state
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum VarValue {
    /// Static literal value — can be used for path validation
    Literal(String),
    /// Unknown value (loop variable, read variable, etc.)
    /// Currently matched in simple_expansion resolution but not yet produced
    /// (future: loop variables, read variables will produce this)

    #[allow(dead_code)]
    // API-23: variants `Placeholder` and `CmdSubPlaceholder` are never constructed
    Placeholder,
    /// Command substitution output $(cmd)
    /// Currently matched in simple_expansion resolution but not yet produced
    /// (future: command substitution tracking will produce this)

    #[allow(dead_code)] // API-23: variant `CmdSubPlaceholder` is never constructed
    CmdSubPlaceholder,
}

/// Variable scope — maps variable names to their resolved values
pub(crate) type VarScope = std::collections::HashMap<String, VarValue>;

/// Placeholder constants for unresolved variables
const PLACEHOLDER_STR: &str = "<PLACEHOLDER>";
const CMDSUB_PLACEHOLDER_STR: &str = "<CMDSUB_PLACEHOLDER>";

/// Pre-check before AST parsing — detect known problematic input
fn precheck(command: &str) -> Option<ParseResult> {
    // Length check
    if command.len() > MAX_COMMAND_LENGTH {
        return Some(ParseResult::ParseAborted {
            reason: format!(
                "command exceeds max length ({} > {})",
                command.len(),
                MAX_COMMAND_LENGTH
            ),
        });
    }
    // Control character detection (except \t \n \r)
    for c in command.chars() {
        if c.is_control() && c != '\t' && c != '\n' && c != '\r' {
            return Some(ParseResult::TooComplex {
                reason: format!("control character detected: U+{:04X}", c as u32),
                node_type: "control_char".into(),
            });
        }
    }
    // Unicode whitespace detection
    for c in command.chars() {
        if is_unicode_whitespace(c) {
            return Some(ParseResult::TooComplex {
                reason: format!("unicode whitespace detected: U+{:04X}", c as u32),
                node_type: "unicode_whitespace".into(),
            });
        }
    }
    None
}

/// Check if a character is a Unicode whitespace character that bash
/// doesn't handle as a standard token separator.
fn is_unicode_whitespace(c: char) -> bool {
    matches!(
        c,
        '\u{00A0}' | '\u{2000}'..='\u{200B}' | '\u{202F}' | '\u{205F}' | '\u{3000}' | '\u{FEFF}'
    )
}

/// Main entry — parse a shell command for security analysis.
///
/// Dispatches to the appropriate tree-sitter grammar based on detected
/// shell type (bash, PowerShell, cmd.exe). Unknown shell types return
/// `ParseUnavailable`. Internal panics are caught via `catch_unwind`.
/// 功能：兼容原自动探测shell的解析入口
/// 参数：command为待分析命令
/// 返回：原解析结果
/// 错误：失败返回ParseUnavailable
/// 状态：只读分析
/// 并发与取消：使用原长度/节点/时间预算
/// 权限：工具执行必须使用parse_for_shell绑定实际launcher
/// 关联：需求/设计 DES-OP-004；REQ-OP-005。
/// Stability: unstable.
pub fn parse_for_security(command: &str) -> ParseResult {
    parse_for_shell(command, detect_shell_type(command))
}

/// 功能：按实际执行shell复用原解析链
/// 参数：command为原文，shell为实际launcher类型
/// 返回：解析结果或不可解析
/// 错误：内部异常转为不可解析
/// 状态：只读分析，不执行
/// 并发与取消：有原长度/节点预算；无异步取消
/// 权限：未知或缺grammar不能判安全
/// 关联：需求/设计 DES-OP-004；REQ-OP-005。
/// Stability: unstable.
pub(crate) fn parse_for_shell(command: &str, shell: ShellType) -> ParseResult {
    if command.is_empty() {
        return ParseResult::Simple { commands: vec![] };
    }
    // pre-check
    if let Some(result) = precheck(command) {
        return result;
    }
    match shell {
        ShellType::Bash => parse_bash(command),
        // Plan D-2 / Phase 3: tree-sitter-pwsh is not part of this workspace,
        // so PowerShell syntax cannot be AST-analyzed here. Fail closed →
        // ParseUnavailable lets the fallback decide (regex match →
        // Dangerous, clean → RequiresApproval) instead of analyzing
        // PowerShell syntax with bash grammar.
        ShellType::PowerShell => ParseResult::ParseUnavailable,
        ShellType::Cmd => super::cmd_parser::parse_cmd(command),
        ShellType::Unknown => ParseResult::ParseUnavailable,
    }
}

#[allow(deprecated)] // set_timeout_micros deprecated in 0.25 but still functional
fn parse_bash(command: &str) -> ParseResult {
    let result = std::panic::catch_unwind(|| {
        let mut parser = tree_sitter::Parser::new();
        // Preserve tree-sitter language error details instead of discarding.
        parser
            .set_language(&tree_sitter_bash::LANGUAGE.into())
            .map_err(|_| "language error")?;
        // set parse timeout to prevent DoS
        parser.set_timeout_micros(PARSE_TIMEOUT_MS * 1_000);

        let tree = parser.parse(command, None).ok_or("parse failed")?;
        let root = tree.root_node();
        Ok::<ParseResult, &str>(collect_commands(command, root))
    });

    match result {
        Ok(Ok(r)) => r,
        Ok(Err(_)) => ParseResult::ParseUnavailable,
        Err(_) => ParseResult::ParseUnavailable,
    }
}

pub(crate) fn collect_commands(source: &str, node: tree_sitter::Node) -> ParseResult {
    let mut commands = Vec::new();
    let mut scope = VarScope::new();
    let mut node_count: usize = 0;
    match walk_node(source, node, &mut commands, &mut scope, &mut node_count) {
        WalkResult::Ok => ParseResult::Simple { commands },
        WalkResult::TooComplex(reason, node_type) => ParseResult::TooComplex { reason, node_type },
    }
}

pub(crate) enum WalkResult {
    Ok,
    TooComplex(String, String),
}

/// Check if a tree-sitter node type is allowed in "simple" commands.
///
/// Allowed types are safe to statically analyze. Anything not in this list
/// → `TooComplex`.
fn is_allowed_node(node_type: &str) -> bool {
    matches!(
        node_type,
        "program"
            | "command"
            | "command_name"
            | "command_argument"
            | "variable_assignment"
            | "file_redirect"
            | "redirect"
            | "pipeline"
            | "list"
            | "string"
            | "raw_string"
            | "concatenation"
            | "simple_expansion"
            | "word"
            | "number"
            | "assignment"
            | "assign"
            | "env_variable"
            | "subscript"
            | "extglob_pattern"
            | "variable_name"
    )
    // Notable excluded types → TooComplex:
    // "command_substitution" ($(...))  — command substitution
    // "process_substitution" (<(...) >(...))  — process substitution
    // "case_item" / "case_statement"  — case control flow
    // "for_statement" / "while_statement" / "if_statement"  — control flow
    // "function_definition"  — function definition
    // "subshell"  — subshell
    // "heredoc_body" / "heredoc_start"  — heredoc
    // "declaration_command" / "unset_command" — special builtins
    // "test_command" — [[ ... ]]
    // "negated_command" — ! command
}

fn walk_node(
    source: &str,
    node: tree_sitter::Node,
    commands: &mut Vec<SimpleCommand>,
    scope: &mut VarScope,
    node_count: &mut usize,
) -> WalkResult {
    // node budget check
    *node_count += 1;
    if *node_count > MAX_NODES {
        return WalkResult::TooComplex(
            format!("exceeded max node budget ({})", MAX_NODES),
            "node_budget".into(),
        );
    }

    let node_type = node.kind();

    // Skip unnamed nodes (operators like &&, ||, |, ;)
    if !node.is_named() {
        return WalkResult::Ok;
    }

    // Check for TooComplex node types first
    if is_too_complex_node(node_type) {
        return WalkResult::TooComplex(
            format!("contains complex structure: {}", node_type),
            node_type.to_string(),
        );
    }

    match node_type {
        "program" | "list" | "pipeline" => {
            // Scope isolation — | / |& / & create new scope (subshell/background);
            // && / || / ; inherit scope (same process)
            let mut cursor = node.walk();
            let children: Vec<_> = node.children(&mut cursor).collect();
            for (i, child) in children.iter().enumerate() {
                // Check separator before this child for scope isolation
                if i > 0 {
                    let prev = children[i - 1];
                    // All separators (&&, ||, |, ;, &) are anonymous tokens
                    let sep = if !prev.is_named() {
                        node_text(prev, source).trim()
                    } else {
                        ""
                    };
                    // | / |& / & → new scope; && / || / ; → inherit scope
                    if sep == "|" || sep == "|&" || sep == "&" {
                        let mut scope_copy = scope.clone();
                        match walk_node(source, *child, commands, &mut scope_copy, node_count) {
                            WalkResult::Ok => {}
                            other => return other,
                        }
                        continue;
                    }
                }
                // For named children, recurse normally (inheriting scope)
                if child.is_named() {
                    match walk_node(source, *child, commands, scope, node_count) {
                        WalkResult::Ok => {}
                        other => return other,
                    }
                }
            }
            WalkResult::Ok
        }
        "redirected_statement" => {
            // Parse command and redirects together
            let mut cursor = node.walk();
            let mut cmd_redirects = Vec::new();
            for child in node.children(&mut cursor) {
                let child_type = child.kind();
                if child_type == "file_redirect" || child_type == "herestring_redirect" {
                    if let Some(r) = parse_redirect(source, child) {
                        cmd_redirects.push(r);
                    }
                } else {
                    match walk_node(source, child, commands, scope, node_count) {
                        WalkResult::Ok => {}
                        other => return other,
                    }
                }
            }
            // Attach redirects to the last command
            if let Some(last) = commands.last_mut() {
                last.redirects.extend(cmd_redirects);
            }
            WalkResult::Ok
        }
        "command" => {
            // Parse a simple command — extract argv, env_vars, redirects
            match parse_command_node(source, node, scope) {
                Ok(cmd) => {
                    commands.push(cmd);
                    WalkResult::Ok
                }
                Err(e) => e,
            }
        }
        "variable_assignment" => {
            // Standalone variable assignment (direct child of list/pipeline,
            // not wrapped in a command node). Track in scope and create a
            // SimpleCommand so the assignment is visible to subsequent commands.
            let assign_text = node_text(node, source);
            if let Some((name, val)) = parse_assignment(assign_text) {
                scope.insert(name.clone(), VarValue::Literal(val.clone()));
                commands.push(SimpleCommand {
                    text: assign_text.trim().to_string(),
                    argv: vec![],
                    env_vars: vec![(name, val)],
                    redirects: vec![],
                });
            }
            WalkResult::Ok
        }
        _ => {
            // Unknown node type — check if allowed
            if is_allowed_node(node_type) {
                // Recurse into children
                let mut cursor = node.walk();
                for child in node.children(&mut cursor) {
                    match walk_node(source, child, commands, scope, node_count) {
                        WalkResult::Ok => {}
                        other => return other,
                    }
                }
                WalkResult::Ok
            } else {
                WalkResult::TooComplex(
                    format!("unhandled node type: {}", node_type),
                    node_type.to_string(),
                )
            }
        }
    }
}

/// Node types that are definitely too complex for static analysis.
fn is_too_complex_node(node_type: &str) -> bool {
    matches!(
        node_type,
        "command_substitution"
            | "process_substitution"
            | "case_item"
            | "case_statement"
            | "for_statement"
            | "while_statement"
            | "until_statement"
            | "if_statement"
            | "function_definition"
            | "subshell"
            | "heredoc_body"
            | "heredoc_start"
            | "heredoc"
            | "declaration_command"
            | "unset_command"
            | "test_command"
            | "negated_command"
            | "c_style_for_statement"
            | "compound_command"
            | "arithmetic_expansion"
    )
}

/// Helper: safely extract text from a tree-sitter node.
pub(crate) fn node_text<'a>(node: tree_sitter::Node, source: &'a str) -> &'a str {
    node.utf8_text(source.as_bytes()).unwrap_or("")
}

/// Parse a `command` node into a `SimpleCommand`.
fn parse_command_node(
    source: &str,
    node: tree_sitter::Node,
    scope: &mut VarScope,
) -> Result<SimpleCommand, WalkResult> {
    let mut argv = Vec::new();
    let mut env_vars = Vec::new();
    let mut redirects = Vec::new();
    let text = node_text(node, source).trim().to_string();

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        let child_type = child.kind();
        match child_type {
            "variable_assignment" | "assignment" | "env_variable" => {
                // Parse VAR=val
                let assign_text = node_text(child, source);
                if let Some((name, val)) = parse_assignment(assign_text) {
                    // Track variable in scope
                    scope.insert(name.clone(), VarValue::Literal(val.clone()));
                    env_vars.push((name, val));
                }
            }
            "command_name" | "command_argument" | "word" | "raw_string" | "number" => {
                let text = extract_text(source, child);
                if !text.is_empty() {
                    argv.push(text);
                }
            }
            "string" | "concatenation" => {
                let text = extract_text(source, child);
                if !text.is_empty() {
                    argv.push(text);
                }
            }
            "simple_expansion" => {
                // Resolve $VAR via scope
                let var_name = extract_var_name(source, child);
                let resolved = scope.get(&var_name);
                match resolved {
                    Some(VarValue::Literal(v)) => {
                        // Check if bare expansion is safe (no spaces/globs)
                        if is_bare_var_unsafe(v) {
                            return Err(WalkResult::TooComplex(
                                format!("variable value unsafe for bare expansion: ${}", var_name),
                                "simple_expansion".into(),
                            ));
                        }
                        argv.push(v.clone());
                    }
                    Some(VarValue::Placeholder) => {
                        argv.push(PLACEHOLDER_STR.to_string());
                    }
                    Some(VarValue::CmdSubPlaceholder) => {
                        argv.push(CMDSUB_PLACEHOLDER_STR.to_string());
                    }
                    None => {
                        // Undefined variable — keep $VAR as-is
                        let text = extract_text(source, child);
                        if !text.is_empty() {
                            argv.push(text);
                        }
                    }
                }
            }
            "file_redirect" | "redirect" => {
                if let Some(redirect) = parse_redirect(source, child) {
                    redirects.push(redirect);
                }
            }
            _ => {
                // Check if it's a too-complex child
                if is_too_complex_node(child_type) {
                    return Err(WalkResult::TooComplex(
                        format!("contains complex structure: {}", child_type),
                        child_type.to_string(),
                    ));
                }
                // Try to extract text from other allowed nodes
                if is_allowed_node(child_type) {
                    let text = extract_text(source, child);
                    if !text.is_empty() {
                        argv.push(text);
                    }
                } else {
                    return Err(WalkResult::TooComplex(
                        format!("unhandled child node: {}", child_type),
                        child_type.to_string(),
                    ));
                }
            }
        }
    }

    // Solo-placeholder protection — any unresolved variable as an
    // argument is too dangerous to analyze (could expand to anything).
    if argv
        .iter()
        .any(|arg| arg == PLACEHOLDER_STR || arg == CMDSUB_PLACEHOLDER_STR)
    {
        return Err(WalkResult::TooComplex(
            "unresolved variable placeholder in arguments".into(),
            "simple_expansion".into(),
        ));
    }

    // Audit remediation: an unresolved variable/expansion in the COMMAND
    // NAME position cannot be analyzed — the real command is only known at
    // runtime (`CMD=rm; "$CMD" -rf /` parses as `"$CMD" -rf /`, and every
    // downstream layer previously saw an unknown-but-harmless command).
    // Fail closed: require approval instead of judging the literal text.
    // (Quoted/unquoted/braced forms all normalize to a `$`-prefixed argv[0]:
    // `"$CMD"`, `$CMD`, `"${CMD}x"`.)
    if let Some(first) = argv.first() {
        if first.starts_with('$') {
            return Err(WalkResult::TooComplex(
                format!("unresolved variable in command name: {first}"),
                "simple_expansion".into(),
            ));
        }
    }

    Ok(SimpleCommand {
        text,
        argv,
        env_vars,
        redirects,
    })
}

/// Extract variable name from a simple_expansion node
fn extract_var_name(source: &str, node: tree_sitter::Node) -> String {
    let text = node_text(node, source);
    if let Some(inner) = text.strip_prefix("${") {
        if let Some(inner) = inner.strip_suffix('}') {
            return inner.to_string();
        }
    }
    if let Some(name) = text.strip_prefix('$') {
        return name.to_string();
    }
    text.to_string()
}

/// Check if a bare variable value is unsafe (contains spaces/globs)
fn is_bare_var_unsafe(value: &str) -> bool {
    value.contains(' ')
        || value.contains('\t')
        || value.contains('\n')
        || value.contains('*')
        || value.contains('?')
        || value.contains('[')
}

/// Extract text from a node, stripping quotes for string nodes.
pub(crate) fn extract_text(source: &str, node: tree_sitter::Node) -> String {
    // For wrapper nodes (command_name, word), recurse into children
    let node_type = node.kind();
    if node_type == "command_name" || node_type == "word" {
        let mut cursor = node.walk();
        let mut result = String::new();
        for child in node.children(&mut cursor) {
            if child.is_named() {
                result.push_str(&extract_text(source, child));
            }
        }
        if result.is_empty() {
            return node_text(node, source).trim().to_string();
        }
        return result;
    }

    let raw = node_text(node, source);

    match node_type {
        "string" | "raw_string" => {
            // Remove surrounding quotes (both single and double)
            let trimmed = raw.trim();
            if trimmed.len() >= 2 {
                let first = trimmed.chars().next().unwrap();
                let last = trimmed.chars().last().unwrap();
                if (first == '"' && last == '"') || (first == '\'' && last == '\'') {
                    return trimmed[1..trimmed.len() - 1].to_string();
                }
            }
            raw.to_string()
        }
        "concatenation" => {
            // For concatenation, combine all child text
            let mut result = String::new();
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                result.push_str(&extract_text(source, child));
            }
            result
        }
        "simple_expansion" => {
            // Keep $VAR as-is
            raw.to_string()
        }
        _ => raw.trim().to_string(),
    }
}

/// Parse a variable assignment like `FOO=bar` or `FOO="hello world"`.
pub(crate) fn parse_assignment(text: &str) -> Option<(String, String)> {
    let text = text.trim();
    if let Some(eq_pos) = text.find('=') {
        let name = text[..eq_pos].trim().to_string();
        let mut val = text[eq_pos + 1..].trim().to_string();
        // Strip quotes from value
        if val.len() >= 2 {
            let first = val.chars().next().unwrap();
            let last = val.chars().last().unwrap();
            if (first == '"' && last == '"') || (first == '\'' && last == '\'') {
                val = val[1..val.len() - 1].to_string();
            }
        }
        if !name.is_empty() {
            return Some((name, val));
        }
    }
    None
}

/// Parse a redirect node.
pub(crate) fn parse_redirect(source: &str, node: tree_sitter::Node) -> Option<Redirect> {
    let text = node_text(node, source).trim();

    // Decompose the raw text into `[fd] operator target` reliably.
    // The tree-sitter child layout varies between node kinds, so text-based
    // decomposition is the authoritative path. Examples:
    //   "2> err.txt" → fd=2, op=">", target="err.txt"
    //   "> /dev/null" → fd=None, op=">", target="/dev/null"
    //   ">> log" → op=">>", target="log"
    //   "2>&1" → fd=2, op=">&", target="1"
    //   "&> out.txt" → op="&>", target="out.txt"
    //   "<<< $'x'" → op="<<<" (herestring, input side)
    let mut rest = text;
    let mut fd: Option<u32> = None;
    let digit_end = rest.chars().take_while(|c| c.is_ascii_digit()).count();
    if digit_end > 0 {
        if let Some(next) = rest.chars().nth(digit_end) {
            if matches!(next, '>' | '<') {
                fd = rest[..digit_end].parse().ok();
                rest = &rest[digit_end..];
            }
        }
    }
    let op_len = rest
        .chars()
        .take_while(|c| matches!(c, '<' | '>' | '&'))
        .count();
    if op_len == 0 {
        return None;
    }
    let op = &rest[..op_len];
    let target = rest[op_len..].trim().to_string();

    let kind = if op.contains('<') && op != "<>" {
        // `<`, `<<<` — input redirection (read side)
        RedirectKind::Stdin
    } else {
        match op {
            ">>" => {
                if fd == Some(2) {
                    RedirectKind::StderrAppend
                } else {
                    RedirectKind::StdoutAppend
                }
            }
            "&>" | ">&" => RedirectKind::Both,
            // `<>` opens read+write — treat as a write.
            "<>" => RedirectKind::Stdout,
            _ => {
                if fd == Some(2) {
                    RedirectKind::Stderr
                } else {
                    RedirectKind::Stdout
                }
            }
        }
    };

    Some(Redirect { kind, target })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_command() {
        let r = parse_for_security("ls -la");
        match r {
            ParseResult::Simple { commands } => {
                assert_eq!(commands.len(), 1);
                assert_eq!(commands[0].argv, vec!["ls", "-la"]);
            }
            _ => panic!("expected Simple, got {:?}", r),
        }
    }

    #[test]
    fn test_compound_and() {
        let r = parse_for_security("ls && git push");
        match r {
            ParseResult::Simple { commands } => {
                assert_eq!(commands.len(), 2);
                assert_eq!(commands[0].argv, vec!["ls"]);
                assert_eq!(commands[1].argv, vec!["git", "push"]);
            }
            _ => panic!("expected Simple, got {:?}", r),
        }
    }

    #[test]
    fn test_compound_pipe_semicolon() {
        let r = parse_for_security("a | b; c");
        match r {
            ParseResult::Simple { commands } => {
                assert_eq!(commands.len(), 3);
            }
            _ => panic!("expected Simple, got {:?}", r),
        }
    }

    #[test]
    fn test_compound_or() {
        let r = parse_for_security("a || b");
        match r {
            ParseResult::Simple { commands } => {
                assert_eq!(commands.len(), 2);
            }
            _ => panic!("expected Simple, got {:?}", r),
        }
    }

    #[test]
    fn test_empty_command() {
        let r = parse_for_security("");
        assert!(matches!(r, ParseResult::Simple { commands } if commands.is_empty()));
    }

    #[test]
    fn test_command_substitution() {
        let r = parse_for_security("echo $(whoami)");
        match r {
            ParseResult::TooComplex { node_type, .. } => {
                assert_eq!(node_type, "command_substitution");
            }
            _ => panic!("expected TooComplex, got {:?}", r),
        }
    }

    #[test]
    fn test_env_var_prefix() {
        let r = parse_for_security("FOO=bar ls");
        match r {
            ParseResult::Simple { commands } => {
                assert_eq!(commands.len(), 1);
                assert_eq!(commands[0].argv, vec!["ls"]);
                assert_eq!(
                    commands[0].env_vars,
                    vec![("FOO".to_string(), "bar".to_string())]
                );
            }
            _ => panic!("expected Simple, got {:?}", r),
        }
    }

    #[test]
    fn test_redirect() {
        let r = parse_for_security("ls > file.txt");
        match r {
            ParseResult::Simple { commands } => {
                assert_eq!(commands.len(), 1);
                assert!(!commands[0].redirects.is_empty());
            }
            _ => panic!("expected Simple, got {:?}", r),
        }
    }

    #[test]
    fn test_for_loop() {
        let r = parse_for_security("for i in 1 2 3; do echo $i; done");
        assert!(matches!(r, ParseResult::TooComplex { .. }));
    }

    #[test]
    fn test_if_statement() {
        let r = parse_for_security("if true; then echo yes; fi");
        assert!(matches!(r, ParseResult::TooComplex { .. }));
    }

    #[test]
    fn test_while_loop() {
        let r = parse_for_security("while true; do echo hi; done");
        assert!(matches!(r, ParseResult::TooComplex { .. }));
    }

    #[test]
    fn test_function_definition() {
        let r = parse_for_security("foo() { echo bar; }");
        assert!(matches!(r, ParseResult::TooComplex { .. }));
    }

    #[test]
    fn test_subshell() {
        let r = parse_for_security("(cd /tmp && ls)");
        assert!(matches!(r, ParseResult::TooComplex { .. }));
    }

    #[test]
    fn test_pipeline() {
        let r = parse_for_security("cat file | grep pattern | head -10");
        match r {
            ParseResult::Simple { commands } => {
                assert_eq!(commands.len(), 3);
                assert_eq!(commands[0].argv[0], "cat");
                assert_eq!(commands[1].argv[0], "grep");
                assert_eq!(commands[2].argv[0], "head");
            }
            _ => panic!("expected Simple, got {:?}", r),
        }
    }

    #[test]
    fn test_quoted_string() {
        let r = parse_for_security("echo 'hello world'");
        match r {
            ParseResult::Simple { commands } => {
                assert_eq!(commands.len(), 1);
                assert_eq!(commands[0].argv, vec!["echo", "hello world"]);
            }
            _ => panic!("expected Simple, got {:?}", r),
        }
    }

    #[test]
    fn test_double_quoted_string() {
        let r = parse_for_security("echo \"hello world\"");
        match r {
            ParseResult::Simple { commands } => {
                assert_eq!(commands.len(), 1);
                assert_eq!(commands[0].argv, vec!["echo", "hello world"]);
            }
            _ => panic!("expected Simple, got {:?}", r),
        }
    }

    // === Parser Robustness tests ===

    #[test]
    fn test_b04_max_command_length() {
        let long_cmd = "echo ".to_string() + &"a".repeat(MAX_COMMAND_LENGTH);
        let r = parse_for_security(&long_cmd);
        assert!(
            matches!(r, ParseResult::ParseAborted { .. }),
            "expected ParseAborted, got {:?}",
            r
        );
    }

    #[test]
    fn test_b04_control_char() {
        let cmd = "echo \x01hello";
        let r = parse_for_security(cmd);
        match r {
            ParseResult::TooComplex { node_type, .. } => {
                assert_eq!(node_type, "control_char");
            }
            _ => panic!("expected TooComplex, got {:?}", r),
        }
    }

    #[test]
    fn test_b04_unicode_whitespace() {
        let cmd = "echo\u{00A0}hello";
        let r = parse_for_security(cmd);
        match r {
            ParseResult::TooComplex { node_type, .. } => {
                assert_eq!(node_type, "unicode_whitespace");
            }
            _ => panic!("expected TooComplex, got {:?}", r),
        }
    }

    // === Variable Scope Tracking tests ===

    #[test]
    fn test_b03_var_static_value() {
        let r = parse_for_security("FOO=bar; echo $FOO");
        match r {
            ParseResult::Simple { commands } => {
                assert_eq!(commands.len(), 2);
                // $FOO should resolve to the literal "bar"
                assert_eq!(commands[1].argv, vec!["echo", "bar"]);
            }
            _ => panic!("expected Simple, got {:?}", r),
        }
    }

    #[test]
    fn test_b03_var_and_inherit() {
        let r = parse_for_security("X=hello && echo $X");
        match r {
            ParseResult::Simple { commands } => {
                assert_eq!(commands.len(), 2);
                // && inherits scope — $X should resolve to "hello"
                assert_eq!(commands[1].argv, vec!["echo", "hello"]);
            }
            _ => panic!("expected Simple, got {:?}", r),
        }
    }

    #[test]
    fn test_b03_var_pipe_no_inherit() {
        let r = parse_for_security("true | X=hello; echo $X");
        match r {
            ParseResult::Simple { commands } => {
                assert_eq!(commands.len(), 3);
                // X was set in a pipe subshell — should NOT be visible after ;
                assert_eq!(commands[2].argv, vec!["echo", "$X"]);
            }
            _ => panic!("expected Simple, got {:?}", r),
        }
    }
}
