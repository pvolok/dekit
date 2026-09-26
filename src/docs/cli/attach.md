---
title: dekit attach
cli: dekit attach
order: 1
related: [cli, config/user, cli/screen]
---

Attaches your terminal to a task's screen. With no target that is the
console, `@dekit/console`: the task list next to the selected task's
terminal. With a task path you get that task's screen alone.

The runner is started if it is not running; `--no-start` fails instead. In
the console, `q` detaches and leaves everything running, and `Q` stops the
runner like |cli/down|. Other keys are in |config/user|.

:::usage

```sh
dekit attach
dekit attach web
dekit attach host::@dekit/console
```
