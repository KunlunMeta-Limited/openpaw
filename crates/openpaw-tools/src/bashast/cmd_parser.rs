//! cmd.exe / batch AST parsing entry point.
//!
//! Mirrors the bash parser's approach: node-type whitelist traversal,
//! fail-closed for unknown structures.
//!
//! Port note: the `tree-sitter-batch` grammar dependency was stripped during
//! the port (pwsh/batch grammars are deferred to Phase 3 per the workspace
//! dependency plan). Without the grammar, batch commands cannot be
//! AST-analyzed, so [`parse_cmd`] fails closed with
//! [`ParseResult::ParseUnavailable`] and the parse-unavailable fallback decides
//! (regex match → Dangerous, clean → RequiresApproval). The function
//! signature is kept so callers (parse dispatch + fuzz baseline) keep
//! compiling unchanged.

use super::parser::ParseResult;

/// Parse a cmd.exe/batch command for security analysis.
///
/// Always returns [`ParseResult::ParseUnavailable`] in this port because the
/// tree-sitter-batch grammar is not available (Phase 3). Fail-closed: the
/// decision path treats an unavailable AST + clean regex as
/// RequiresApproval, never Safe.
pub fn parse_cmd(_command: &str) -> ParseResult {
    ParseResult::ParseUnavailable
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cmd_parse_fails_closed_without_batch_grammar() {
        // Grammar stripped in this port — the only valid behavior is the
        // fail-closed ParseUnavailable verdict.
        assert!(matches!(parse_cmd("dir"), ParseResult::ParseUnavailable));
    }
}
