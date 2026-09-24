---
title: dekit rm
cli: dekit rm
order: 14
related: [cli/spawn, cli/kill, start/targets]
---

Removes the matching tasks from the runner, killing the ones that are
running. Tasks from `dekit.yaml` come back the next time the config is
loaded; tasks added with `spawn` or `run` are gone for good. `dekit rm
+dynamic` clears every runtime-added task.

:::usage

```sh
dekit rm tmp/watch
dekit rm +dynamic
```
