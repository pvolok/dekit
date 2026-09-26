---
title: Targets
summary: Paths, globs, +tags, and the difference between stop, veto, and kill.
related: [cli, cli/up]
order: 20
---

A **target** selects tasks. `start`, `stop`, `kill`, `veto`, `restart`, and
`rm` require one; `ls` defaults to everything. `up` and `down` take no
target: they start and stop the whole project (|cli/up|).

## Forms

- a task path — `services/web`
- a glob — `services/*` matches one level, `services/**` any depth
- a tag — `+backend` (a plus, not a hash: an unquoted `#` starts a shell comment)
- in a space — `@dekit/console`, or `@*/web` for every space
- on a runner — `project::web`, `host::+ci`, `~/dev/api::web/*`

Quote globs so the shell leaves them alone.

A runner is `project` (the nearest project), `host` (the machine-wide
runner, |start/runners|), or a path (`/abs/dir`, `~/dir`, `./dir`) naming a
project root. `**` covers the default space only; `@*/**` spans every space
in the runner, including the built-in `@dekit/console`.

## Stopping tasks

A task runs while it is **pinned** (you started it) or something running
depends on it.

- **stop** unpins it and stops it now; it starts again if a dependent still needs it
- **veto** stops it and its running dependents and keeps it stopped until it, or a task that needs it, is started again
- **kill** is stop with an immediate hard kill

`dekit stop '**'` stops every task and keeps the runner. `dekit down`
stops the runner too, keeping the tasks for the next `dekit up`.
