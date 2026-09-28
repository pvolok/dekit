---
title: dekit up
cli: dekit up
related: [cli, cli/down, cli/start, config]
order: 2
---

The workday start verb, the pair of |cli/down|. It starts the project's
runner if it is not running, then starts the tasks you had started when
`down` saved them, and every task with `autostart: true` (|config|).
Dependencies come up first, and a dependency with a `ready` check is waited
on before its dependents start.

`up` starts what is not running: tasks that are idle, stopped, or crashed.
Running tasks and jobs that finished are left alone, so a second `up`
changes nothing. The tasks saved by `down` are started once: stop one
after `up` and the next `up` leaves it stopped, unless it autostarts.

Only `up` starts tasks by itself. A runner started any other way (`dekit
attach`, `dekit start web`, |cli/runner/start|) shows the saved tasks
idle, with their last screens, and starts only what it was asked to.

The runner keeps going after you close the terminal; `dekit attach` gets
you back in. `up` takes a runner like `down`: `host`, `project`, or a path
to a project root (|cli/runner|).

:::usage

```sh
dekit up
dekit up -C ~/src/api
dekit up host
```
