---
title: Requests
summary: The request methods, their params and results, and the error codes.
related: [rpc, rpc/attach, start/targets]
hidden: true
order: 10
---

A connection carries any number of requests, and several may be in flight
at once. `id` is chosen by the client, unique per connection, and
correlates each response; responses may arrive out of order. `attach` is
the exception: it takes over the connection (|rpc/attach|). Empty params
are omitted on the wire; receivers treat missing, `null`, and `{}` params
the same.

:::commands
- cmd: command
  desc: "Params are a command verbatim (`{command: start, target: ...}`); result `{matched}` for task-directed verbs, `add`, and `up`; `{stop_within_ms}` for `down` and `quit`; `{}` for `batch`."
- cmd: ls
  desc: "Params `{target?}`, default `**`; result `{tasks: [task-info]}`."
- cmd: why
  desc: "Params `{target}` for one task; result the why object: `wanted`, `supported`, `vetoed`, `pinned`, `saved_pin` (when set), `required_by`, `deps`, `attempts`."
- cmd: screen
  desc: "Params `{target}` for one task; result `{screen}` as ANSI text."
- cmd: attach
  desc: "Params `{target, width, height, until_exit?}`; result `{}`, then the connection is in session mode."
- cmd: upgrade
  desc: "Params `{binary}`; result `{version}` from the new binary once it has taken over."
:::

A target is written as on the command line (|start/targets|): a path, a
glob, or a `+tag`, optionally in a space. The exact runtime-id form is the
object `{"id": 7}`. A runner qualifier is resolved by the client and never
sent to a kernel; a malformed target is `bad_target`.

## Commands

Every mutation is one `command` request whose params are the command
itself: a `command` tag plus its fields. Task-directed verbs (start, stop,
kill, veto, restart, force-restart, remove, rename, duplicate) carry
a `target`, act on the matches atomically in the kernel, and reply with
`{matched}`, the count of tasks acted on. A task registered concurrently
is either fully included or fully excluded. Zero matches is a normal
reply, not an error.

`add {target, label?, cmd: [..] | script, cwd?, env?, deps?, tags?}`
registers a process task at an exact path and starts it. `cmd` is an argv
run without a shell (a string is split into one as in
|config/tasks#commands|), and an empty one is `invalid_params`; `script`
is a `.js` or `.mjs` file to run instead (|js|). `env` values of `null`
unset a variable, each `deps` target must match at least one task (else
`no_match`), and a taken path, or one above or under another task, is
`path_taken`. The task takes the
project's `defaults` except `autorestart`, which is `never`.

`up` takes no target. It pins and starts the tasks that were started when
the tasks were last saved (their saved pin, which it clears) and the
tasks tagged `autostart`, with their dependencies, and replies
`{matched}` with how many that was. It leaves running tasks and done
jobs alone, so it is safe to repeat.

`down` saves the tasks for the next `up` and stops the runner; `quit`
stops it without saving. Both reply `{stop_within_ms}`: how long
its stops can take at most, each task's stop timeout and kill along its
deps, plus the save. `batch {commands}` runs a list in order and replies
`{}`.

`why`, `screen`, and `attach` need exactly one task: `no_match` when the
target matches none, `ambiguous` when it matches several, `no_screen` when
the task has no screen.

## Task state

`ls` task-info and `why` carry a stable state token: `idle`, `starting`,
`running`, `ready`, `stopping`, `backoff`, `done`, or `exited`. `done`,
`exited`, and `backoff` also carry how the last run ended: `exit_code` or
`signal`, and `reason: "ready_timeout"` when the task was stopped for not
being ready in time. Task-info is `{id, path, label?, state, exit_code?,
signal?, reason?}` where `path` is the space-qualified target of the task
(`@dekit/console`).

## Error codes

`unknown_method`, `invalid_params`, `no_match`, `ambiguous`, `bad_target`,
`path_taken`, `no_screen`, `internal`, `unsupported_protocol`,
`unsupported`, `busy`. Codes are wire API; messages are human text and may
change.
