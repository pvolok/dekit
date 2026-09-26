---
title: dekit runner start
cli: dekit runner start
order: 1
related: [cli/runner, cli/runner/stop, cli/runner/status]
---

Starts the selected runner without attaching, and prints any config
warnings. A runner that stopped with |cli/down| comes
back with its saved tasks, and the ones you had started run again;
otherwise the tasks with `autostart: true` start. A runner that is already
running is left alone.

:::usage

```sh
dekit runner start
dekit runner start host
dekit runner start ~/dev/api
```
