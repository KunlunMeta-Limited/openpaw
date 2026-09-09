---
id: mem-7f3a2b9c
created: 2026-08-20T09:30:00Z
source: reflection
confidence: medium
pinned: false
tags:
  - rust
  - build
zone: general
description: Builds are offline against a vendored dependency tree
memory_type: fact
sm2_repetition: 1
sm2_ease_factor: 2.5
sm2_interval_days: 1
sm2_next_review: 2026-08-21
---
The workspace builds offline: a vendor directory plus a native-deps overlay
supply everything. Do not add crates.io-only dependencies without checking
the vendoring list first.
