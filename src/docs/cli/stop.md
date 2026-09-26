---
title: dekit stop
cli: dekit stop
order: 8
related: [cli/kill, cli/veto, cli/down, config/tasks]
---

Unpins and stops the matching tasks now. A task that a pinned task still
depends on starts again right away; see |start/targets| for `veto` and
`kill`. The task gets its configured `stop` (|config/tasks|) and is killed
if it has not exited after a grace period. `dekit stop '**'` stops every
task and keeps the runner; |cli/down| stops the runner too.

:::usage

```sh
dekit stop web
dekit stop +workers
dekit stop '**'
```
