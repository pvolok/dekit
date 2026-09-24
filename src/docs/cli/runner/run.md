---
title: dekit runner run
cli: dekit runner run
order: 6
related: [cli/runner, cli/runner/start]
---

Runs a runner in the foreground for the given root. This is the entry point
a client uses when it spawns a runner in the background; run it yourself to
watch a runner's log on the terminal. `--log-level` takes off, error, warn,
info, debug, or trace.

:::usage

```sh
dekit runner run --dir . --log-level debug
```
