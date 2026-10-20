# Changelog

## Unreleased — pre-1.0 T0 integration baseline

- **Breaking (Rust types):** replace archived `serde_yml` with the maintained
  `serde_norway` package, retaining `serde_yml` as the workspace dependency alias.
  Public YAML `Mapping`, `Value`, and `Error` types exposed by Store, Memory,
  Skill, Goal, and Reflect now have the new package's type identity. Downstream
  Rust consumers passing these types must use the same package alias or migrate
  to the new types. See [migration and rollback](docs/engine-host/dependency-baseline.md).
  Existing frontmatter and session data remain readable; no bulk migration.
- Upgrade vulnerable LRU/TLS dependencies. Propose exact existing-package
  license exceptions with complete distribution notices, subject to PR review.
- Freeze Office/OpenCode test contracts and the T0–T8 handoff process; extend
  native platform CI and retire numeric line limits. The opt-in `engine-host`
  dependency is wired, but its runtime and protocol endpoints are not implemented.
