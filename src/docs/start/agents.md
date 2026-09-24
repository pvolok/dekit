---
title: Using dekit from an agent
summary: The commands, flags, and JSON shapes an agent needs to drive a project's processes.
related: [cli, start/targets, cli/help]
order: 40
---

dekit is built to be driven by tools as much as by people. Everything an
agent needs is on the command line: the documentation, machine-readable
output, and the runner's view of every task.

## Reading the docs

`dekit help` lists the topics and `dekit help <topic>` prints one. When
stdout is not a terminal the output is markdown, so `dekit help ls` from
a shell tool reads cleanly. `dekit help --json` exports everything as
data.

## Machine-readable output

Every command takes `--json`.

- `dekit ls --json` prints `{"tasks": [...]}`; each task has `id`, `path`, `label` (if any), `state`, and `exit_code` or `signal` once it has ended. States: idle, starting, running, ready, stopping, backoff, done, exited.
- `dekit why <path> --json` prints `wanted`, `supported`, `vetoed`, `pinned`, `required_by`, `attempts`, and `deps` with each dependency's state.
- `dekit screen <path> --json` prints `{"screen": "..."}`, the task's current terminal contents with ANSI colors.
- start, stop, down, kill, veto, restart, and rm print `{"matched": n}`; zero matches is a normal result, not an error.

## Running things

- `dekit run <path> -- <cmd>` runs a one-off in the foreground and exits with the command's status (128 plus the signal number when a signal ended it); the task is removed afterwards.
- `dekit spawn <path> -- <cmd>` starts a long-running task; `dekit rm <path>` removes it, and `dekit rm +dynamic` removes every task added this way.
- `dekit up`, `dekit down`, and the surgical verbs take targets (|start/targets|); quote globs so the shell leaves them alone.

Exit status is 0 on success and 1 on any error: no project, a bad target,
or a runner that refused the request. Error text goes to stderr.

## A note for CLAUDE.md

```markdown
This project uses dekit. `dekit ls --json` shows the running tasks,
`dekit why <task>` explains one, `dekit screen <task>` shows its output,
and `dekit help <topic>` documents any command. Use `dekit run <name> --
<cmd>` for one-off commands so their output stays visible in the TUI.
```
