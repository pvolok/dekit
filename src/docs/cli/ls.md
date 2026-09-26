---
title: dekit ls
cli: dekit ls
order: 4
related: [cli/why, start/targets]
---

Lists tasks with their state, one per line; with no target, every task in
the default space. `ls '@*/**'` also shows the runner's own tasks, such as
`@dekit/console`.

With `--json` the result is `{"tasks": [...]}`; each task has `id`, `path`,
an optional `label`, `state`, and `exit_code` or `signal` once it has ended.

:::usage

```sh
dekit ls
dekit ls +backend
dekit ls --json
```
