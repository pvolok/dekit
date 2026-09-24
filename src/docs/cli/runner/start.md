---
title: dekit runner start
cli: dekit runner start
order: 1
related: [cli/runner, cli/runner/stop, cli/runner/status]
---

Starts the selected runner without attaching to it, and prints the config
warnings it published while loading. A runner that is already running is
left alone. Tasks with `autostart: true` come up with the runner.

:::usage

```sh
dekit runner start
dekit runner start host
dekit runner start ~/dev/api
```
