---
title: Using dekit from an agent
hidden: true
summary: The commands, flags, and JSON shapes an agent needs to drive a project's processes.
related: [cli, start/targets, cli/help]
order: 40
---

Everything an agent needs is on the command line: the docs, JSON output,
and the runner's view of every task.

## Reading the docs

`dekit help` lists the topics and `dekit help <topic>` prints one. When
stdout is not a terminal the output is markdown. `dekit help --json`
exports every page as data.

## JSON output

Pass `--json` for machine-readable output.

- `dekit ls --json` prints `{"tasks": [...]}`; each task has `id`, `path`, `label` (if set), `kind` (`service` or `job`), and `state`. States: idle, starting, running, ready, stopping, backoff, done, exited. A job is running until it ends. In done, exited, and backoff, `exit_code` or `signal` tells how the last run ended, and `reason: "ready_timeout"` that it was not ready in time; an idle task with `reason: "kill_timeout"` outlived a kill.
- `dekit wait <target> --json`, and `--wait` on a verb, print `{"wait": "met" | "failed" | "timeout", "tasks": [...]}`: each task's `ls` entry with `wait` (`pending`, `met`, `failed`), and for a failed one `blocked_by` and its `why`.
- `dekit why <path> --json` prints the task's `path` and `state`, plus `wanted`, `supported`, `vetoed`, `pinned`, `required_by`, `attempts`, and `deps` with each dependency's state; `saved_pin: true` means `dekit up` starts it again.
- `dekit screen <path> --json` prints `{"screen": "..."}`, the task's current terminal contents with ANSI colors.
- `up`, `start`, `stop`, `kill`, `veto`, `restart`, and `rm` print `{"matched": n}`; zero matches is not an error.
- `dekit down --json` prints `{"stopped": true}`, or `false` when the runner was not running.
- `dekit runner stop --json` prints `{"stopped": true, "removed_saved": false}`; when the runner was not running and `down` had saved its tasks, `{"stopped": false, "removed_saved": true}`. With neither it exits 1.

## Waiting

Never sleep or poll `ls` to wait for a task: `dekit start api --wait`
returns once api is ready, and fails with why, including the end of the
failing task's screen. `dekit restart api --wait` waits for the new run
after an edit, `dekit wait api` waits on what someone else started, and
`dekit wait test --for exit` collects a job's result later. Give a wait a
limit that fits your tool's, as in `--wait-timeout 90s`: it exits with 124
and says what is pending, and running `dekit wait` again goes on waiting
(|cli/wait|).

## Running things

- `dekit run <path> -- <cmd>` runs a one-off in the foreground and exits with the command's status (128 plus the signal number when a signal ended it). The task is removed when it exits.
- `dekit spawn <path> -- <cmd>` starts a long-running task. `dekit rm +dynamic` removes every task not from `dekit.yaml`, these among them.

Other commands exit with 0 on success and 1 on an error, such as no
project, a bad target, or a request the runner refused; the message goes to
stderr. A malformed command line exits with 2, and a wait that runs out of
time with 124.

## A note for CLAUDE.md

```markdown
This project uses dekit. `dekit ls --json` shows the tasks,
`dekit why <task>` explains one, `dekit screen <task>` shows its output,
and `dekit help <topic>` documents any command. After an edit, `dekit
restart <task> --wait --wait-timeout 90s` waits for it to come back up.
Use `dekit run <name> -- <cmd>` for one-off commands so they show up in
the TUI while they run.
```
