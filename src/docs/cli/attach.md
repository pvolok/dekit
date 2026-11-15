---
title: dekit attach
cli: dekit attach
order: 1
related: [cli, config/user, cli/screen]
---

Attaches your terminal to a task's screen. With no target it opens the
console: the task list next to the selected task's terminal. With a task
path you get that task's screen alone.

The console lists the tasks of every runner on this machine, each runner's
under a group named after its project (`host` for the host runner): the
runner it was opened on first and open, the others closed, and one that
starts later is added as it comes. A runner that stops leaves the list;
the console closes when the last one does. `dekit .` (|cli|) opens a
console of the one runner instead: its tasks at the top of the list, with
no other runners and no project picker.

Each runner's tasks are in the order of its `dekit.yaml`, as |cli/ls|
prints them. A task like `web/dev` sits under a `web/` group, placed where
the first of its tasks comes and shown as `web/…` while collapsed. A
group's row shows how many of its tasks are up; `<h>` and `<l>` collapse
and expand it, as does a click, and starting, stopping, or restarting it
acts on every task in it. A runner's group does the same for all of its
tasks.

The runner is started if it is not running, with no tasks (|cli/up|
starts them); `--no-start` fails instead. In the console, `q` asks how
to quit: `q` again stops the selected task's runner like |cli/down|,
`x` stops it without saving like |cli/runner/stop|, and `d` leaves
everything running; each closes the console. `Q` stops the runner
without asking. The other keys are in |config/user|.

The console runs in `dekit`, not in the runner: it talks to each runner
as any client does. A runner older than the console is left out of the
list; |cli/runner/upgrade| brings it up to date, and the console opened
on one asks for that instead of opening.

:::usage

```sh
dekit attach
dekit attach web
dekit attach --no-start
```
