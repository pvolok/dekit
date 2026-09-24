---
title: dekit run
cli: dekit run
related: [cli/spawn, cli/attach, start/agents]
---

Runs a command as a task in the foreground: the task is added as
|cli/spawn| would, your terminal attaches to it, and when the command
exits the task is removed and `dekit run` exits with its status (128 plus
the signal number when a signal ended it). Because the task runs under the
runner, its `--dep` dependencies come up first and its output is visible
to other clients while it runs. If you detach instead, the task keeps
running and `dekit rm <path>` removes it.

:::usage

```sh
dekit run migrate --dep db -- ./bin/migrate
dekit run test -- cargo test
```
