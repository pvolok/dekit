---
title: dekit spawn
cli: dekit spawn
order: 12
related: [cli/run, cli/rm, config/tasks]
---

Adds a task at a path and starts it, without touching `dekit.yaml`. The
command comes after `--`. The task gets the `dynamic` tag, so
`dekit rm +dynamic` removes every task added this way. It survives
|cli/down| like any other task.

:::usage

```sh
dekit spawn tmp/watch -- npm run watch
dekit spawn workers/1 --dep queue --tag worker -- ./worker --id 1
dekit spawn api --cwd services/api --env PORT=8080 -- cargo run
```
