# Feature regression entry point

Run `./tests/feature-regression/run.ps1` from the repository root on Windows.
It stops at the first failing category and reports its name and exit code.
T1 adds the Host bootstrap contract test; later Tn tasks add focused
cross-module journeys here only when crate tests cannot cover them.

```text
cargo test -p openpaw-engine-host --features engine-host --test contract
cargo test -p openpaw-cli
cargo test -p openpaw-engine
cargo test -p openpaw-store
cargo test -p openpaw-tools
cargo test -p openpaw-cli --features sidecar
```

Pass `-CargoConfig 'key=value'` when a local Cargo registry mirror is needed.
Record the exact commands, exit codes, and skipped environment-dependent tests
in each Tn PR. Keep default-feature isolation under `cargo tree` verification.
