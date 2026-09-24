---
title: dekit restart
cli: dekit restart
order: 11
related: [cli/start, cli/stop, cli/runner/restart]
---

Pins and bounces the matching tasks: each one that is running stops and
starts again, and one that was down comes up. To restart the runner itself
and reload `dekit.yaml`, use |cli/runner/restart|.

:::usage

```sh
dekit restart web
dekit restart +backend
```
