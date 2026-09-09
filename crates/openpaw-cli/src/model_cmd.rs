//! `openpaw model ...` — static provider/model table and the default
//! provider placeholder.

/// Print a three-segment error (what happened / why / what to do next).
fn report_error(what: String, why: &str, next: &str) {
    eprintln!("error: {what}");
    eprintln!("why: {why}");
    eprintln!("next: {next}");
}

/// The built-in provider list with default models (static table; matches
/// what `openpaw chat --provider` accepts and what openpaw-llm ships).
const PROVIDERS: [(&str, &str, &str); 4] = [
    (
        "anthropic",
        "claude-sonnet-4",
        "network; requires ANTHROPIC_API_KEY",
    ),
    ("openai", "gpt-4o", "network; requires OPENAI_API_KEY"),
    ("mock", "mock-1", "offline; deterministic counter replies"),
    ("echo", "echo-1", "offline; zero-config echo"),
];

/// Subcommands of `openpaw model`.
#[derive(Debug, clap::Subcommand)]
pub enum Command {
    /// Print the built-in providers and their default models.
    List,
    /// Record the default provider choice (placeholder: no file is
    /// written yet).
    Default {
        /// Provider name as shown by `openpaw model list`.
        name: String,
    },
}

/// `openpaw model list` — static provider table.
pub fn run_list() -> i32 {
    println!("{:<10} {:<18} notes", "provider", "default model");
    for (name, model, notes) in PROVIDERS {
        println!("{name:<10} {model:<18} {notes}");
    }
    0
}

/// `openpaw model default <name>` — accepts a known provider name and
/// prints the placeholder notice (config file persistence lands later).
pub fn run_default(name: &str) -> i32 {
    if !PROVIDERS.iter().any(|(known, _, _)| *known == name) {
        report_error(
            format!("unknown provider `{name}`"),
            "`model default` accepts one of anthropic, openai, mock, echo.",
            "re-run with one of those names, or run `openpaw model list`.",
        );
        return 1;
    }
    println!("default provider `{name}` accepted (config file persistence lands in M5b).");
    println!("why: no config file is read or written yet, so the choice is not persisted.");
    println!(
        "next: re-run after M5b to persist the default; `openpaw chat --provider` works today."
    );
    0
}
