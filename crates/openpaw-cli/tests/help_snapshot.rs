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

// 默认十二个入口固定；可选项与CLI使用相同feature，四种组合都精确核对，不能只检查下限。
const SUBCOMMANDS: &[&str] = &[
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
    #[cfg(feature = "engine-host")]
    "engine-host",
    #[cfg(feature = "engine-host")]
    "engine-host-workspace",
    #[cfg(feature = "sidecar")]
    "sidecar",
];

#[test]
fn root_help_lists_enabled_subcommands_and_version() {
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
fn top_level_subcommands_match_exact_feature_surface() {
    let mut cmd: Command = Cli::command();
    cmd.build();
    // clap的内建help不属于业务入口；默认十二项加每个已启用可选入口。
    let managed: Vec<&str> = cmd
        .get_subcommands()
        .map(|c| c.get_name())
        .filter(|n| *n != "help")
        .collect();
    assert_eq!(
        SUBCOMMANDS.len(),
        12 + 2 * usize::from(cfg!(feature = "engine-host"))
            + usize::from(cfg!(feature = "sidecar")),
        "默认十二项不能因增加可选命令而改变"
    );
    assert_eq!(
        managed.len(),
        SUBCOMMANDS.len(),
        "expected {} managed subcommands, got: {managed:?}",
        SUBCOMMANDS.len()
    );
    for name in SUBCOMMANDS {
        assert!(
            managed.contains(name),
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
    #[cfg(feature = "engine-host")]
    {
        let help = sub_help("engine-host");
        for key in ["--config", "--port"] {
            assert!(
                help.contains(key),
                "`openpaw engine-host` help must contain `{key}`:\n{help}"
            );
        }
        let help = sub_help("engine-host-workspace");
        for key in ["plan", "apply"] {
            assert!(
                help.contains(key),
                "离线工作区迁移帮助必须包含{key}：{help}"
            );
        }
    }
    assert_eq!(
        cases.len(),
        SUBCOMMANDS.len()
            - usize::from(cfg!(feature = "sidecar"))
            - 2 * usize::from(cfg!(feature = "engine-host")),
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

#[test]
fn unfinished_commands_fail_instead_of_reporting_success() {
    use std::process::Command;
    for args in [
        vec!["model", "default", "echo"],
        vec!["completions", "bash"],
        vec!["completions", "not-a-shell"],
    ] {
        let result = Command::new(env!("CARGO_BIN_EXE_openpaw"))
            .args(args)
            .output()
            .unwrap();
        assert!(!result.status.success());
        assert!(
            result.stdout.is_empty(),
            "must not emit a fake configuration or completion script"
        );
        let error = String::from_utf8(result.stderr).unwrap();
        assert!(
            error.contains("error:") && error.contains("why:") && error.contains("next:"),
            "{error}"
        );
    }
}

#[test]
fn chat_help_states_automatic_approval_and_host_boundary() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_openpaw"))
        .args(["chat", "--help"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    for expected in [
        "automatically approved",
        "not an OS sandbox",
        "only to engine-host",
        "not advertised",
    ] {
        assert!(text.contains(expected), "{expected}: {text}");
    }
}
