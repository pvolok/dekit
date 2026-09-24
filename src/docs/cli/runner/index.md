---
title: dekit runner
cli: dekit runner
related: [start/runners, cli/runner/stop, cli/kernel]
---

A runner is the long-lived process that owns a project's tasks: one per
project root, plus the optional host runner for machine-wide tasks. Every
other command starts the project's runner on demand; these verbs manage the
runner itself. |start/runners| explains how a runner is found and where its
records live.

:::usage

Most verbs take an optional runner reference: `project` (the nearest
project), `host`, or a path to a project root. Without one, the runner is
the one discovered from the current directory, or from `-C`.
