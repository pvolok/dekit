---
title: Config
summary: dekit.yaml keys for tasks, defaults, fragments, hooks, and the kernel.
related: [start/getting-started, cli]
---

The nearest `dekit.yaml` is the project. Small projects keep every task
inline; larger ones `load` fragments. Relative `cwd`, `script`, `add_path`,
and log paths resolve from the file that declares them.

## Project keys

:::fields kind=config
- key: tasks
  type: object
  desc: Task path to task definition. Paths may nest (`services/web`).
- key: defaults
  type: object
  desc: Settings shared by every task (cwd, env, add_path, autostart, autorestart, ready_log, stop, log, scrollback_len, mouse_scroll_speed). A task's own value wins.
- key: load
  type: "(string | {file, at})[]"
  desc: Extra YAML fragments, as globs relative to this file. `at` mounts the fragment's tasks under a path.
- key: kernel
  type: "npm | {path}"
  desc: Pin the dekit binary that runs this project. `npm` resolves the project's native dekit package; `path` points at a binary.
- key: log
  type: "{level, file}"
  desc: The runner's own log. `level` is off, error, warn, info, debug, or trace.
- key: on_init
  type: command
  desc: "A command to run once the runner has loaded the project, such as `{command: start, target: +ci}`."
- key: on_idle
  type: command
  desc: A command to run each time the last active task goes idle.
:::

## Task keys

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

:::callout note
Duplicate task paths, missing load files, and load cycles are errors. A
nested `dekit.yaml` stays an independent project, never a fragment.
:::

:::footnote
The JSON Schema is [dekit.json](https://dekit.run/schemas/dekit.json).
Presentation settings (`tui`, `keymap`) live in the user config, not in the
project.
:::
