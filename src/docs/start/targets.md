---
title: Targets
summary: Paths, globs, +tags, and the difference between stop, down, veto, and kill.
related: [cli, cli/up]
order: 20
---

A **target** selects tasks. The surgical verbs (`start`, `run`, `stop`,
`kill`, `veto`, `restart`, `rm`) require one. The workday verbs have
defaults: `up` means the autostart set, `down` means everything.

## Forms

- a task path — `services/web`
- a glob — `services/*` matches one level, `**` matches everything
- a tag — `+backend` (a plus, not a hash: an unquoted `#` starts a shell comment)
- optionally in a space — `@dekit/console`, or `@*/web` for every space
- optionally on a runner — `project::web`, `host::+ci`, `~/dev/api::web/*`

A runner is `project` (the nearest project), `host` (the machine-wide
runner), or a path (`/abs/dir`, `~/dir`, `./dir`) naming a project root.
Outside a project the host runner is never implied; write `host::` when you
mean it. Everything in the default space is `**`; `@*/**` deliberately spans
every space in the runner, including the built-in `@dekit/console`.

## Bringing tasks down

- **stop** unpins and stops now; a task restarts if a dependent still needs it
- **down** unpins only; a task keeps running while something still needs it
- **veto** forces a task down and holds it there until it is started again
- **kill** is stop with an immediate hard kill

Bare `dekit down` unpins every task (`**`). It does **not** stop the runner;
that is `dekit runner stop`.

:::callout warning
`veto` is the hold-down. `down` is not: if something still depends on a
task, `down` leaves it running.
:::
