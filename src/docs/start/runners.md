---
title: Runners
summary: Where a project's runner lives, how it is found, and the host runner for machine-wide tasks.
related: [cli/runner, start/targets, config/kernel]
order: 30
---

Every project has its own runner: a separate process that owns the
project's tasks, their terminals, and their logs, and outlives the
terminal you started it from. The CLI, the TUI, and scripts connect to it
over a socket. Each runner runs the dekit version selected for its project
(|config/kernel|).

## Finding the project

A command looks upward from the current directory for `dekit.yaml`, then
for a git repository, then for a `package.json`. A `dekit.yaml` wins even
when another marker is closer. `-C <dir>` names the root explicitly.

Commands that start tasks (`up`, `start`, `restart`, `run`, `spawn`,
`attach`) start the runner when it is not running;
|cli/runner/start| starts it without doing anything else.

## The host runner

Tasks that are yours rather than a project's, such as a database you
always want up, belong to the host runner. Its config is
`~/.config/dekit/host/dekit.yaml`. Name it with `host::` in a target or
`host` on `dekit down` or a `dekit runner` command. Outside a project dekit
reports an error instead of falling back to it, so `down` cannot stop
machine-wide tasks by accident.

## Stopping a runner

`dekit down` stops the runner and saves its tasks and screens; the next
start brings them back. `dekit runner stop` stops it without saving, so
the next start begins from `dekit.yaml`.

Script tasks (`script:` in `dekit.yaml`) get `DEKIT_RUNNER_ROOT` and
`DEKIT_RUNNER_KIND` in their environment, so the script talks to the runner
that started it.
