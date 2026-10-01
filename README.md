<h1 align="center">dekit</h1>

<p align="center"><b>Process manager for dev and prod</b></p>

<p align="center">
  <a href="https://dekit.run/docs"><img src="https://img.shields.io/badge/docs-dekit.run-blue" alt="Docs" /></a>
  <a href="https://x.com/pavelvolok"><img src="https://img.shields.io/badge/follow-%40pavelvolok-black?logo=x" alt="Follow @pavelvolok on X" /></a>
</p>

**dekit is a process manager.** It runs your project's tasks, such as servers,
databases and workers, in development and in production.

- Define your project's tasks in a config file
- dekit handles dependencies, crashes and restarts
- Watch and control tasks in a terminal UI
- A full CLI for humans and agents
- Built-in JavaScript for writing scripts

<img src="img/dekit-tui.png" alt="dekit terminal UI" width="900" />

## Install

macOS, Linux:

```sh
curl -fsSL https://dekit.run/install.sh | sh
```

Windows (PowerShell):

```powershell
irm https://dekit.run/install.ps1 | iex
```

## Quick start

`dekit.yaml` at the project root:

```yaml
tasks:
  db:
    cmd: ["postgres", "-D", ".data/db"]
    ready: { log: "ready to accept connections" }
  api:
    cmd: ["cargo", "run", "-p", "api"]
    deps: [db]
    autostart: true
```

```sh
dekit up        # start the runner and the autostart tasks
dekit ls        # see what is running
dekit attach    # open the terminal UI
dekit down      # stop for the day; `dekit up` brings everything back
```

<img src="img/dekit-up.png" alt="dekit up and dekit ls in a terminal" width="360" />

The tasks keep running after you close the terminal. `dekit help` shows the
docs in your terminal.

## Coming from mprocs

dekit is the next version of [mprocs](README-mprocs.md). `dekit mprocs` runs
your `mprocs.yaml` with the same flags and keys. See
[Coming from mprocs](https://dekit.run/docs/start/from-mprocs) to switch to
`dekit.yaml`.

## Updates

Follow [@pavelvolok](https://x.com/pavelvolok) on X for news about dekit.

## License

MIT
