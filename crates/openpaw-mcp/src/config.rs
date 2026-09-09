//! `[mcp.servers.<name>]` config parsing, validation, and normalization.
//!
//! Source of truth for the schema is the engineering plan §11:
//!
//! ```toml
//! [mcp.servers.<name>]
//! transport = "stdio"                  # stdio | http
//! command = ["npx", "-y", "<server>"]  # transport = "http": url = "..."
//! ```
//!
//! with these additions: `env` (per-server, `${VAR}`-interpolated),
//! `cwd` (stdio), `headers` (http; each value must be a single `${VAR}`
//! reference so secrets never live in the file), `timeout_secs`
//! (default 60, 1..=3600), `enabled` (default true).
//!
//! Fail-closed rules: an unknown transport, a missing command/url, a
//! `.cmd`/`.bat` command (unreliable to spawn directly on Windows), an
//! `https://` server (no TLS in the 1.0 HTTP client), a non-`http://` URL,
//! or a plain-text header value all reject the config. A missing config
//! file is not an error — it is the zero-config, zero-overhead default.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::error::{McpError, Result};
use crate::interpolator::{expand_with, is_single_var_reference, EnvLookup};

/// Default per-call timeout when `timeout_secs` is absent.
pub const DEFAULT_TIMEOUT_SECS: u64 = 60;

/// One validated, normalized MCP server entry.
///
/// Stability: unstable.
#[derive(Debug)]
pub struct ServerEntry {
    /// Config key under `[mcp.servers]`; becomes the `mcp__<name>__<tool>`
    /// tool-name prefix.
    pub name: String,
    pub spec: ServerSpec,
    pub timeout_secs: u64,
    pub enabled: bool,
}

/// Validated transport-specific server specification.
///
/// Stability: unstable.
#[derive(Debug)]
pub enum ServerSpec {
    Stdio {
        /// argv; every element had `${VAR}` references expanded.
        command: Vec<String>,
        /// Per-server environment overlay, values expanded.
        env: BTreeMap<String, String>,
        /// Optional working directory (not interpolated).
        cwd: Option<PathBuf>,
    },
    Http {
        /// Parsed `http://` URL (`https` is a documented 1.0 limitation).
        url: url::Url,
        /// Header values, each expanded from its single `${VAR}` reference.
        headers: BTreeMap<String, String>,
    },
}

/// All configured MCP servers, in file order.
///
/// Stability: unstable.
#[derive(Debug, Default)]
pub struct McpConfig {
    pub servers: Vec<ServerEntry>,
}

impl McpConfig {
    /// Load from `<home>/config.toml`. A missing file yields the empty
    /// config (zero servers, zero overhead); a present-but-invalid file is
    /// a config error.
    ///
    /// Stability: unstable.
    pub fn load(home: &Path) -> Result<Self> {
        let path = home.join("config.toml");
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => {
                return Err(McpError::config(format!(
                    "cannot read {}: {e}; check file permissions",
                    path.display()
                )))
            }
        };
        Self::load_from_str(&text, &|name| std::env::var(name).ok())
    }

    /// Parse config text with an injectable environment source (tests use
    /// this to avoid racing the real process environment).
    ///
    /// Stability: unstable.
    pub fn load_from_str(text: &str, env: EnvLookup<'_>) -> Result<Self> {
        let raw: RawFile = toml::from_str(text)
            .map_err(|e| McpError::config(format!("invalid TOML in config.toml: {e}")))?;
        let Some(raw_mcp) = raw.mcp else {
            return Ok(Self::default());
        };
        let mut servers = Vec::new();
        for (name, raw_server) in raw_mcp.servers {
            servers.push(build_entry(name, raw_server, env)?);
        }
        Ok(Self { servers })
    }

    /// Names of enabled servers, in config order.
    ///
    /// Stability: unstable.
    pub fn enabled_names(&self) -> Vec<&str> {
        self.servers
            .iter()
            .filter(|s| s.enabled)
            .map(|s| s.name.as_str())
            .collect()
    }
}

#[derive(Deserialize)]
struct RawFile {
    mcp: Option<RawMcp>,
}

#[derive(Deserialize)]
struct RawMcp {
    #[serde(default)]
    servers: BTreeMap<String, RawServer>,
}

#[derive(Deserialize)]
struct RawServer {
    transport: Option<String>,
    command: Option<Vec<String>>,
    url: Option<String>,
    env: Option<BTreeMap<String, String>>,
    headers: Option<BTreeMap<String, String>>,
    cwd: Option<PathBuf>,
    timeout_secs: Option<u64>,
    enabled: Option<bool>,
}

fn fail(server: &str, what: &str, next: &str) -> McpError {
    McpError::config(format!("{what} (server '{server}'); {next}"))
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

fn build_entry(name: String, raw: RawServer, env: EnvLookup<'_>) -> Result<ServerEntry> {
    if !valid_name(&name) {
        return Err(fail(
            &name,
            "server name must be 1-64 chars of [A-Za-z0-9_-]",
            "rename the `[mcp.servers.<name>]` key; the name becomes the \
             mcp__<name>__<tool> tool prefix",
        ));
    }

    let timeout_secs = match raw.timeout_secs {
        Some(x) if x == 0 || x > 3600 => {
            return Err(fail(
                &name,
                &format!("timeout_secs {x} is out of range 1..=3600"),
                "set a sane per-call timeout, e.g. timeout_secs = 60",
            ));
        }
        Some(x) => x,
        None => DEFAULT_TIMEOUT_SECS,
    };
    let enabled = raw.enabled.unwrap_or(true);

    let transport = raw.transport.as_deref().unwrap_or_default().to_string();
    let spec = match transport.as_str() {
        "stdio" => build_stdio(&name, raw, env)?,
        "http" => build_http(&name, raw, env)?,
        "" => {
            return Err(fail(
                &name,
                "missing transport",
                "add transport = \"stdio\" (child process) or transport = \
                 \"http\" (Streamable HTTP) to the server table",
            ));
        }
        other => {
            return Err(fail(
                &name,
                &format!("unknown transport \"{other}\""),
                "use transport = \"stdio\" or transport = \"http\"",
            ));
        }
    };

    Ok(ServerEntry {
        name,
        spec,
        timeout_secs,
        enabled,
    })
}

fn build_stdio(name: &str, raw: RawServer, env: EnvLookup<'_>) -> Result<ServerSpec> {
    if raw.url.is_some() {
        return Err(fail(
            name,
            "url is not valid for transport = \"stdio\"",
            "move url into an http server table, or drop it here",
        ));
    }
    let raw_command = raw.command.ok_or_else(|| {
        fail(
            name,
            "missing command",
            "add command = [\"<executable>\", \"<arg>\", ...] for stdio \
             servers",
        )
    })?;
    if raw_command.is_empty() {
        return Err(fail(
            name,
            "command must not be empty",
            "list the executable as command[0], e.g. command = [\"npx\", \
             \"-y\", \"<server>\"]",
        ));
    }
    if raw_command[0].ends_with(".cmd") || raw_command[0].ends_with(".bat") {
        return Err(fail(
            name,
            &format!("command[0] '{}' is a .cmd/.bat shim", raw_command[0]),
            "shims cannot be spawned directly on Windows; wrap with \
             command = [\"cmd\", \"/c\", \"<shim>\", ...] or point at the \
             real executable",
        ));
    }
    let mut command = Vec::with_capacity(raw_command.len());
    for arg in raw_command {
        command.push(expand_with(env, &arg).map_err(|e| {
            McpError::config(format!("{e} (while expanding command of server '{name}')"))
        })?);
    }
    let mut expanded_env = BTreeMap::new();
    for (key, value) in raw.env.unwrap_or_default() {
        expanded_env.insert(
            key,
            expand_with(env, &value).map_err(|e| {
                McpError::config(format!("{e} (while expanding env of server '{name}')"))
            })?,
        );
    }
    Ok(ServerSpec::Stdio {
        command,
        env: expanded_env,
        cwd: raw.cwd,
    })
}

fn build_http(name: &str, raw: RawServer, env: EnvLookup<'_>) -> Result<ServerSpec> {
    if raw.command.is_some() {
        return Err(fail(
            name,
            "command is not valid for transport = \"http\"",
            "move command into a stdio server table, or drop it here",
        ));
    }
    let raw_url = raw.url.ok_or_else(|| {
        fail(
            name,
            "missing url",
            "add url = \"http://<host>:<port>/<path>\" for http servers",
        )
    })?;
    let expanded_url = expand_with(env, &raw_url)
        .map_err(|e| McpError::config(format!("{e} (while expanding url of server '{name}')")))?;
    let parsed = url::Url::parse(&expanded_url).map_err(|e| {
        fail(
            name,
            &format!("invalid url '{expanded_url}': {e}"),
            "fix the url value",
        )
    })?;
    match parsed.scheme() {
        "http" => {}
        "https" => {
            return Err(fail(
                name,
                "https:// servers are not supported in 1.0 (the HTTP \
                 client ships without TLS)",
                "use a plain-http endpoint, or wait for the additive TLS \
                 feature",
            ));
        }
        other => {
            return Err(fail(
                name,
                &format!("unsupported url scheme '{other}:'"),
                "only http:// endpoints are accepted",
            ));
        }
    }
    let mut headers = BTreeMap::new();
    for (key, value) in raw.headers.unwrap_or_default() {
        if !is_single_var_reference(&value) {
            return Err(fail(
                name,
                &format!(
                    "header '{key}' must be a single ${{VAR}} \
                          reference, not a literal"
                ),
                "keep secrets out of config.toml: set them in the \
                 environment and reference them, e.g. Authorization = \
                 \"${MY_TOKEN}\"",
            ));
        }
        headers.insert(
            key,
            expand_with(env, &value).map_err(|e| {
                McpError::config(format!("{e} (while expanding headers of server '{name}')"))
            })?,
        );
    }
    Ok(ServerSpec::Http {
        url: parsed,
        headers,
    })
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

    fn load(text: &str) -> Result<McpConfig> {
        McpConfig::load_from_str(text, &env_from(&[("TOKEN", "tok"), ("PORT", "8080")]))
    }

    fn full_sample() -> String {
        r#"
[mcp.servers.filesystem]
transport = "stdio"
command = ["npx", "-y", "@modelcontextprotocol/server-filesystem"]
env = { NODE_ENV = "production", KEY = "${TOKEN}" }
cwd = "/tmp/ws"
timeout_secs = 30

[mcp.servers.docs]
transport = "http"
url = "http://127.0.0.1:${PORT}/mcp"
headers = { Authorization = "${TOKEN}" }
"#
        .to_string()
    }

    #[test]
    fn parses_stdio_and_http_entries() {
        let cfg = load(&full_sample()).unwrap();
        assert_eq!(cfg.servers.len(), 2);
        assert_eq!(cfg.enabled_names(), vec!["docs", "filesystem"]);

        let fs = &cfg.servers[1];
        assert_eq!(fs.name, "filesystem");
        assert_eq!(fs.timeout_secs, 30);
        let ServerSpec::Stdio { command, env, cwd } = &fs.spec else {
            panic!("expected stdio");
        };
        assert_eq!(command[0], "npx");
        assert_eq!(env.get("KEY").unwrap(), "tok");
        assert_eq!(cwd.as_deref(), Some(Path::new("/tmp/ws")));

        let docs = &cfg.servers[0];
        let ServerSpec::Http { url, headers } = &docs.spec else {
            panic!("expected http");
        };
        assert_eq!(url.as_str(), "http://127.0.0.1:8080/mcp");
        assert_eq!(headers.get("Authorization").unwrap(), "tok");
    }

    #[test]
    fn defaults_and_disabled_entries() {
        let cfg = load(
            r#"
[mcp.servers.a]
transport = "stdio"
command = ["demo"]

[mcp.servers.b]
transport = "stdio"
command = ["demo"]
enabled = false
"#,
        )
        .unwrap();
        assert_eq!(cfg.servers[0].timeout_secs, DEFAULT_TIMEOUT_SECS);
        assert!(cfg.servers[0].enabled);
        assert_eq!(cfg.enabled_names(), vec!["a"]);
    }

    #[test]
    fn missing_section_and_missing_file_yield_empty() {
        assert!(load("").unwrap().servers.is_empty());
        assert!(load("[provider]\ndefault = \"mock\"\n")
            .unwrap()
            .servers
            .is_empty());
        let home = tempfile::tempdir().unwrap();
        assert!(McpConfig::load(home.path()).unwrap().servers.is_empty());
    }

    #[test]
    fn transport_and_required_field_errors() {
        let e = load("[mcp.servers.a]\ncommand = [\"x\"]").unwrap_err();
        assert!(e.to_string().contains("missing transport"));
        let e = load("[mcp.servers.a]\ntransport = \"stdio\"").unwrap_err();
        assert!(e.to_string().contains("missing command"));
        let e = load("[mcp.servers.a]\ntransport = \"http\"").unwrap_err();
        assert!(e.to_string().contains("missing url"));
        let e = load("[mcp.servers.a]\ntransport = \"ws\"\nurl = \"ws://x\"").unwrap_err();
        assert!(e.to_string().contains("unknown transport"));
    }

    #[test]
    fn cross_field_pollution_rejected() {
        let e =
            load("[mcp.servers.a]\ntransport = \"stdio\"\ncommand = [\"x\"]\nurl = \"http://x\"")
                .unwrap_err();
        assert!(e.to_string().contains("url is not valid"));
        let e =
            load("[mcp.servers.a]\ntransport = \"http\"\nurl = \"http://x\"\ncommand = [\"x\"]")
                .unwrap_err();
        assert!(e.to_string().contains("command is not valid"));
    }

    #[test]
    fn http_url_rules() {
        let e = load("[mcp.servers.a]\ntransport = \"http\"\nurl = \"https://x/mcp\"").unwrap_err();
        assert!(e.to_string().contains("https:// servers are not supported"));
        let e = load("[mcp.servers.a]\ntransport = \"http\"\nurl = \"ftp://x\"").unwrap_err();
        assert!(e.to_string().contains("unsupported url scheme"));
        let e = load("[mcp.servers.a]\ntransport = \"http\"\nurl = \"not a url\"").unwrap_err();
        assert!(e.to_string().contains("invalid url"));
        let cfg =
            load("[mcp.servers.a]\ntransport = \"http\"\nurl = \"http://127.0.0.1:1/\"").unwrap();
        assert!(matches!(cfg.servers[0].spec, ServerSpec::Http { .. }));
    }

    #[test]
    fn header_rules_fail_closed() {
        let e = load(
            "[mcp.servers.a]\ntransport = \"http\"\nurl = \"http://x/\"\nheaders = { Authorization = \"Bearer plain\" }",
        )
        .unwrap_err();
        assert!(e.to_string().contains("must be a single"));
        let e = load(
            "[mcp.servers.a]\ntransport = \"http\"\nurl = \"http://x/\"\nheaders = { Authorization = \"${MISSING_VAR}\" }",
        )
        .unwrap_err();
        assert!(e.to_string().contains("MISSING_VAR"));
    }

    #[test]
    fn stdio_command_rules() {
        let e = load("[mcp.servers.a]\ntransport = \"stdio\"\ncommand = []").unwrap_err();
        assert!(e.to_string().contains("must not be empty"));
        let e =
            load("[mcp.servers.a]\ntransport = \"stdio\"\ncommand = [\"npx.cmd\", \"-y\", \"x\"]")
                .unwrap_err();
        assert!(e.to_string().contains(".cmd/.bat shim"));
        let e = load("[mcp.servers.a]\ntransport = \"stdio\"\ncommand = [\"run\", \"${MISSING}\"]")
            .unwrap_err();
        assert!(e.to_string().contains("MISSING"));
    }

    #[test]
    fn name_and_timeout_rules() {
        let e = load("[mcp.servers.\"bad name\"]\ntransport = \"stdio\"\ncommand = [\"x\"]")
            .unwrap_err();
        assert!(e.to_string().contains("server name"));
        let e = load("[mcp.servers.a]\ntransport = \"stdio\"\ncommand = [\"x\"]\ntimeout_secs = 0")
            .unwrap_err();
        assert!(e.to_string().contains("out of range"));
        let e =
            load("[mcp.servers.a]\ntransport = \"stdio\"\ncommand = [\"x\"]\ntimeout_secs = 99999")
                .unwrap_err();
        assert!(e.to_string().contains("out of range"));
    }
}
