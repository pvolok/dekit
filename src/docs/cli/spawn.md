---
title: dekit spawn
cli: dekit spawn
related: [cli/run, cli/rm, config/tasks]
---

Adds a process task at a path and starts it, without touching
`dekit.yaml`. The command comes after `--`; `--cwd` defaults to the
current directory, `--env` sets variables, `--dep` makes the task wait on
existing tasks, and `--tag` tags it. Tasks added this way carry the
`dynamic` tag, so `dekit rm +dynamic` removes all of them. They are not
autostart, and they are gone once the runner stops unless it was paused.

:::usage

```sh
dekit spawn tmp/watch -- npm run watch
dekit spawn workers/1 --dep queue --tag worker -- ./worker --id 1
dekit spawn api --cwd services/api --env PORT=8080 -- cargo run
```
