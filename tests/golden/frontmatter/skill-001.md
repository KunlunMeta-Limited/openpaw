---
id: sk-bash-safety-001
name: bash-safety
version: 1.2.0
description: Guard destructive bash usage in CI pipelines
state: active
permissions:
  - exec:read
tags:
  - bash
  - safety
---
# bash-safety

Wraps pipeline commands with a readonly/destructive pre-check and blocks
`rm -rf /` class patterns before execution.
