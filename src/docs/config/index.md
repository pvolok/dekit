---
title: Config
summary: dekit.yaml, its fragments, and the user config.yaml.
related: [config/tasks, config/load, config/kernel, config/hooks, config/user]
---

A project is the nearest `dekit.yaml` above the current directory. It
declares the tasks (|config/tasks|), may pull in fragments (|config/load|),
pins a kernel (|config/kernel|), and runs hooks (|config/hooks|).
Presentation settings, such as key bindings and the sidebar, belong to you
rather than to a project: they live in the user config (|config/user|).

Relative paths (`cwd`, `script`, `add_path`, log files) resolve from the
file that declares them.

## Project keys

:::fields kind=config
- key: tasks
  type: object
  desc: Task path to task definition (|config/tasks|). Paths may nest, as in `services/web`.
- key: defaults
  type: object
  desc: Settings shared by every task (cwd, env, add_path, autostart, autorestart, ready_log, stop, log, scrollback_len, mouse_scroll_speed). A task's own value wins.
- key: load
  type: "(string | {file, at})[]"
  desc: Extra YAML fragments, as globs relative to this file (|config/load|).
- key: kernel
  type: "npm | {path}"
  desc: Pin the dekit binary that runs this project (|config/kernel|).
- key: log
  type: "{level, file}"
  desc: The runner's own diagnostic log. `level` is off, error, warn, info, debug, or trace.
- key: on_init
  type: command
  desc: A command to run once the runner has loaded the project (|config/hooks|).
- key: on_idle
  type: command
  desc: A command to run each time the last active task goes idle (|config/hooks|).
:::

## Example

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

:::footnote
The JSON Schema is [dekit.json](https://dekit.run/schemas/dekit.json).
:::
