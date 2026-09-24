---
title: dekit veto
cli: dekit veto
related: [cli/stop, cli/down, cli/start, start/targets]
---

Forces the matching tasks down and holds them there, whatever depends on
them, until they are started again with |cli/start| or |cli/up|. A veto is
the one way to keep a task down while its dependents stay pinned: they are
blocked, not restarted, and `dekit why` shows the dependency as not
satisfied.

:::usage

```sh
dekit veto db
dekit start db
```
