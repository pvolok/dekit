---
title: dekit why
cli: dekit why
related: [cli/ls, start/targets, start/agents]
---

Explains one task: its state, whether it is wanted (pinned itself, or
required by a pinned task), whether every dependency is satisfied, a veto,
who requires it, restart attempts, and each dependency with its own state.
The target must match exactly one task.

With `--json` the fields are `id`, `path`, `state`, `wanted`,
`supported`, `vetoed`, `pinned`, `required_by`, `attempts`, and `deps`,
where each dependency has `path`, `state`, `wanted`, and `satisfied`.

:::usage

```sh
dekit why web
dekit why web --json
```
