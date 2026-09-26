---
title: dekit rm
cli: dekit rm
order: 14
related: [cli/spawn, cli/kill, start/targets]
---

Removes the matching tasks from the runner, killing the ones that are
running. Tasks from `dekit.yaml` come back the next time the runner loads
it; tasks added with `spawn` or `run` are gone for good.

:::usage

```sh
dekit rm tmp/watch
dekit rm +dynamic
```
