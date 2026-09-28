---
title: Tasks
summary: Every key of a task in dekit.yaml, from cmd and deps to ready, autorestart, and stop.
related: [config, start/targets, config/hooks]
order: 10
---

Each entry under `tasks` is a task: a command to run, what it depends on,
and how dekit treats it.

```yaml
tasks:
  db:
    cmd: ["postgres", "-D", ".data/db"]
    ready: {log: "ready to accept connections"}
  migrate:
    type: job
    cmd: ["./migrate.sh"]
    deps: [db]
  api:
    cmd: ["cargo", "run", "-p", "api"]
    deps: [db, migrate]
    ready: {http: "http://localhost:3000/health"}
    autorestart: on-failure
    autostart: true
  logs:
    cmd: tail -f 'var/log/api server.log'
  repl:
    cmd: node
    stop: {keys: ["<C-d>"]}
```

:::fields kind=config
- key: tasks.*.cmd
  type: "string[] | string"
  desc: The program to run and its arguments. An array is the exact argv; a string is split into words (|config/tasks#commands|).
- key: tasks.*.script
  type: string
  desc: A `.js` or `.mjs` file to run on dekit's built-in JavaScript runtime (|js|).
- key: tasks.*.label
  type: string
  desc: The name shown in the TUI. Defaults to the task path.
- key: tasks.*.type
  type: "service | job"
  default: service
  desc: A service keeps running; a job runs to completion (|config/tasks#jobs|).
- key: tasks.*.deps
  type: string[]
  default: "[]"
  desc: Tasks that must be ready, or done for a job, before this one starts. Starting this task starts them too.
- key: tasks.*.tags
  type: string[]
  default: "[]"
  desc: Tags for `+tag` targets (|start/targets|).
- key: tasks.*.ready
  type: object
  desc: When the task counts as ready, so its dependents can start (|config/tasks#ready|). Without it, the task is ready once it starts.
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
  desc: Start the task on `dekit up`; a runner that starts on its own starts no task. It adds the `autostart` tag.
- key: tasks.*.autorestart
  type: "never | on-failure | always"
  default: never
  desc: When to start the task again by itself after it exits (|config/tasks#restarting|).
- key: tasks.*.stop
  type: "string | object"
  default: SIGTERM
  desc: How to stop the task, as a signal name or an object (|config/tasks#stopping|).
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

Exactly one of `cmd` or `script` is required. The `defaults`
section (|config|) takes the same settings, from `cwd` down, for every
task; a task's own value wins. `log` combines key by key. `stop` combines
in two parts: how to stop, which is the signal with its `group`, or
`keys`, or `cmd`, and `timeout`. With `defaults: {stop: SIGINT}`, a
task's `stop: {timeout: 60s}` still sends SIGINT; with `defaults: {stop:
{signal: SIGINT, group: false}}`, a task's `stop: SIGTERM` sends SIGTERM
to the whole process group.

Tasks added while the runner runs, with |cli/spawn|, |cli/run|, the `add`
command (|config/hooks|), or the add prompt in the TUI, take `defaults`
too, except `autorestart`: they are never started again by themselves.

Durations are written as `500ms`, `10s`, `2m`, or `1h`, up to `24h`.

Key names follow one rule. An action says how dekit does it: `stop` is how
dekit stops the task. `auto` in front says when dekit does it by itself:
`autostart` on `dekit up`, `autorestart` after a failure. A state says what
counts as it: `ready`.

## Commands {#commands}

A `cmd` is one program and its arguments, run without a shell. An array
such as `["npm", "run", "dev"]` is used as it is. A string such as
`npm run dev` is a shorter way to write the same array: dekit splits it
into words as sh would when it loads the config. The same holds for
`ready.cmd`, `stop.cmd`, and the `add` command (|config/hooks|).

- Spaces and tabs separate words.
- `'...'` keeps everything inside as written.
- `"..."` keeps everything inside, except that `\` escapes `"`, `\`, `$`, and `` ` ``. An unescaped `$` or `` ` `` inside is an error, since a shell would expand it.
- Outside quotes, `\` keeps the next character as it is, such as `a\ b`, and `\` at the end of a line joins it to the next.

The rules are the same on every OS, so a Windows path with `\` goes in
single quotes, as in `cmd: server.exe --data 'C:\data'`, or in an array,
as in `cmd: ['C:\tools\server.exe', --port, '80']`. Outside quotes,
`C:\data` would read as `C:data`.

In a string, anything a shell would treat specially is an error, so the
string never means something other than what it looks like. These need
quotes: `|`, `&`, `;`, `<`, `>`, `(`, `)`, `$`, `` ` ``, globs (`*`, `?`,
`[`), and `~` or `#` at the start of a word. A string is also refused if
it has a newline between words, starts with `NAME=value` (set variables
in `env`), or starts with a shell keyword such as `!` or `if`:

```sh
`&` needs quotes: cmd runs one program without a shell, so it has no operators; to use a shell, run one, as in `bash -c '...'` at <config>.tasks.web.cmd
```

An array has no such rules: each item is one argument, as written. Items
are strings, so a number goes in quotes, as in `["serve", "--port",
"3000"]`. For pipes, `&&`, variables, or globs, run a shell:

```yaml
tasks:
  protos:
    cmd: ["bash", "-c", "for f in proto/*.proto; do protoc $f; done"]
  report:
    cmd: ["pwsh", "-Command", "Get-ChildItem | Select-Object Name"]
```

:::callout note
On Windows, dekit does not yet start programs that are `.cmd` or `.bat`
files, such as `npm`, `npx`, or `yarn`, since it looks only for `.exe`
files. Run them through `cmd /c` for now, as in
`cmd: ["cmd", "/c", "npm run dev"]`.
:::

## Ready {#ready}

`ready` holds one check. Until it passes the task is starting, and the
tasks that depend on it wait.

:::fields kind=config
- key: tasks.*.ready.log
  type: string
  desc: A line of output contains this text. Colors and other escape codes are ignored.
- key: tasks.*.ready.tcp
  type: "port | host:port"
  desc: A TCP connection succeeds. Without a host, `localhost`.
- key: tasks.*.ready.http
  type: url
  desc: A GET to this `http://` URL answers with a 2xx or 3xx status.
- key: tasks.*.ready.cmd
  type: "string[] | string"
  desc: This command exits with 0. It runs like the task's `cmd`, in the task's directory with its environment.
- key: tasks.*.ready.file
  type: path
  desc: This file exists. Relative to the task's directory.
- key: tasks.*.ready.interval
  type: duration
  default: 1s
  desc: How long to wait between tries of `tcp`, `http`, `cmd`, and `file`. A try that takes longer than this (at least 1s) counts as failed.
- key: tasks.*.ready.timeout
  type: duration
  desc: If the task is not ready this long after it started, the start failed. dekit stops the task, and `autorestart` decides whether it runs again. No limit by default.
:::

```yaml
tasks:
  db:
    cmd: ["postgres", "-D", ".data/db"]
    ready: {cmd: pg_isready -h localhost, interval: 2s, timeout: 30s}
```

For `tcp` and `http`, `localhost` means both 127.0.0.1 and ::1: dekit
tries the two at the same time, and the first to connect counts.

A task that failed to start shows as `exited (not ready in time)` in
`dekit ls` and `dekit why`, or `backoff (not ready in time)` while
`autorestart` waits to start it again. `health` is reserved for checks on
a running task and is not supported yet.

## Jobs {#jobs}

A job is a task that runs to completion, such as a migration or a build
step. Tasks that depend on a job start once it exits with 0. A finished job
stays done: it does not run again when a task that needs it restarts, only
when you start it directly or after the runner stops and starts again. A job
takes no `ready` and no `autorestart: always`.

## Restarting {#restarting}

`autorestart` says when dekit starts a task again by itself. With `never`,
a task that exits stays stopped, so a crash stays on screen. `on-failure`
starts it again after a failure: the task exits with an error or with a
signal dekit did not send, or it is not ready within `ready.timeout`.
`always` also starts it again after a clean exit with 0. A stop that dekit
was asked for, such as `dekit stop`, is never a failure, whatever the task
exits with. The wait between tries grows after each quick failure, up to 30
seconds.

The `restart` key is reserved for how dekit restarts a task, such as a
reload. Today a restart (|cli/restart|) is a stop and then a start.

## Stopping {#stopping}

By default dekit sends SIGTERM to the task's whole process group. A signal
name, as in `stop: SIGINT`, sends that signal instead. The object form
takes at most one of `signal`, `keys`, and `cmd`; with none, it
keeps the default.

:::fields kind=config
- key: tasks.*.stop.signal
  type: string
  default: SIGTERM
  desc: Send this signal.
- key: tasks.*.stop.group
  type: boolean
  default: "true"
  desc: With `signal`, send it to the whole process group. `false` sends it to the main process only.
- key: tasks.*.stop.keys
  type: string[]
  desc: Type these keys into the task's terminal, such as `["<C-c>"]`.
- key: tasks.*.stop.cmd
  type: "string[] | string"
  desc: Run this program, like the task's `cmd`.
- key: tasks.*.stop.timeout
  type: duration
  default: 10s
  desc: "How long the task may take to exit before dekit kills it. It can stand alone, as in `stop: {timeout: 30s}`."
:::

`cmd` runs in the task's directory with its environment, and the task
should then exit on its own, as `docker compose up` does after
`{cmd: docker compose stop}`.

On Windows, `SIGINT`, `SIGTERM`, and `SIGKILL` end the process; other
signals do nothing, so the task is killed after the timeout.

## Logs {#logs}

A string is a directory: the output goes to a file named after the task,
such as `api.log`. The object form sets `dir`, `file`, and `mode`
(`append`, the default, or `truncate`); `{name}`, `{pid}`, and `{ts}` in
`dir` or `file` are filled in on each run. `true` and `false` turn
logging on or off, which is handy when `defaults` sets the directory.
