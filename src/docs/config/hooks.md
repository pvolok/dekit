---
title: Hooks
summary: on_init and on_idle run a command when the project loads and when its tasks go idle.
related: [config, config/tasks]
order: 40
---

A hook runs a command. `on_init` runs once, after the runner has loaded
the project. `on_idle` runs each time no task is left starting, running,
stopping, or waiting to restart.

```yaml
on_init: {command: start, target: +ci}
on_idle: {command: down}
```

This starts the tasks tagged `ci` and stops the runner when they are done.
A runner starts no task by itself; `on_init: {command: up}` makes every
start of the runner work like |cli/up|.

## Commands

A command is an object with a `command` name and its fields. `target` is
written as on the command line: a path, a glob, or a `+tag`
(|start/targets|).

:::commands
- cmd: "{command: up}"
  desc: Start what the last `down` saved as started and the `autostart` tasks, as |cli/up| does.
- cmd: "{command: start, target}"
  desc: Start the tasks and their dependencies.
- cmd: "{command: stop, target}"
  desc: Stop the tasks; a task comes back if another task still needs it.
- cmd: "{command: kill, target}"
  desc: Stop the tasks right away with a hard kill.
- cmd: "{command: veto, target}"
  desc: Stop the tasks and keep them stopped until started again.
- cmd: "{command: restart, target}"
  desc: Stop the tasks and start them again; stopped tasks start.
- cmd: "{command: force-restart, target}"
  desc: Like restart, but with a hard kill instead of a normal stop.
- cmd: "{command: add, target, cmd | script, label, cwd, env, deps, tags}"
  desc: Add a task at the path `target` and start it.
- cmd: "{command: remove, target}"
  desc: Remove the tasks, killing running ones.
- cmd: "{command: rename, target, name}"
  desc: Change the tasks' label.
- cmd: "{command: duplicate, target, name}"
  desc: Copy a task under a new name.
- cmd: "{command: down}"
  desc: Save the tasks for the next `up` and stop the runner, as |cli/down| does.
- cmd: "{command: quit}"
  desc: Stop the runner without saving, as |cli/runner/stop| does.
- cmd: "{command: batch, commands}"
  desc: Run a list of commands in order.
:::
