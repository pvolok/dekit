---
title: dekit up
cli: dekit up
related: [cli, cli/down, cli/start, config]
order: 2
---

The workday start verb. It starts the project's runner if it is not
running and then starts every task with `autostart: true` (|config|).
Dependencies come up first, and a dependency with a `ready_log` is waited
on before its dependents start.

A runner that starts after |cli/down| first brings back what it had: the
tasks from the current `dekit.yaml` and tasks added with `spawn`. The ones
you had started run again; the rest keep their last screen. So `up` restores
yesterday's state and adds the autostart set, including an autostart task
you stopped before `down`. To start other tasks, use |cli/start|.

The runner keeps going after you close the terminal; `dekit attach` gets
you back in.

:::usage

```sh
dekit up
dekit up -C ~/src/api
```
