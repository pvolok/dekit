---
title: dekit restart
cli: dekit restart
order: 11
related: [cli/start, cli/stop, cli/runner/restart]
---

Pins the matching tasks and restarts them: a running task stops and starts
again, a stopped one starts. To reload `dekit.yaml`, restart the runner with
|cli/runner/restart|.

With `--wait` it returns once the new run is ready, and fails when it
can't get there, with the end of its screen (|cli/wait|): the old run
being ready doesn't count.

:::usage

```sh
dekit restart web
dekit restart api --wait --wait-timeout 90s
dekit restart +backend
```
