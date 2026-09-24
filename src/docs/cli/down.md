---
title: dekit down
cli: dekit down
related: [cli/up, cli/stop, start/targets]
---

Unpins the matching tasks; with no target, every task in the project. A
task that nothing needs any more stops; one that a pinned dependent still
needs keeps running. Nothing is forced: |cli/veto| holds a task down and
|cli/kill| ends it now.

Bare `down` is the end-of-day counterpart of |cli/up|. It does not stop
the runner; that is |cli/runner/stop|.

:::usage

```sh
dekit down
dekit down +ci
dekit down 'services/*'
```
