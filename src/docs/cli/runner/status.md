---
title: dekit runner status
cli: dekit runner status
order: 3
related: [cli/runner, cli/runner/list, cli/kernel/status]
---

Shows whether the selected runner is running, starting, stale, failed, or
absent, with its pid, socket, version, and kernel binary. When the kernel
selected for the project differs from the one running, the output says so:
|cli/runner/upgrade| switches live.

With `--json` the runtime record is printed with `status`,
`restart_required`, `selected_binary`, and `selection_error` added.

:::usage
