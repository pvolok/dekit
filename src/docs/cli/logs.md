---
title: dekit logs
cli: dekit logs
order: 6
related: [cli/screen, config/tasks, start/targets]
---

Prints what tasks logged: the lines they printed, merged in the order
they were logged, each after its task's name, with a line where a task
started or exited. With `-f` it then prints new lines as they come, until
Ctrl-C. With no target it covers every task, and `-f` takes in tasks
that are added later and match.

## Lines {#lines}

dekit takes lines from what a task prints, the way a terminal shows it:
a line is logged when the task moves to the next one, or exits. A line
redrawn in place, such as a `\r` progress bar or a status line, is logged
once, as it ended. A full screen clear (`tsc --watch`, jest) starts a
fresh screen, and what was logged stays. Full-screen apps such as `vim`
or `htop` log nothing while they run; |cli/screen| and |cli/attach| show
them. The line a task is still writing is printed at the end, without
`-f`.

dekit keeps the last 10,000 lines of each task, while the runner runs:
they are gone after `dekit down` or a live upgrade. A `log:` file
(|config/tasks#logs|) keeps everything, as the task printed it.

## Output {#output}

`-n` sets how many lines are printed first, 100 by default, or `all`;
`--since 10m` prints only what came in the last ten minutes.
`--no-prefix` leaves out the names. Names are colored when stdout is a
terminal, unless `NO_COLOR` is set. If `-f` falls more than 10,000 lines
behind a task, it says how many it missed.

With `--json`, each line is an object: `seq` (the order across tasks),
`time_ms` (when it was logged, in ms since the Unix epoch), `task`, and
`kind`, which is `line` with `text`, `open` (a line still being written)
with `text`, `started`, `exited` with `code` or `signal`, or `dropped`
with `count`.

The status is 0, also when `-f` ends on Ctrl-C, and 1 when the runner
stops or is upgraded under `-f`.

:::usage

```sh
dekit logs
dekit logs api db -n 20
dekit logs 'web/*' -f
dekit logs api --since 5m --no-prefix | grep error
dekit logs +ci --json -n all | jq -r 'select(.kind == "exited")'
```
