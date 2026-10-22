---
title: dekit ls
cli: dekit ls
order: 4
related: [cli/why, cli/reload, start/targets]
---

Lists tasks with their state, one per line; with no target, every task in
the default space. `ls '@*/**'` also shows the runner's own tasks, such as
`@dekit/console`. A task that has ended shows how, as in `exited (code 1)`,
`backoff (signal 9)`, or `exited (not ready in time)`. A job is `running`
until it ends, then `done`. A task whose process outlived a kill, so
that dekit gave up on the stop, is `idle (stop timed out)` until it starts
again.

Tasks are listed in the order `dekit.yaml` has them when the runner
starts, and the console (|cli/attach|) lists them the same way. Tasks
under one path, like `web/api` and `web/ui`, stay together where the
first of them comes. A duplicate comes right after the task it was made
from, and a task a reload adds where `dekit.yaml` has it. Any other new
task, such as one from |cli/spawn|, comes last among the tasks beside it.
From then on the order is the runner's: a task moved in the console stays
where it was put, across restarts too, and when `dekit.yaml` reorders its
tasks, |cli/reload| offers the new order.

With `--json` the result is `{"tasks": [...]}`; each task has `id`, `path`,
an optional `label`, `kind` (`service` or `job`), and `state`. A task that
is `done`, `exited`, or in `backoff` also has `exit_code` or `signal` from
its last run, and `reason: "ready_timeout"` if it was stopped for not
being ready in time, as a `stopping` one has while that stop is under way;
an idle one has `reason: "kill_timeout"` when its stop timed out.

:::usage

```sh
dekit ls
dekit ls +backend
dekit ls --json
```
