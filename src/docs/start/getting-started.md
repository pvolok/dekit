---
title: Getting started
summary: Install dekit, write a dekit.yaml, and bring a stack up.
related: [start/targets, cli/up, config]
order: 10
---

Install with the script, from npm, or from crates.io:

```sh
curl -fsSL https://dekit.run/install.sh | sh
```

```sh
npm install -g dekit
```

```sh
cargo install dekit
```

On Windows (PowerShell):

```powershell
iwr -useb https://dekit.run/install.ps1 | iex
```

## A first project

Describe the stack in `dekit.yaml` at the project root:

```yaml
tasks:
  db:
    cmd: ["postgres", "-D", ".data/db"]
    ready_log: "ready to accept connections"

  api:
    cmd: ["cargo", "run", "-p", "api"]
    deps: [db]
    autostart: true

  web:
    cmd: ["npm", "run", "dev"]
    deps: [api]
    autostart: true
```

Then:

```sh
dekit up        # start the autostart tasks and what they need
dekit ls        # see what is running
dekit why web   # explain why a task is (or is not) running
dekit attach    # watch live output in the TUI
```

`db` has no `autostart`, but `api` needs it, so `up` starts it first and
waits for its `ready_log` line.

Close the terminal and the tasks keep running. `dekit down` stops
everything for the day, and the next `dekit up` brings it back
(|cli/down|).

:::callout note
The project root is the nearest directory above you with a `dekit.yaml`,
else a git repository, else a `package.json`. `-C <dir>` names it
explicitly (|start/runners|).
:::

