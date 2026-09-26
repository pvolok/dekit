---
title: dekit restart
cli: dekit restart
order: 11
related: [cli/start, cli/stop, cli/runner/restart]
---

Pins the matching tasks and restarts them: a running task stops and starts
again, a stopped one starts. To reload `dekit.yaml`, restart the runner with
|cli/runner/restart|.

:::usage

```sh
dekit restart web
dekit restart +backend
```
