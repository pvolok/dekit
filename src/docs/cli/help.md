---
title: dekit help
cli: dekit help
order: 18
related: [cli]
---

Shows a documentation topic: a command (`dekit help runner stop`), a page
(`dekit help targets`), or a tag. With no topic it lists every topic. On a
terminal the page goes through `$PAGER` (`less` by default); piped, it is
plain markdown. `--json` prints the topic as data, or every topic when none
is given.

`dekit <command> --help` shows only the flags.

:::usage

```sh
dekit help
dekit help up
dekit help targets
dekit help --json > docs.json
```
