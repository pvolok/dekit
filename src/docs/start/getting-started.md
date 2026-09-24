---
title: Getting started
summary: Install dekit, write a dekit.yaml, and bring a stack up.
related: [cli, config, cli/up]
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
dekit up        # start the runner and the autostart tasks, in dependency order
dekit ls        # see what is running
dekit why web   # explain why a task is (or is not) running
dekit attach    # watch live output in the TUI
```

Close the terminal and the stack keeps running: the runner is a separate
process. `dekit down` unpins every task; `dekit runner stop` stops the runner
itself. See |start/targets| for what stop, down, and veto mean.

:::callout note
The nearest `dekit.yaml` above the current directory defines the project; a
git repository or a `package.json` also marks a project root. `-C <dir>`
selects another root. Outside any project, dekit does not fall back to the
host runner: name it explicitly with a `host::` target.
:::

## Next

- |cli/up| — what bare `up` actually starts
- |config| — `cmd`, `deps`, `ready_log`, `load`, `kernel`
- |cli| — the rest of the verbs
