---
title: dekit runner upgrade
cli: dekit runner upgrade
order: 7
related: [cli/runner, cli/kernel, config/kernel]
---

Replaces the running runner with another dekit binary without stopping any
task; attached terminals stay attached. The default is the project's
selected kernel (|config/kernel|); `--binary` names one. Not available on
Windows.

:::usage

```sh
dekit runner upgrade
dekit runner upgrade --binary ~/.cargo/bin/dekit
```
