# Contributing to OpenPaw

Thank you for considering a contribution.

## Developer Certificate of Origin (DCO)

All contributions must be signed off under the DCO. By signing off you
certify that you wrote the change or have the right to submit it under the
project license. Add a sign-off line to each commit message:

```
Signed-off-by: Your Name <you@example.com>
```

Or use `git commit -s`.

## Ground rules

- **No telemetry, ever.** PRs adding network calls must pass the network
  audit review; the binary must stay fully functional offline.
- **Line counts are informational.** `cargo xtask lines` reports AST-based
  statistics without numeric caps. Review reuse, responsibilities, dependency
  direction and tests; do not move or duplicate code to satisfy a size metric.
- **`unsafe` is forbidden** workspace-wide, with a single crate-level
  exemption: `openpaw-sandbox`, whose OS-FFI backends (restricted tokens,
  job objects) are the only sanctioned `unsafe` (every block carries a
  SAFETY comment; see the deviation register D-9 in `docs/devlog.md`).
- **Porting hygiene.** Ported code must be free of upstream-internal
  references; `cargo xtask scrub --check` runs in CI.

## CI platforms

The canonical repository currently lives on Gitee, which does not run
GitHub Actions. The workflows under `.github/workflows/` take effect on a
GitHub mirror; until one is wired up, run the PR self-check list locally
(see the pull-request template) — it mirrors the CI gate set.

## Workflow

1. Fork, create a topic branch.
2. Make your change with tests (`cargo test` green, `cargo clippy --workspace --all-targets` warning-free, `cargo fmt` clean).
3. Commit with DCO sign-off.
4. Open a PR using the template; tick the self-check list.

## Security

`SECURITY.md` and the governance/data-policy pages are planned with the
first public release batch (design plan §7.11/§7.12). Until they land,
report exploitable problems privately to the repository maintainers
(Gitee private message to the owner) — do not open public issues.
