---
title: dekit veto
cli: dekit veto
order: 10
related: [cli/stop, cli/kill, cli/start, start/targets]
---

Stops the matching tasks and keeps them stopped, even when pinned tasks
depend on them. Those dependents stop too and wait. The veto ends when a
start reaches the task: `dekit start` on it or on a task that depends on
it, or `dekit up` when an autostart task needs it.

:::usage

```sh
dekit veto db
dekit start db
```
