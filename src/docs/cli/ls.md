---
title: dekit ls
cli: dekit ls
related: [cli/why, start/targets, start/agents]
---

Lists tasks with their state, one per line; with no target, every task in
the project's default space. States are idle, starting, running, ready,
stopping, backoff, done, and exited, the last two with an exit code or
signal. `ls '@*/**'` spans every space, including the runner's own
`@dekit/console`.

With `--json` the result is `{"tasks": [...]}`, each task with `id`,
`path`, an optional `label`, `state`, and `exit_code` or `signal` once it
has ended.

:::usage

```sh
dekit ls
dekit ls +backend
dekit ls '@*/**'
dekit ls --json
```
