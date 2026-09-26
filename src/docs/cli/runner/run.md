---
title: dekit runner run
cli: dekit runner run
order: 6
related: [cli/runner, cli/runner/start]
---

Runs a runner in the foreground for the given root. Clients use it to start
a runner in the background; run it yourself to debug one. The log goes to
`dekit.log` in the root unless the `log` key in |config| says otherwise.
`--log-level` takes off, error, warn, info, debug, or trace.

:::usage

```sh
dekit runner run --dir . --log-level debug
```
