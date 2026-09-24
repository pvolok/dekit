---
title: dekit help
cli: dekit help
order: 18
related: [cli, start/agents]
---

Opens a documentation topic: a command (`dekit help up`, `dekit help
runner stop`), a page (`dekit help config`, `dekit help targets`), or a
tag. With no topic it lists every topic. On a terminal the page is styled,
wrapped, and paged through `$PAGER` (`less` by default); piped, it is
markdown, which is what an agent wants to read. `--json` prints the topic
as data, or the whole tree when no topic is given.

`dekit <command> --help` shows only a command's flags, while
`dekit help <command>` shows the same flags inside the command's page.

:::usage

```sh
dekit help
dekit help up
dekit help runner stop
dekit help targets
dekit help --json > docs.json
```
