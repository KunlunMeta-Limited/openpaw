//! `${VAR}` interpolation for MCP server config values.
//!
//! Scope is deliberately minimal (crate card §3.5): environment-variable
//! references only — no nested expressions, no command substitution, no
//! defaults. Anything else is an error (fail-closed), because a silently
//! unexpanded reference is how credentials leak into process arguments or
//! request headers.

use crate::error::{McpError, Result};

/// Environment lookup seam. Production callers pass `std::env::var`
/// (via [`expand`]); tests inject a map so parallel tests never race the
/// real process environment.
pub type EnvLookup<'a> = &'a dyn Fn(&str) -> Option<String>;

/// Expand every `${VAR}` in `input` against the given env source.
///
/// - `NAME` matches `[A-Za-z_][A-Za-z0-9_]*`; anything else inside the
///   braces is an error.
/// - `$NAME` without braces stays literal (no accidental expansion).
/// - An unclosed `${` is an error, not literal passthrough.
/// - An unset variable is an error naming the variable — never the value.
///
/// Stability: unstable.
pub fn expand_with(env: EnvLookup<'_>, input: &str) -> Result<String> {
    let mut out = String::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'$' {
            // Copy the whole run of non-$ bytes at once.
            let run_start = i;
            while i < bytes.len() && bytes[i] != b'$' {
                i += 1;
            }
            out.push_str(&input[run_start..i]);
            continue;
        }
        // At a '$': brace form is required for expansion.
        match bytes.get(i + 1) {
            Some(b'{') => {
                let close = input[i + 2..]
                    .find('}')
                    .map(|rel| i + 2 + rel)
                    .ok_or_else(|| {
                        McpError::interpolation(format!(
                            "unclosed ${{ at byte {i}: variable references must be ${{NAME}}; \
                             set the variable or remove the reference"
                        ))
                    })?;
                let name = &input[i + 2..close];
                validate_name(name, i)?;
                match env(name) {
                    Some(value) => out.push_str(&value),
                    None => {
                        return Err(McpError::interpolation(format!(
                            "environment variable '{name}' is not set \
                             (referenced by ${{VAR}} in the mcp config); \
                             export it before running openpaw"
                        )));
                    }
                }
                i = close + 1;
            }
            // `$x`, `$ ` and a trailing lone `$` are literal.
            _ => {
                out.push('$');
                i += 1;
            }
        }
    }
    Ok(out)
}

/// [`expand_with`] against the real process environment.
///
/// Stability: unstable.
pub fn expand(input: &str) -> Result<String> {
    expand_with(&|name| std::env::var(name).ok(), input)
}

/// True when the whole value is a single `${VAR}` reference — the only
/// shape accepted for HTTP headers, so secrets never live in the config
/// file itself ("keys only ever pass through environment variables").
///
/// Stability: unstable.
pub fn is_single_var_reference(input: &str) -> bool {
    let Some(rest) = input.strip_prefix("${") else {
        return false;
    };
    let Some(close_rel) = rest.find('}') else {
        return false;
    };
    if rest.len() != close_rel + 1 {
        // Trailing bytes after the closing brace: not a single reference.
        return false;
    }
    validate_name(&rest[..close_rel], 0).is_ok()
}

fn validate_name(name: &str, at: usize) -> Result<()> {
    let valid = !name.is_empty()
        && name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if valid {
        Ok(())
    } else {
        Err(McpError::interpolation(format!(
            "invalid variable name '{name}' at byte {at}: \
             expected ${{NAME}} with NAME = [A-Za-z_][A-Za-z0-9_]*"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn env_from(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: BTreeMap<String, String> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        move |name: &str| map.get(name).cloned()
    }

    #[test]
    fn expands_simple_and_repeated_references() {
        let env = env_from(&[("TOKEN", "abc"), ("HOME_DIR", "/h")]);
        assert_eq!(expand_with(&env, "${TOKEN}").unwrap(), "abc");
        assert_eq!(expand_with(&env, "${TOKEN}-${TOKEN}").unwrap(), "abc-abc");
        assert_eq!(expand_with(&env, "Bearer ${TOKEN}").unwrap(), "Bearer abc");
        assert_eq!(expand_with(&env, "${HOME_DIR}/x").unwrap(), "/h/x");
    }

    #[test]
    fn plain_text_and_dollar_without_braces_stay_literal() {
        let env = env_from(&[("TOKEN", "abc")]);
        assert_eq!(expand_with(&env, "no refs here").unwrap(), "no refs here");
        assert_eq!(expand_with(&env, "$TOKEN stays").unwrap(), "$TOKEN stays");
        assert_eq!(expand_with(&env, "cost: 5$").unwrap(), "cost: 5$");
        assert_eq!(expand_with(&env, "").unwrap(), "");
    }

    #[test]
    fn missing_variable_fails_closed_naming_the_var_only() {
        let env = env_from(&[]);
        let err = expand_with(&env, "Bearer ${SECRET_TOKEN}").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("SECRET_TOKEN"), "names the variable: {msg}");
        assert!(!msg.contains("abc"), "never echoes values: {msg}");
        assert!(matches!(err, McpError::Interpolation(_)));
    }

    #[test]
    fn unclosed_and_bad_names_are_errors() {
        let env = env_from(&[("A", "1")]);
        assert!(expand_with(&env, "${A").is_err());
        assert!(expand_with(&env, "${1BAD}").is_err());
        assert!(expand_with(&env, "${A B}").is_err());
        assert!(expand_with(&env, "${}").is_err());
        assert!(expand_with(&env, "${A}${").is_err());
    }

    #[test]
    fn single_var_reference_shape() {
        assert!(is_single_var_reference("${TOKEN}"));
        assert!(is_single_var_reference("${_A1}"));
        assert!(!is_single_var_reference("Bearer ${TOKEN}"));
        assert!(!is_single_var_reference("${TOKEN} "));
        assert!(!is_single_var_reference("$TOKEN"));
        assert!(!is_single_var_reference("${TOKEN"));
        assert!(!is_single_var_reference("${1BAD}"));
        assert!(!is_single_var_reference(""));
    }
}
