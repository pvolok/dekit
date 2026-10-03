---
title: dekit ls
cli: dekit ls
order: 4
related: [cli/why, start/targets]
---

Lists tasks with their state, one per line; with no target, every task in
the default space. `ls '@*/**'` also shows the runner's own tasks, such as
`@dekit/console`. A task that has ended shows how, as in `exited (code 1)`,
`backoff (signal 9)`, or `exited (not ready in time)`.

Tasks are listed in the order `dekit.yaml` has them, and the console
(|cli/attach|) lists them the same way. Tasks under one path, like
`web/api` and `web/ui`, stay together where the first of them comes. A
duplicate comes right after the task it was made from. Any other new
task, such as one from |cli/spawn|, comes last among the tasks beside it.
When `dekit.yaml` is edited, the next restart lists its tasks in the new
order; a task that is not in it stays right after the task it followed.

With `--json` the result is `{"tasks": [...]}`; each task has `id`, `path`,
an optional `label`, and `state`. A task that is `done`, `exited`, or in
`backoff` also has `exit_code` or `signal` from its last run, and `reason:
"ready_timeout"` if it was stopped for not being ready in time.

:::usage

```sh
dekit ls
dekit ls +backend
dekit ls --json
```
