---
title: dekit attach
cli: dekit attach
related: [cli, config/user, cli/screen]
---

Attaches your terminal to a task's screen. With no target that is the
runner's console, `@dekit/console`: the task list beside the selected
task's live terminal, driven by the keys in |config/user|. A task path
attaches straight to that task's screen, with no console in between.
`dekit` with no command does the same as `dekit attach`.

The runner is started if it is not running; `--no-start` fails instead.
In the console, `q` detaches and leaves everything running, and `Q` kills
every task and stops the runner. Several terminals may attach at once:
they share one view, rendered at the smallest of their sizes.

:::usage

```sh
dekit attach
dekit attach web
dekit attach --no-start
dekit attach host::@dekit/console
```
