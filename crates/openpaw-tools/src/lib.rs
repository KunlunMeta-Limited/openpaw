//! openpaw-tools — the tool system.
//!
//! Landed (M2, staged): the register-dispatch ToolHost skeleton, workspace
//! path safety, fs tools (read_file / write_file / edit_file / grep / glob /
//! notebook_edit), exec tools (bash / clipboard / sleep) with the bash AST
//! security analysis, web tools (web_fetch / web_search / web_extract +
//! html_to_md), and the meta tool set. Every tool follows the upstream
//! `spec() + run(workspace, args)` shape with a contract test and a failure
//! path test each.
//!
//! Pending (M2 remainder): the local sandbox backends. The upstream net
//! modules (client/cookies/robots/blocklist) are deferred — the web tools
//! carry their own SSRF/DNS-rebinding protection and have no consumer for
//! the net layer until a browser stack lands.

pub mod bashast;
pub mod exec;
pub mod fs;
pub mod host;
pub mod meta;
pub mod safety;
pub mod web;

pub use bashast::{analyze_command_safety, CommandSafety};
pub use host::{arc_registry, ToolRegistry};
