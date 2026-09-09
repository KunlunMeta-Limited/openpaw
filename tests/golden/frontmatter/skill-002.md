---
id: sk-release-check-002
name: release-check
version: 0.3.1
description: Pre-release verification checklist runner
state: draft
permissions:
  - fs:read
  - exec:run
tags:
  - release
depends_on:
  - bash-safety
---
# release-check

Runs fmt/clippy/test, then the line-budget assertion, and prints a signed
go/no-go summary.
