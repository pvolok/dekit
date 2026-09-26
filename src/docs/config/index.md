---
title: Config
summary: dekit.yaml, its fragments, and the user config.yaml.
related: [config/tasks, config/load, config/kernel, config/hooks, config/user]
---

A project is the nearest `dekit.yaml` in the current directory or above.
It lists the tasks (|config/tasks|) and can pull in more files
(|config/load|), pin a dekit version (|config/kernel|), and run hooks
(|config/hooks|). Key bindings and TUI settings are yours, not the
project's: they live in the user config (|config/user|).

Relative paths resolve from the file that declares them.

```yaml
tasks:
  db:
    cmd: ["postgres", "-D", ".data/db"]
    ready_log: "ready to accept connections"
  api:
    cmd: ["cargo", "run", "-p", "api"]
    deps: [db]
    autostart: true
defaults:
  env:
    RUST_LOG: info
on_idle: {command: quit}
```

## Project keys

:::fields kind=config
- key: tasks
  type: object
  desc: Tasks by path (|config/tasks|). Paths may nest, as in `services/web`.
- key: defaults
  type: object
  desc: Task settings shared by every task, such as `cwd`, `env`, or `stop`. A task's own value wins.
- key: load
  type: "(string | {file, at})[]"
  desc: More files with tasks, as globs (|config/load|).
- key: kernel
  type: "npm | {path}"
  desc: The dekit binary that runs this project (|config/kernel|).
- key: log
  type: "{level, file}"
  desc: The runner's own debug log. `level` is off, error, warn, info, debug, or trace.
- key: on_init
  type: command
  desc: A command to run once the project has loaded (|config/hooks|).
- key: on_idle
  type: command
  desc: A command to run each time the last active task goes idle (|config/hooks|).
:::

:::footnote
The JSON Schema is [dekit.json](https://dekit.run/schemas/dekit.json).
:::
