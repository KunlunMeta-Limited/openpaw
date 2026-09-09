//! Exec tools: `bash` / `clipboard` / `sleep`, plus the [`Environment`]
//! abstraction the bash tool executes through — [`LocalEnvironment`] runs
//! directly on the host, [`SandboxedEnvironment`] delegates restricted and
//! isolated policies to the local sandbox crate.

pub mod bash;
pub mod clipboard;
pub mod environment;
pub mod sleep;

pub use environment::{
    create_environment, create_environment_from_policy, Environment, EnvironmentKind,
    EnvironmentResult, LocalEnvironment, SandboxPolicy, SandboxedEnvironment,
};
