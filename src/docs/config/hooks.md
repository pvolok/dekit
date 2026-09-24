---
title: Hooks
summary: on_init and on_idle run a command when the project loads and when its tasks go idle.
related: [config, config/tasks]
order: 40
---

Both hooks hold a command in the same shape the JavaScript API and the
RPC use: an object with a `command` verb and its fields.

```yaml
on_init: {command: start, target: +ci}
on_idle: {command: quit}
```

`on_init` runs once the runner has loaded the project. `on_idle` runs each
time the number of active tasks drops to zero; starting, running, ready,
stopping, and backing off all count as active, so a project whose last
task finishes fires it once. `{command: quit}` on idle stops the runner
when the work is done.

## Verbs

:::commands
- cmd: "{command: start, target}"
  desc: Pin and start the matching tasks and their dependencies.
- cmd: "{command: stop, target}"
  desc: Unpin and stop; a task restarts if a dependent still needs it.
- cmd: "{command: down, target}"
  desc: Unpin only.
- cmd: "{command: kill, target}"
  desc: Stop with an immediate hard kill.
- cmd: "{command: veto, target}"
  desc: Force down and hold down until started again.
- cmd: "{command: restart, target}"
  desc: Restart the matching tasks.
- cmd: "{command: force-restart, target}"
  desc: Restart even tasks that are not wanted.
- cmd: "{command: add, target, cmd | shell, cwd, env, deps, tags}"
  desc: Register a process task at an exact path and start it.
- cmd: "{command: remove, target}"
  desc: Remove the matching tasks, killing running ones.
- cmd: "{command: rename, target, name}"
  desc: Set a task's display label.
- cmd: "{command: duplicate, target, name}"
  desc: Copy a task under a new name.
- cmd: "{command: quit}"
  desc: Stop the runner.
- cmd: "{command: batch, commands}"
  desc: Run a list of commands in order.
:::

A `target` is written as in the CLI: a path, a glob, or a `+tag`
(|start/targets|).
