---
title: Coming from mprocs
summary: dekit runs your mprocs.yaml unchanged, and what changed on the way to dekit.yaml.
related: [cli/mprocs, config, config/user]
order: 50
---

dekit is the next version of mprocs. The mprocs CLI is still in the
binary: `dekit mprocs` reads `mprocs.yaml`, takes the same flags, answers
`--ctl`, and uses the classic keymap (|cli/mprocs|).

## What is different

- The runner is a separate process. Close the terminal and the tasks keep running; `dekit attach` comes back to them.
- `dekit.yaml` replaces `mprocs.yaml`. Tasks live under `tasks:` and gain `ready_log` and `tags` (|config/tasks|).
- Key bindings and TUI settings live in your user config (|config/user|), so a project cannot rebind your keys.
- The CLI does what `--ctl` did, with targets: `dekit start web`, `dekit stop +workers`, `dekit ls` (|cli|).
- A `mprocs.yaml` does not make a directory a dekit project; a `dekit.yaml`, a git repository, or a `package.json` does.

:::callout tip
To convert a config, rename `procs:` to `tasks:` and write each task as a
mapping (`shell: "npm run dev"` rather than a bare string). mprocs started
every process by default; in dekit, set `autostart: true` on the tasks
`dekit up` should start.
:::
