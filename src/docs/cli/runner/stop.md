---
title: dekit runner stop
cli: dekit runner stop
related: [cli/runner, cli/down, cli/runner/pause]
---

Stops the selected runner and everything it runs. The runner gets a
graceful quit first; if it has not exited after 21 seconds it is killed by
its recorded pid, and on Unix the tasks it spawned are reaped with it. A
stale runtime record is cleaned up on the way.

To keep the tasks and their screens for tomorrow, use |cli/runner/pause|
instead. To stop tasks but keep the runner, use |cli/down|.

:::usage
