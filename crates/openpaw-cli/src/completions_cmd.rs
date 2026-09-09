//! `openpaw completions <shell>` — shell-completion generation status.
//!
//! `clap_complete` is not a dependency yet, so this is a placeholder:
//! it prints the status and exits 0 without writing any file.

/// `openpaw completions <shell>` — placeholder notice, exit 0.
pub fn run(shell: &str) -> i32 {
    println!("completion generation for `{shell}` requires the clap_complete feature (M5b).");
    println!(
        "next: re-run this command after clap_complete is wired; no completion \
         script was written."
    );
    0
}
