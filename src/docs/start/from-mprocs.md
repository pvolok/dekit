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
- `dekit.yaml` replaces `mprocs.yaml`. Tasks live under `tasks:` and gain `ready`, `type: job`, and `tags` (|config/tasks|).
- `autorestart: true` is `autorestart: on-failure` in `dekit.yaml`.
- `mprocs.yaml` runs `shell` lines through the system shell, `/bin/sh` or PowerShell, as mprocs did. `dekit.yaml` has no `shell`: a `cmd` is a program and its arguments, run without a shell (|config/tasks#commands|); a line that needs a shell runs one, as in `cmd: ["bash", "-c", "..."]`.
- Key bindings and TUI settings live in your user config (|config/user|), so a project cannot rebind your keys.
- The CLI does what `--ctl` did, with targets: `dekit start web`, `dekit stop +workers`, `dekit ls` (|cli|).
- A `mprocs.yaml` does not make a directory a dekit project; a `dekit.yaml`, a git repository, or a `package.json` does.

:::callout tip
To convert a config, rename `procs:` to `tasks:` and write each task as a
mapping: `web: "npm run dev"` becomes `web: {cmd: npm run dev}`, or, on
Windows for now, `web: {cmd: ["cmd", "/c", "npm run dev"]}`
(|config/tasks#commands|). A `cmd` array carries over as it is, and a
`shell` line becomes a `cmd` string when it is a plain command, or runs
its shell when it uses pipes, `&&`, or variables: `["sh", "-c", "..."]`,
or `["pwsh", "-Command", "..."]` on Windows, where mprocs used
PowerShell. mprocs started every process by default; in dekit, set
`autostart: true` on the tasks `dekit up` should start. In `stop`,
`send-keys` is `keys`, a `cmd` runs without a shell like a task's, and
`hard-kill` is `SIGKILL`. mprocs sent stop signals to the main process
only; dekit sends them to the whole process group unless `group: false`.
:::
