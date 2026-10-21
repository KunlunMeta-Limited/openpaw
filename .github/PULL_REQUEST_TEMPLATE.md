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

- Tx / milestone M / integration base / previous approved-and-merged PR:
- Pre-PR milestone audit: checked revision/content hash, each required output, Linux/Windows evidence, unresolved items and conclusion (master §14.3):
- Pinned Office document/Schema section → master implementation anchor → actual code/test/evidence:
- [ ] Required milestone outputs, pre-PR native checks and human delivery acceptance passed before this PR was created (existing T0 #4 records its Draft revision history separately)
- Milestone state: pending / implementing / blocked / PR review / approved awaiting merge / completed; PR creation is not completion.
- After approval and merge: master §14 milestone row, main CHG history, real approved commit/merge SHA/date and handoff are synchronized. Pending events stay pending.
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

- Approved proposal: CHG version / exact Git object / reviewer / date / evidence (approved before implementation):
- Delivery acceptance: reviewer / accepted commit / date / evidence (accepted before PR creation):
- Explicit linked revisions and separate acceptance; overlapping-file responsibilities:
- Historical metadata or approved explanation migration, exact paths and protected original scope:
- Verification source (worktree or clean matching head) / stage / digest / actual argv:


- Main CHG / implementation authorization / REQ / DES:
- Exact files and necessity; reused / extended / fixed / new:
- Affected documentation and synchronized sections (or specific reasons):
- D regression / Office Rx approved version and evidence (if applicable):
- Full verification report, exact head or dirty hash; Linux / Windows native:
- Reviewer decision and merge SHA: pending until real review.

Initial Tx serialization applies only until T0–T8 agreed scope is accepted.
After that, use normal CHG development and PR review. Office bugs, necessary
extensions and new features all need explicit Rx approval before any file edit.
