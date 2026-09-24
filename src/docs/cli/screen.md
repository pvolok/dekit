---
title: dekit screen
cli: dekit screen
related: [cli/attach, cli/ls, start/agents]
---

Prints a task's current screen as text with its colors, the same cells the
TUI shows, and resets the terminal attributes afterwards. Use it in a pipe
or from an agent to read what a task is showing without attaching. With
`--json` the result is `{"screen": "..."}`. The target must match exactly
one task.

:::usage

```sh
dekit screen web
dekit screen web | tail -20
```
