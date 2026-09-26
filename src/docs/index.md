---
title: What is dekit
summary: A process manager that runs your project's tasks in development and in production.
related: [start/getting-started, start/runners, start/from-mprocs]
---

**dekit is a process manager.** It runs your project's tasks, such as
servers, databases and workers, in development and in production.

- Define your project's tasks in a config file (|config|)
- dekit handles dependencies, crashes and restarts (|config/tasks|)
- Watch and control tasks in a terminal UI (|cli/attach|)
- A full CLI for humans and agents (|cli|)
- Built-in JavaScript for writing scripts (|js|)

## How it works

A **runner** owns the project's tasks. It is a separate process, like a tmux
server: close the terminal and the tasks keep running (|start/runners|). The
CLI, the TUI and scripts all talk to the same runner, so they all see the
same state.

dekit is the next version of mprocs and still runs `mprocs.yaml` through
`dekit mprocs` (|start/from-mprocs|).

:::footnote
The same pages are in the terminal: `dekit help <topic>`.
:::
