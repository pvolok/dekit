---
title: dekit runner list
cli: dekit runner list
order: 4
related: [cli/runner, cli/runner/status, cli/runner/clean]
---

Lists every runner record on this machine, project and host, running or
stale, with pid, root, socket, and version. With `--json` each record
carries a `running` flag. Stale records go away with |cli/runner/clean|.

:::usage
