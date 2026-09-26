---
title: Tasks
summary: Every key of a task in dekit.yaml, from cmd and deps to stop and log.
related: [config, start/targets, config/hooks]
order: 10
---

Each entry under `tasks` is a task: a command to run, what it depends on,
and how dekit treats it.

```yaml
tasks:
  db:
    cmd: ["postgres", "-D", ".data/db"]
    ready_log: "ready to accept connections"
  api:
    cmd: ["cargo", "run", "-p", "api"]
    deps: [db]
    autostart: true
  logs:
    shell: tail -f var/log/*.log | grep -v healthz
  repl:
    cmd: node
    stop: {send-keys: [".exit", "<Enter>"]}
```

:::fields kind=config
- key: tasks.*.cmd
  type: "string | string[]"
  desc: The program to run. An array is the exact argv; a string is split into words like a shell would, without running one.
- key: tasks.*.shell
  type: string
  desc: A line to run through `/bin/sh -c` (PowerShell on Windows), so pipes and variables work.
- key: tasks.*.script
  type: string
  desc: A `.js` or `.mjs` file to run on dekit's built-in JavaScript runtime (|js|).
- key: tasks.*.label
  type: string
  desc: The name shown in the TUI. Defaults to the task path.
- key: tasks.*.deps
  type: string[]
  default: "[]"
  desc: Tasks that must be up before this one starts. Starting this task starts them too.
- key: tasks.*.tags
  type: string[]
  default: "[]"
  desc: Tags for `+tag` targets (|start/targets|).
- key: tasks.*.cwd
  type: path
  desc: Working directory. Defaults to the project root.
- key: tasks.*.env
  type: object
  desc: Extra environment variables. A null value removes the variable.
- key: tasks.*.add_path
  type: string[]
  desc: Directories to put in front of PATH.
- key: tasks.*.autostart
  type: boolean
  default: "false"
  desc: Start the task on `dekit up` and when the runner starts fresh. It adds the `autostart` tag.
- key: tasks.*.autorestart
  type: boolean
  default: "false"
  desc: Restart the task when it exits with an error, waiting longer after each quick failure.
- key: tasks.*.ready_log
  type: string
  desc: Dependents wait until a line of the task's output contains this string. Without it, the task counts as up once it starts.
- key: tasks.*.stop
  type: "shutdown | kill | SIGNAME | {signal, group} | {send-keys} | {cmd}"
  default: shutdown
  desc: How to stop the task (|config/tasks#stopping|).
- key: tasks.*.log
  type: "boolean | path | {enabled, dir, file, mode}"
  desc: Also write the task's output to a file (|config/tasks#logs|).
- key: tasks.*.scrollback_len
  type: integer
  default: "1000"
  desc: Lines of output kept for scrolling back.
- key: tasks.*.mouse_scroll_speed
  type: integer
  default: "5"
  desc: Lines per mouse-wheel step in the TUI.
:::

Exactly one of `cmd`, `shell`, or `script` is required. The `defaults`
section (|config|) takes the same settings, from `cwd` down, for every
task; a task's own value wins.

## Stopping {#stopping}

The default `shutdown` sends SIGTERM to the task's whole process group. A
signal name such as `SIGINT` sends that signal instead, and
`{signal: SIGINT, group: false}` sends it to the main process only.
`kill` sends SIGKILL. `{send-keys: ["<C-c>"]}` types keys into the task's
terminal, and `{cmd: "docker compose stop"}` runs a shell command. A task
that is still running 10 seconds later is killed.

On Windows, `shutdown`, `kill`, and `SIGINT`/`SIGTERM`/`SIGKILL` all end
the process.

## Logs {#logs}

A string is a directory: the output goes to a file named after the task,
such as `api.log`. The object form sets `dir`, `file`, and `mode`
(`append`, the default, or `truncate`); `{name}`, `{pid}`, and `{ts}` in
`dir` or `file` are filled in on each run. `true` and `false` turn
logging on or off, which is handy when `defaults` sets the directory.
