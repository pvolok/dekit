---
title: Tasks
summary: Every key of a task in dekit.yaml, from cmd and deps to stop and log.
related: [config, start/targets, config/hooks]
order: 10
---

Each entry under `tasks` is a task: a command to run, what it depends on,
and how dekit treats it. The `defaults` section holds the same settings
for every task; a task's own value wins.

:::fields kind=config
- key: tasks.*.cmd
  type: "string | string[]"
  desc: Command to run. An array is argv; a string is split like a shell would. One of cmd, shell, or script is required.
- key: tasks.*.shell
  type: string
  desc: Run this line through the shell (`/bin/sh -c`, PowerShell on Windows).
- key: tasks.*.script
  type: string
  desc: A `.js` or `.mjs` file to run on the embedded runtime.
- key: tasks.*.label
  type: string
  desc: Display name in the TUI. Defaults to the path.
- key: tasks.*.deps
  type: string[]
  default: "[]"
  desc: Task paths this task waits on. Fragment mounts rebase local names; prefix `/` to address the project root.
- key: tasks.*.tags
  type: string[]
  default: "[]"
  desc: "Tags for `+tag` targets. `autostart: true` adds the autostart tag."
- key: tasks.*.cwd
  type: path
  desc: Working directory. Relative paths resolve from the declaring file.
- key: tasks.*.env
  type: object
  desc: Extra environment variables. A null value unsets the variable.
- key: tasks.*.add_path
  type: string[]
  desc: Directories prepended to PATH.
- key: tasks.*.autostart
  type: boolean
  default: "false"
  desc: Start with `dekit up` and whenever the runner starts.
- key: tasks.*.autorestart
  type: boolean
  default: "false"
  desc: Restart the task after it exits, with backoff between attempts.
- key: tasks.*.ready_log
  type: string
  desc: Dependents wait until an output line contains this string.
- key: tasks.*.stop
  type: "shutdown | kill | SIGNAME | {signal, group} | {send-keys} | {cmd}"
  default: shutdown
  desc: How to stop the task. `shutdown` sends SIGTERM to the process group (terminates the process on Windows), a signal name targets the group too, `send-keys` types keys into the pty, and `cmd` runs a shell command.
- key: tasks.*.log
  type: "boolean | path | {enabled, dir, file, mode}"
  desc: Write the task's output to a file. A string is the directory; `mode` is append or truncate.
- key: tasks.*.scrollback_len
  type: integer
  default: "1000"
  desc: Lines of scrollback kept for the task's screen.
- key: tasks.*.mouse_scroll_speed
  type: integer
  default: "5"
  desc: Lines per mouse-wheel tick in the TUI.
:::

## Commands

`cmd` runs a program directly. As an array it is the exact argv; as a
string it is split the way a shell splits words, without running one.
`shell` hands the line to `/bin/sh -c` (PowerShell on Windows), so
pipes and variables work. `script` runs a JavaScript file on dekit's
embedded runtime, with the runner's identity in its environment.

```yaml
tasks:
  api:
    cmd: ["cargo", "run", "-p", "api"]
  logs:
    shell: tail -f var/log/*.log | grep -v healthz
  watcher:
    script: scripts/watch.js
```

## Dependencies and readiness

`deps` lists the tasks that must be up first. Starting a task starts its
dependencies; stopping a dependency while something still needs it brings
it back (|start/targets|). A dependency counts as up once it is running,
or, when it declares `ready_log`, once a line of its output contains that
string.

`autostart: true` tags the task `autostart`, the set that bare `dekit up`
starts and that comes up with the runner.

## Stopping

`stop` decides what a stop sends. The default `shutdown` is SIGTERM to the
whole process group. A signal name (`SIGINT`, `SIGHUP`, ...) does the
same with that signal; `{signal: SIGINT, group: false}` signals only the
leader. `{send-keys: ["<C-c>"]}` types keys into the task's terminal, and
`{cmd: "docker compose stop"}` runs a command. A task that ignores its
stop is killed after a grace period; `dekit kill` skips straight to that.

```yaml
tasks:
  repl:
    cmd: ["node"]
    stop: {send-keys: [".exit", "<Enter>"]}
```

## Logs

`log: true` writes the task's output to a file named after the task in
the default log directory; a string names the directory; the object form
sets `dir`, `file`, and `mode` (`append` or `truncate`). `{name}`,
`{id}`, `{pid}`, and `{ts}` in `dir` and `file` expand per run.
