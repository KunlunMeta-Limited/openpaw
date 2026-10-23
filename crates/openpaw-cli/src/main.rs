//! openpaw — an open, evolvable personal AI agent.

mod chat;
mod hello;
#[cfg(feature = "sidecar")]
mod sidecar;

use std::path::PathBuf;

use clap::{CommandFactory, Parser};
#[cfg(feature = "sidecar")]
use openpaw_cli::cli::SidecarCommand;
use openpaw_cli::{
    completions_cmd, config_cmd, doctor, evolve_cmd, goal_cmd, init_cmd, memory_cmd, model_cmd,
    session_cmd, tool_cmd, Cli, Command,
};

/// Resolve the OpenPaw home from an optional `--home` override.
fn resolve_home(home: Option<PathBuf>) -> PathBuf {
    home.unwrap_or_else(openpaw_core::openpaw_home)
}

/// Default workspace for tool-host surfaces: the current directory.
fn default_workspace() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

fn main() {
    let cli = Cli::parse();
    // `--sidecar` short-circuits subcommands: the MCP server runs in-process.
    #[cfg(feature = "sidecar")]
    if cli.sidecar {
        #[cfg(feature = "engine-host")]
        if matches!(cli.command, Some(Command::EngineHost { .. })) {
            eprintln!("engine-host不能与--sidecar同时启动");
            std::process::exit(2);
        }
        let code = sidecar::run_server(&cli.sidecar_transport);
        std::process::exit(code);
    }
    let code = match cli.command {
        #[cfg(feature = "engine-host")]
        Some(Command::EngineHost { config, port }) => {
            openpaw_cli::host_cmd::run_with_env(&config, port, cli.env_file.as_deref())
        }
        Some(Command::Hello) => hello::run(),
        Some(Command::Chat {
            model,
            base_url,
            provider,
            workspace,
            home,
        }) => chat::run(chat::ChatArgs {
            env_file: cli.env_file,
            model,
            base_url,
            provider,
            workspace,
            home,
        }),
        Some(Command::Session { cmd }) => match cmd {
            session_cmd::Command::List { limit, home } => {
                session_cmd::run_list(&resolve_home(home), limit)
            }
            session_cmd::Command::Show { id, home } => {
                session_cmd::run_show(&resolve_home(home), &id)
            }
            session_cmd::Command::Search { query, limit, home } => {
                session_cmd::run_search(&resolve_home(home), &query, limit)
            }
        },
        Some(Command::Memory { cmd }) => match cmd {
            memory_cmd::Command::List { home, project } => {
                memory_cmd::run_list(&resolve_home(home), project.as_deref())
            }
            memory_cmd::Command::Search {
                query,
                limit,
                home,
                project,
            } => memory_cmd::run_search(&resolve_home(home), project.as_deref(), &query, limit),
            memory_cmd::Command::Show { id, home, project } => {
                memory_cmd::run_show(&resolve_home(home), project.as_deref(), &id)
            }
            memory_cmd::Command::Delete { id, home, project } => {
                memory_cmd::run_delete(&resolve_home(home), project.as_deref(), &id)
            }
        },
        Some(Command::Evolve { cmd }) => match cmd {
            evolve_cmd::Command::Status { home, project } => {
                evolve_cmd::run_status(&resolve_home(home), project.as_deref())
            }
            evolve_cmd::Command::Reset { home, project, yes } => {
                evolve_cmd::run_reset(&resolve_home(home), project.as_deref(), yes)
            }
        },
        Some(Command::Doctor { home, workspace }) => {
            let workspace = workspace.unwrap_or_else(default_workspace);
            doctor::run(&resolve_home(home), &workspace)
        }
        Some(Command::Goal { cmd }) => match cmd {
            goal_cmd::Command::New { objective, project } => {
                let goals_dir = match project {
                    Some(dir) => dir.join(".openpaw").join("goals"),
                    None => resolve_home(None).join("goals"),
                };
                goal_cmd::run_new(&goals_dir, &objective)
            }
            goal_cmd::Command::Status { home } => goal_cmd::run_status(&resolve_home(home)),
            goal_cmd::Command::Resume { id, home } => {
                goal_cmd::run_resume(&resolve_home(home), &id)
            }
        },
        Some(Command::Config { cmd }) => match cmd {
            config_cmd::Command::Show => {
                config_cmd::run_show_with_env(&resolve_home(None), cli.env_file.as_deref())
            }
            config_cmd::Command::Doctor { home, workspace } => {
                let workspace = workspace.unwrap_or_else(default_workspace);
                config_cmd::run_doctor(&resolve_home(home), &workspace)
            }
        },
        Some(Command::Model { cmd }) => match cmd {
            model_cmd::Command::List => model_cmd::run_list(),
            model_cmd::Command::Default { name } => model_cmd::run_default(&name),
        },
        Some(Command::Tool { cmd }) => match cmd {
            tool_cmd::Command::List { workspace } => {
                let workspace = workspace.unwrap_or_else(default_workspace);
                tool_cmd::run_list(&workspace)
            }
        },
        Some(Command::Completions { shell }) => completions_cmd::run(&shell),
        Some(Command::Init { home }) => init_cmd::run(&resolve_home(home)),
        #[cfg(feature = "sidecar")]
        Some(Command::Sidecar { cmd }) => match cmd {
            SidecarCommand::Status => sidecar::run_status_note(),
            SidecarCommand::Stop => sidecar::run_stop_note(),
        },
        None => {
            Cli::command().print_help().expect("print help");
            println!();
            0
        }
    };
    std::process::exit(code);
}
