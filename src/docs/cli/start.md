---
title: dekit start
cli: dekit start
order: 7
related: [cli/up, cli/stop, start/targets]
---

Pins and starts the matching tasks. Their dependencies come up first, and a
dependency with a `ready_log` is waited on before its dependents start.
This clears any veto on the tasks and their dependencies. The reply is the
number of tasks that matched; zero is not an error.

:::usage

```sh
dekit start web
dekit start +workers
dekit start 'services/*'
```
