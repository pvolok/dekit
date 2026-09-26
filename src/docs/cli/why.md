---
title: dekit why
cli: dekit why
order: 5
related: [cli/ls, start/targets]
---

Explains why one task is or is not running: its state, whether something
wants it up, whether it is pinned or vetoed, who depends on it, restart
attempts, and the state of each dependency. The target must match exactly
one task.

:::usage

```sh
dekit why web
dekit why web --json
```
