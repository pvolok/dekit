---
title: dekit runner
cli: dekit runner
order: 15
related: [start/runners, cli/runner/stop, cli/kernel]
---

A runner is the long-lived process that owns a project's tasks: one per
project root, plus an optional host runner for machine-wide tasks. Commands
that start tasks or attach start the runner if needed; these verbs manage
the runner itself. See |start/runners|.

:::usage

Most verbs, and |cli/down|, take an optional runner: `project` (the nearest
project), `host`, or a path to a project root starting with `/`, `~/`, or
`./`. Without one it is the runner for the current directory, or for `-C`.
