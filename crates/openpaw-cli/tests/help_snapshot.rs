//! Help-text snapshot tests for the M5 subcommand surface.
//!
//! Snapshot style: assertions check that key names appear in the rendered
//! help (not full-text hashes), so a clap upgrade that only reshuffles
//! layout or wrapping does not break the suite.

use clap::{Command, CommandFactory};
use openpaw_cli::Cli;

/// Render the help text of one top-level subcommand.
fn sub_help(name: &str) -> String {
    let mut cmd = Cli::command();
    let sub = cmd
        .find_subcommand_mut(name)
        .unwrap_or_else(|| panic!("subcommand `{name}` must exist"));
    let mut buf = Vec::new();
    sub.write_help(&mut buf).expect("write help");
    String::from_utf8(buf).expect("help text is utf-8")
}

/// The managed top-level subcommands (all except the bare root). The
/// `sidecar` entry is feature-gated exactly like the CLI surface.
#[cfg(not(feature = "sidecar"))]
const SUBCOMMANDS: [&str; 12] = [
    "hello",
    "chat",
    "session",
    "memory",
    "evolve",
    "doctor",
    "goal",
    "config",
    "model",
    "tool",
    "completions",
    "init",
];
#[cfg(feature = "sidecar")]
const SUBCOMMANDS: [&str; 13] = [
    "hello",
    "chat",
    "session",
    "memory",
    "evolve",
    "doctor",
    "goal",
    "config",
    "model",
    "tool",
    "completions",
    "init",
    "sidecar",
];

#[test]
fn root_help_lists_all_twelve_subcommands_and_version() {
    let mut cmd = Cli::command();
    let mut buf = Vec::new();
    cmd.write_help(&mut buf).expect("write root help");
    let help = String::from_utf8(buf).expect("help text is utf-8");

    for name in SUBCOMMANDS {
        assert!(
            help.contains(name),
            "root help must mention `{name}`:\n{help}"
        );
    }
    // clap 4 renders the version through the version flag, not the help
    // body; the snapshot carries it via `render_version()`.
    let version = env!("CARGO_PKG_VERSION");
    let version_text: String = Cli::command().render_version();
    assert!(
        version_text.contains(version),
        "version output must carry `{version}`: {version_text}"
    );
    assert!(
        help.contains("Usage:"),
        "root help must render a usage block:\n{help}"
    );
}

#[test]
fn top_level_subcommand_count_is_twelve() {
    let mut cmd: Command = Cli::command();
    cmd.build();
    // clap injects a built-in `help` subcommand; the managed surface is
    // the twelve names above.
    let managed: Vec<&str> = cmd
        .get_subcommands()
        .map(|c| c.get_name())
        .filter(|n| *n != "help")
        .collect();
    assert_eq!(
        managed.len(),
        SUBCOMMANDS.len(),
        "expected {} managed subcommands, got: {managed:?}",
        SUBCOMMANDS.len()
    );
    for name in SUBCOMMANDS {
        assert!(
            managed.contains(&name),
            "subcommand `{name}` is missing from: {managed:?}"
        );
    }
}

#[test]
fn each_subcommand_help_carries_key_names() {
    // (subcommand, substrings that must appear in its rendered help).
    let cases: &[(&str, &[&str])] = &[
        ("hello", &["echo"]),
        ("chat", &["--provider", "--workspace", "--home"]),
        ("session", &["list", "show", "search"]),
        ("memory", &["list", "search", "show", "delete"]),
        ("evolve", &["status", "reset"]),
        ("doctor", &["--home", "--workspace"]),
        ("goal", &["new", "status", "resume"]),
        ("config", &["show", "doctor"]),
        ("model", &["list", "default"]),
        ("tool", &["list"]),
        ("completions", &["shell"]),
        ("init", &["--home"]),
    ];
    // The sidecar subcommand is informational (status/stop notes only); it
    // is asserted separately because its surface is feature-gated.
    #[cfg(feature = "sidecar")]
    {
        let help = sub_help("sidecar");
        for key in ["status", "stop"] {
            assert!(
                help.contains(key),
                "`openpaw sidecar` help must contain `{key}`:\n{help}"
            );
        }
    }
    assert_eq!(
        cases.len(),
        SUBCOMMANDS.len() - if cfg!(feature = "sidecar") { 1 } else { 0 },
        "the snapshot table must cover every non-gated subcommand"
    );
    for (name, keys) in cases {
        let help = sub_help(name);
        for key in *keys {
            assert!(
                help.contains(key),
                "`openpaw {name}` help must contain `{key}`:\n{help}"
            );
        }
    }
}

#[test]
fn goal_help_lists_all_three_subcommands() {
    let help = sub_help("goal");
    for key in ["new", "status", "resume"] {
        assert!(
            help.contains(key),
            "`goal` help must contain `{key}`:\n{help}"
        );
    }
}
