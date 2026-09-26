---
title: dekit down
cli: dekit down
order: 3
related: [cli/up, cli/stop, cli/runner/stop, start/targets]
---

The workday end verb. It stops every task and the project's runner, and
saves the tasks, their screens, and which ones you had started for the
next |cli/up|. Running it when the runner is not running does nothing.
A runner that crashed or was killed saved nothing, and one stopped with
|cli/runner/stop| saves nothing, so its next start begins from
`dekit.yaml`.

`down` takes a runner, not a target: `host`, `project`, or a path to a
project root (|cli/runner|). To stop tasks and keep the runner, use
|cli/stop|; `dekit stop '**'` stops all of them. How `stop` differs from
`veto` and `kill` is on |start/targets|.

:::usage

```sh
dekit down
dekit down ~/src/api
dekit down host
```
