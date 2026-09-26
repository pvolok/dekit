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

- `dekit ls --json` prints `{"tasks": [...]}`; each task has `id`, `path`, `label` (if set), `state`, and `exit_code` or `signal` once it has ended. States: idle, starting, running, ready, stopping, backoff, done, exited.
- `dekit why <path> --json` prints the task's `path` and `state`, plus `wanted`, `supported`, `vetoed`, `pinned`, `required_by`, `attempts`, and `deps` with each dependency's state.
- `dekit screen <path> --json` prints `{"screen": "..."}`, the task's current terminal contents with ANSI colors.
- `up`, `start`, `stop`, `kill`, `veto`, `restart`, and `rm` print `{"matched": n}`; zero matches is not an error.
- `dekit down --json` prints `{"stopped": true}`, or `false` when the runner was not running.
- `dekit runner stop --json` prints `{"stopped": true, "removed_saved": false}`; when the runner was not running and `down` had saved its tasks, `{"stopped": false, "removed_saved": true}`. With neither it exits 1.

## Running things

- `dekit run <path> -- <cmd>` runs a one-off in the foreground and exits with the command's status (128 plus the signal number when a signal ended it). The task is removed when it exits.
- `dekit spawn <path> -- <cmd>` starts a long-running task. `dekit rm +dynamic` removes every task added from the command line.

Other commands exit with 0 on success and 1 on an error, such as no
project, a bad target, or a request the runner refused; the message goes to
stderr. A malformed command line exits with 2.

## A note for CLAUDE.md

```markdown
This project uses dekit. `dekit ls --json` shows the tasks,
`dekit why <task>` explains one, `dekit screen <task>` shows its output,
and `dekit help <topic>` documents any command. Use `dekit run <name> --
<cmd>` for one-off commands so they show up in the TUI while they run.
```
