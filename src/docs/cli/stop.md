---
title: dekit stop
cli: dekit stop
related: [cli/down, cli/kill, cli/veto, config/tasks]
---

Unpins and stops the matching tasks now. A task that a pinned dependent
still needs comes back afterwards, because the dependent still wants it;
to leave it running instead, unpin only with |cli/down|, and to hold it
down, use |cli/veto|. A stop sends the task's configured `stop`
(|config/tasks|), then kills after a grace period.

:::usage

```sh
dekit stop web
dekit stop +workers
```
