---
id: mem-4f7a9d02
created: '2026-08-28T14:05:00Z'
source: reflection
confidence: low
pinned: false
tags:
- debugging
- cot
zone: thinking-paths
description: Reusable debugging thinking path distilled from a reflect run
memory_type: reasoning
sm2_repetition: 0
sm2_ease_factor: 2.5
sm2_interval_days: 0
sm2_next_review: '2026-08-29'
unverified: true
thinking_path: reproduce → isolate → hypothesize → test → conclude
concrete_instance: Reproduced the panic by re-running the failing command
problem_type: debugging
---
Debug by reproducing first: rerun the failing command, capture the exact panic,
isolate the smallest input that triggers it, then patch and re-run.
