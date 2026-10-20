## Summary

<!-- What does this PR change and why? One or two sentences. -->

## Self-check list

- [ ] `cargo test --locked --workspace` and relevant feature tests recorded
- [ ] `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings` clean
- [ ] `cargo fmt --all -- --check` clean
- [ ] `cargo run -p xtask -- lines` — informational statistics attached (no size cap)
- [ ] `cargo xtask scrub --check` — no banned words
- [ ] `cargo xtask stability` — all public items still tagged
- [ ] `cargo xtask smoke-offline` — offline path still green
- [ ] If trait signatures changed: `cargo xtask snapshot` re-run and committed
- [ ] No telemetry, no new network calls (or justification included)
- [ ] Commits carry `Signed-off-by` (DCO)

## Change type

<!-- Post-M6: bugfix / feature (additive) / docs / RFC-required (breaking or MSRV) -->

## Engine Host Tx handoff (required for T0–T8)

- Tx / integration base / previous approved-and-merged PR:
- Outputs and behavior before/after:
- Reused / extended / fixed / new; why new code is necessary:
- Defect IDs / Office Rx approval and linked cross-repository PR:
- Public interface / contract / data migration and rollback:

| OS and toolchain | Command | Exit code | Evidence / not run and why |
| --- | --- | --- | --- |
| Linux | | | |
| Windows native | | | |

- [ ] Contract/type, build, unit, runtime and browser results are distinguished
- [ ] Missing or skipped required checks are not reported as passing
- [ ] Next Tx interfaces, fixtures and remaining blockers are documented
- Reviewer / approved commit / merge SHA: pending until explicitly reviewed

Author approval and green CI do not approve a Tx. Start the next Tx only after
the current PR is approved by the user/designated reviewer and merged. See
docs/engine-host/handoff.md. Office R1 requires its own design approval.


## CHG / requirements / design (all changes)

- Main CHG / implementation authorization / REQ / DES:
- Exact files and necessity; reused / extended / fixed / new:
- Affected documentation and synchronized sections (or specific reasons):
- D regression / Office Rx approved version and evidence (if applicable):
- Full verification report, exact head or dirty hash; Linux / Windows native:
- Reviewer decision and merge SHA: pending until real review.

Initial Tx serialization applies only until T0–T8 agreed scope is accepted.
After that, use normal CHG development and PR review. Office bugs, necessary
extensions and new features all need explicit Rx approval before any file edit.
