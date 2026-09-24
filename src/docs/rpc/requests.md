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
  desc: "Params are a command verbatim (`{command: start, target: ...}`); result `{matched}` for task-directed verbs, `{}` otherwise."
- cmd: ls
  desc: "Params `{target?}`, default `**`; result `{tasks: [task-info]}`."
- cmd: why
  desc: "Params `{target}` for one task; result the why object: `wanted`, `supported`, `vetoed`, `pinned`, `required_by`, `deps`, `attempts`."
- cmd: screen
  desc: "Params `{target}` for one task; result `{screen}` as ANSI text."
- cmd: attach
  desc: "Params `{target, width, height, until_exit?}`; result `{}`, then the connection is in session mode."
- cmd: upgrade
  desc: "Params `{binary}`; result `{version}` from the new binary once it has taken over."
- cmd: pause
  desc: "No params; result `{}`, then the runner quits."
:::

A target is written as on the command line (|start/targets|): a path, a
glob, or a `+tag`, optionally in a space. The exact runtime-id form is the
object `{"id": 7}`. A runner qualifier is resolved by the client and never
sent to a kernel; a malformed target is `bad_target`.

## Commands

Every mutation is one `command` request whose params are the command
itself: a `command` tag plus its fields. Task-directed verbs (start, stop,
down, kill, veto, restart, force-restart, remove, rename, duplicate) carry
a `target`, act on the matches atomically in the kernel, and reply with
`{matched}`, the count of tasks acted on. A task registered concurrently
is either fully included or fully excluded. Zero matches is a normal
reply, not an error.

`add {target, label?, cmd: [..] | shell, cwd?, env?, deps?, tags?}`
registers a process task at an exact path and starts it; `env` values of
`null` unset a variable, each `deps` target must match at least one task
(else `no_match`), and a taken path is `path_taken`. `quit` stops the
runner and `batch {commands}` runs a list in order; both reply `{}`.

`why`, `screen`, and `attach` need exactly one task: `no_match` when the
target matches none, `ambiguous` when it matches several, `no_screen` when
the task has no screen.

## Task state

`ls` task-info and `why` carry a stable state token: `idle`, `starting`,
`running`, `ready`, `stopping`, `backoff`, `done`, or `exited`, plus
optional `exit_code` or `signal` for `done` and `exited`. Task-info is
`{id, path, label?, state, exit_code?, signal?}` where `path` is the
space-qualified target of the task (`@dekit/console`).

## Error codes

`unknown_method`, `invalid_params`, `no_match`, `ambiguous`, `bad_target`,
`path_taken`, `no_screen`, `internal`, `unsupported_protocol`,
`unsupported`, `busy`. Codes are wire API; messages are human text and may
change.
