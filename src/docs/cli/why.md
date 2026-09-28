---
title: dekit why
cli: dekit why
order: 5
related: [cli/ls, start/targets]
---

Explains why one task is or is not running: its state, whether something
wants it running, whether it is pinned or vetoed, who depends on it,
restart attempts, and the state of each dependency. The state shows how
the last run ended, as in |cli/ls|: `backoff (not ready in time)` is a
task that failed to start and waits to start again. The target must match
exactly one task.

:::usage

```sh
dekit why web
dekit why web --json
```
