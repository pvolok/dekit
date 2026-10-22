---
title: dekit wait
cli: dekit wait
order: 11
related: [cli/start, cli/up, cli/stop, config/tasks]
---

Waits until the matching tasks are ready, or with `--for exit` until
they have exited, and says with its status how it went. It starts and
stops nothing, and it doesn't start the runner. `--wait` on `up`,
`start`, `restart`, `stop`, `kill`, and `veto` carries out the verb, then
waits on the tasks it acted on.

## Ready {#ready}

`--for ready`, the default, waits until each task is ready, or done for
a job: what its dependents wait for (|config/tasks#ready|). A task
without a `ready` check is ready once it starts. Tasks nobody started are
left out, so `dekit wait '**'` asks whether everything that should run is
ready.

It fails as soon as a task can't get ready unless someone acts: its run
failed, nothing wants it (it was stopped, vetoed, or never started), it
was removed, or a dep can't get ready. The failure names the task at the
root of it and shows the end of its screen. While dekit will still start
a task by itself, as `autorestart` does after a crash, the wait goes on.

## Exit {#exit}

`--for exit` waits until each task has no process and won't start again
by itself, then judges how each run ended: done, exit 0, and a task that
nothing wants are a success. It waits for every task before it says, so
`dekit wait +ci --for exit` reports every job, and a job that is done
already is over at once, so its result can be collected later. A task
waiting for its deps has not exited yet.

## Down {#down}

`stop --wait`, `kill --wait`, and `veto --wait` wait until each task is
down: no process, and nothing will start it again. How its last run
ended is shown, not judged. A task that a started task still needs starts
right back, so the wait fails at once and says who needs it; `veto` keeps
it down. A stop that dekit gave up on, because the process outlived a
kill, fails the wait too: |cli/ls| shows it as `idle (stop timed out)`.

## Status {#status}

The status is 0 when the wait is met and 1 when it fails or the runner
goes away. When `--timeout` (`--wait-timeout` on a verb) runs out, it is
124 and what is still pending is listed; the tasks are left as they are.
`--timeout 0` checks once; a limit is at most `24h`.

A wait sees each task as it is now, after whatever came before it: after
`dekit restart api`, `dekit wait api` waits for the new run, and running
it again after a timeout goes on waiting. With `--json` the result is
`{"wait": "met" | "failed" | "timeout", "tasks": [...]}`: each task's
entry from |cli/ls| with `wait` (`pending`, `met`, or `failed`), and for
a failed one `blocked_by`, the task at the root, and its |cli/why|.

:::usage

```sh
dekit wait api
dekit wait '**' --timeout 0
dekit wait +ci --for exit --json
dekit up --wait --wait-timeout 5m
```
