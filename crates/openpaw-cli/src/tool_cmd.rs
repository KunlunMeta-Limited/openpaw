//! `openpaw tool list` — the tool host's registered tools.

use std::path::Path;

/// Subcommands of `openpaw tool`.
#[derive(Debug, clap::Subcommand)]
pub enum Command {
    /// Print the registered tool names (static registry, one per line).
    List,
}

/// `openpaw tool list` — registered tool names from the static registry.
///
/// Uses the registry's synchronous name listing (same specs the async
/// `list_tools` executor method returns).
pub fn run_list(workspace: &Path) -> i32 {
    let registry = openpaw_tools::ToolRegistry::new(workspace);
    let names = registry.tool_names();
    println!("registered tools ({}):", names.len());
    for name in &names {
        println!("  {name}");
    }
    0
}
