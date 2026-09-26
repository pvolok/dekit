---
title: dekit screen
cli: dekit screen
order: 6
related: [cli/attach, cli/ls]
---

Prints what a task's terminal shows right now, with colors, without
attaching. Handy in a pipe or for an agent. With `--json` the result is
`{"screen": "..."}`. The target must match exactly one task.

:::usage

```sh
dekit screen web
dekit screen web | tail -20
```
