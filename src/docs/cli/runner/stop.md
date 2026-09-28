---
title: dekit runner stop
cli: dekit runner stop
order: 2
related: [cli/runner, cli/down, cli/runner/start]
---

Stops the selected runner and every task it runs, without saving them:
its next start begins from `dekit.yaml`. To keep the tasks for the next
start, use |cli/down|. The runner gets as long as its tasks' stops can
take: each task's `stop.timeout` and the kill after it, one task after
another along its deps (|config/tasks#stopping|). If it has not quit 5
seconds after that, it is killed, and on Unix its tasks with it. The same
holds for |cli/down|.

When the runner is not running but |cli/down| saved its tasks, `stop`
removes the save, so the next start is fresh. With nothing to stop or
remove it is an error. `--json` prints `{"stopped": bool, "removed_saved":
bool}`. To stop tasks but keep the runner, use |cli/stop|.

:::usage
