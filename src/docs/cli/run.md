---
title: dekit run
cli: dekit run
order: 13
related: [cli/spawn, cli/attach]
---

Runs a command as a task in the foreground. The task is added as with
|cli/spawn| and your terminal attaches to it. When the command exits, the
task is removed and `dekit run` exits with its status (128 plus the signal
number if a signal ended it). If you detach instead, the task keeps running
until `dekit rm <path>`.

:::usage

```sh
dekit run test -- cargo test
dekit run migrate --dep db -- ./bin/migrate
```
