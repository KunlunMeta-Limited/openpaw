---
name: dependency-audit
description: Audit dependency health after a Cargo.lock change
triggers:
- cargo audit
- dependency drift
status: active
priority: null
tags: []
author: null
category: null
pinned: false
source: reflect-lite
prerequisites: []
parent: null
difficulty: null
concepts: []
disable_model_invocation: false
user_invocable: true
unverified: true
---
Run `cargo audit` and diff Cargo.lock against the vendored list. Escalate any
new crates.io-only dependency to the user before adding it to the vendor set.
