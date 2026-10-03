---
title: dekit runner restart
cli: dekit runner restart
order: 8
related: [cli/runner, cli/runner/upgrade, cli/restart]
---

Restarts the runner in place, which reloads `dekit.yaml`; running tasks keep
running. A task from `dekit.yaml` that would sit above or under a task the
runner already has, such as `web/api` while `web` is still there, is not
added; |cli/runner/status| shows a warning for it. Remove the old task and
restart again to add it. To restart tasks instead, use |cli/restart|. Not
available on Windows.

:::usage
