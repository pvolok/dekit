---
title: dekit runner upgrade
cli: dekit runner upgrade
related: [cli/runner, cli/kernel, config/kernel]
---

Replaces the running runner with another binary without stopping any task:
attached clients stay attached and no process restarts. The default is the
kernel selected for the project (|config/kernel|); `--binary` names one
explicitly. The reply reports the version that took over. Not available on
Windows.

:::usage

```sh
dekit runner upgrade
dekit runner upgrade --binary ~/.cargo/bin/dekit
```
