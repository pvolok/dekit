---
title: dekit docs
summary: dekit is a process manager for dev and ops. These pages cover the CLI, dekit.yaml, and the runner.
related: [start/getting-started, cli, config]
---

dekit runs the processes of a project. Declare tasks in `dekit.yaml`, and a
long-lived runner brings them up in dependency order, waits until each one is
ready, and keeps them healthy while you work.

Drive it from the |cli|, from the TUI (`dekit attach`), or from a JS script.
These pages are the same topics `dekit help` prints in the terminal.

:::callout tip
New here? Start with |start/getting-started|. Coming back for a flag or a
config key? |cli| and |config|.
:::

## What lives here

- |start/getting-started| — install, a first `dekit.yaml`, `up` / `ls` / `attach`
- |start/targets| — paths, globs, `+tags`, and stop vs down vs veto
- |cli| — every subcommand
- |cli/up| — the workday start verb in detail
- |config| — `dekit.yaml` keys

:::footnote
Topic help is `dekit help <topic>`; flag help is `dekit <command> --help`.
:::
