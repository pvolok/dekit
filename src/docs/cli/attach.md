---
title: dekit attach
cli: dekit attach
order: 1
related: [cli, config/user, cli/screen]
---

Attaches your terminal to a task's screen. With no target that is the
console, `@dekit/console`: the task list next to the selected task's
terminal. With a task path you get that task's screen alone.

The task list is in the order of `dekit.yaml`, as |cli/ls| prints it. A
task like `web/dev` sits under a `web/` group, placed where the first of
its tasks comes and shown as `web/…` while collapsed. A group's row shows how
many of its tasks are up; `<h>` and `<l>` collapse and expand it, as does
a click, and starting, stopping, or restarting it acts on every task in
it.

The runner is started if it is not running, with no tasks (|cli/up|
starts them); `--no-start` fails instead. In the console, `q` asks how
to quit: `q` again stops the runner like |cli/down|, `x` stops it
without saving like |cli/runner/stop|, and `d` detaches and leaves
everything running. `Q` stops the runner like |cli/down| without
asking. Other keys are in |config/user|.

:::usage

```sh
dekit attach
dekit attach web
dekit attach host::@dekit/console
```
