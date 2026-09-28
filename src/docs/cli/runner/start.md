---
title: dekit runner start
cli: dekit runner start
order: 1
related: [cli/runner, cli/runner/stop, cli/runner/status]
---

Starts the selected runner without attaching, and prints any config
warnings. It starts no tasks: a runner that stopped with |cli/down| comes
back with its saved tasks idle, with their screens, and |cli/up| starts
the ones you had started and the tasks with `autostart: true`. A runner
that is already running is left alone.

:::usage

```sh
dekit runner start
dekit runner start host
dekit runner start ~/dev/api
```
