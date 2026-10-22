---
title: dekit start
cli: dekit start
order: 7
related: [cli/up, cli/stop, start/targets]
---

Pins and starts the matching tasks. Their dependencies come up first, and a
dependency with a `ready` check is waited on before its dependents start.
This clears any veto on the tasks and their dependencies. The reply is the
number of tasks that matched; zero is not an error.

With `--wait` it returns once the tasks are ready, or done for a job, and
fails when one can't get there, naming why (|cli/wait|); then zero
matches is an error. `--wait-timeout` gives up after a while, with the
status 124. A task that is ready already is met at once, so it is safe
to run before every test run.

:::usage

```sh
dekit start web
dekit start +workers
dekit start 'services/*'
dekit start +e2e --wait --wait-timeout 2m
```
