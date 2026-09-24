---
title: dekit kill
cli: dekit kill
order: 9
related: [cli/stop, cli/rm, config/tasks]
---

Like |cli/stop|, but the task is killed at once instead of getting its
configured `stop` and the grace period: SIGKILL to the process group on
Unix.

:::usage

```sh
dekit kill web
```
