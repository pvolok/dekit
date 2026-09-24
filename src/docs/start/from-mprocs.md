---
title: Coming from mprocs
summary: dekit runs your mprocs.yaml unchanged, and what changed on the way to dekit.yaml.
related: [cli/mprocs, config, config/user]
order: 50
---

dekit is the next version of mprocs. The whole mprocs CLI is still in the
binary: `dekit mprocs` reads `mprocs.yaml`, accepts the same flags,
answers `--ctl`, and uses the classic keymap, so nothing has to change on
day one (|cli/mprocs|).

## What is different

- The runner is a separate process. Close the terminal and the tasks keep running; `dekit attach` comes back to them, and `dekit runner stop` ends them.
- `dekit.yaml` replaces `mprocs.yaml`. Tasks live under `tasks:` and gain `deps`, `ready_log`, `tags`, and `autostart` (|config/tasks|); a `procs:` key is not read.
- Key bindings and TUI settings moved to the user config (|config/user|), so a project cannot rebind your keys.
- The CLI does what `--ctl` did, with targets: `dekit start web`, `dekit stop +workers`, `dekit ls` (|cli|).
- A `mprocs.yaml` does not make a directory a dekit project; `dekit.yaml`, a git repository, or a `package.json` does.

:::callout tip
To convert a config, rename `procs:` to `tasks:` and keep `cmd`, `shell`,
`cwd`, `env`, `autostart`, `autorestart`, `stop`, and `add_path` as they
were.
:::
