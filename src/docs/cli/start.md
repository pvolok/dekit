---
title: dekit start
cli: dekit start
order: 7
related: [cli/up, cli/stop, start/targets]
---

Pins and starts the matching tasks, bringing their dependencies up first;
a dependency with a `ready_log` is waited on before dependents start. A
vetoed task is released. The reply is the number of tasks that matched;
zero is not an error. |cli/up| is `start +autostart`.

:::usage

```sh
dekit start web
dekit start +workers
dekit start 'services/*'
```
