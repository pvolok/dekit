---
title: dekit up
cli: dekit up
related: [cli, start/targets, config]
order: 2
---

`dekit up` is the workday start verb. With no target it starts every task
tagged `autostart` (the ones with `autostart: true` in |config|), pulling
dependencies up first and waiting on each `ready_log` before dependents
start.

:::callout note
If the runner for this project is not running, `up` starts it. Close the
terminal: the runner keeps going. Attach later with `dekit attach`.
:::

:::usage

```sh
dekit up
dekit up +ci
dekit up 'services/*'
dekit up -C ~/src/api
```

## What it actually does {#semantics}

Bare `up` is `dekit start +autostart`. With a target it is `dekit start`
with that target: the matching tasks are pinned and started, and every
dependency they need comes up with them. Surgical control of single tasks is
the same verb, `dekit start`.

:::callout warning
`up` is not `run`. `dekit run <path> -- <cmd>` runs a fresh command in the
foreground and removes it when it exits. `up` is for the long-lived graph.
:::

## See also

Stop versus down versus veto is on |start/targets|. Config for `autostart`
and `ready_log` is on |config|. Bare `down` unpins every task; it does
**not** stop the runner, which is `dekit runner stop`.
