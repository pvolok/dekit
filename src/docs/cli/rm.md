---
title: dekit rm
cli: dekit rm
order: 14
related: [cli/spawn, cli/kill, start/targets]
---

Removes the matching tasks from the runner, killing the ones that are
running. A path or glob that names a folder or a group removes it with
every task and folder in it (|config/tasks#folders|). Tasks from
`dekit.yaml` come back the next time the runner loads it; tasks added
with `spawn` or `run` are gone for good.

:::usage

```sh
dekit rm tmp/watch
dekit rm apps
dekit rm +dynamic
```
