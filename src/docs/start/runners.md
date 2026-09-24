---
title: Runners
summary: Where a project's runner lives, how it is found, and the host runner for machine-wide tasks.
related: [cli/runner, start/targets, config/kernel]
order: 30
---

Every project has its own runner: a separate process that owns the
project's tasks, their terminals, and their logs, and outlives the
terminal you started it from. Clients, whether `dekit`, the TUI, or a
script, connect to it over a socket. Projects are independent: each runner
runs the kernel selected for its project (|config/kernel|), so two
projects can be on different dekit versions.

## Finding the project

A command looks upward from the current directory for `dekit.yaml`;
failing that, for a git repository, then for a `package.json`. A
`dekit.yaml` wins even when another marker is closer. `-C <dir>` names the
root explicitly. The first command that needs a runner starts it;
|cli/runner/start| starts one without doing anything else.

## The host runner

Tasks that are yours rather than a project's, such as a database you
always want up or an editor server, belong to the host runner. Its config
is `~/.config/dekit/host/dekit.yaml`, and it is always addressed
explicitly: `host::` in a target, or `host` on a `dekit runner` verb.
Outside a project dekit reports an error rather than quietly using it, so
a bare `down` cannot reach machine-wide tasks by accident.

## Records

A running runner publishes a record with its pid, socket, root, version,
and kernel, under `$XDG_RUNTIME_DIR/dekit` when that variable is set and
otherwise in the user data directory (`~/.local/share/dekit`). Records
are discovery hints: |cli/runner/list| shows them, |cli/runner/clean|
drops stale ones, and a runner whose record is stale is simply started
again.

Script tasks learn their runner from `DEKIT_RUNNER_ROOT` and
`DEKIT_RUNNER_KIND` in their environment.
