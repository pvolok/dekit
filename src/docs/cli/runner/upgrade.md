---
title: dekit runner upgrade
cli: dekit runner upgrade
order: 7
related: [cli/runner, cli/update, cli/kernel, config/kernel]
---

Replaces the running runner with another dekit binary without stopping any
task; attached terminals stay attached. The default is the project's
selected kernel (|config/kernel|); `--binary` names one. Not available on
Windows.

`--all` is for after an update that replaced the binary you are running:
it upgrades every running runner that runs an older version of this
binary, and lists the runners on other binaries without touching them.
|cli/update| ends with it. On Windows it lists the runners that need a
restart.

:::usage

```sh
dekit runner upgrade
dekit runner upgrade --binary ~/.cargo/bin/dekit
dekit runner upgrade --all
```
