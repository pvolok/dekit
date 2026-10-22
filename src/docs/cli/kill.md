---
title: dekit kill
cli: dekit kill
order: 9
related: [cli/stop, cli/rm, config/tasks]
---

Like |cli/stop|, but the task is killed at once (SIGKILL on Unix) instead of
getting its configured `stop` and a grace period. With `--wait` it returns
once the tasks are down (|cli/wait#down|).

:::usage

```sh
dekit kill web
```
