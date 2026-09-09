## Summary

<!-- What does this PR change and why? One or two sentences. -->

## Self-check list

- [ ] `cargo test --workspace` green
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` clean
- [ ] `cargo fmt --all -- --check` clean
- [ ] `cargo xtask lines` — budgets respected (no per-crate cap exceeded)
- [ ] `cargo xtask scrub --check` — no banned words
- [ ] `cargo xtask stability` — all public items still tagged
- [ ] `cargo xtask smoke-offline` — offline path still green
- [ ] If trait signatures changed: `cargo xtask snapshot` re-run and committed
- [ ] No telemetry, no new network calls (or justification included)
- [ ] Commits carry `Signed-off-by` (DCO)

## Change type

<!-- Post-M6: bugfix / feature (additive) / docs / RFC-required (breaking or MSRV) -->

