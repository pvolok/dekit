---
title: dekit kill
cli: dekit kill
order: 9
related: [cli/stop, cli/rm, config/tasks]
---

Like |cli/stop|, but the task is killed at once (SIGKILL on Unix) instead of
getting its configured `stop` and a grace period.

:::usage

```sh
dekit kill web
```
