# dekit

Process manager for dev and prod.

dekit runs your project's tasks, such as servers, databases and workers, in
development and in production. You define the tasks in a config file, and
dekit handles dependencies, crashes and restarts.

## Install

```sh
cargo install dekit
```

Building from source needs a C compiler. Prebuilt binaries are also
available:

```sh
curl -fsSL https://dekit.run/install.sh | sh     # macOS, Linux
irm https://dekit.run/install.ps1 | iex    # Windows (PowerShell)
npm install -g dekit
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

The tasks keep running after you close the terminal. `dekit help` shows the
docs in your terminal.

## Coming from mprocs

dekit is the next version of mprocs. `dekit mprocs` runs your `mprocs.yaml`
with the same flags and keys. See
[Coming from mprocs](https://dekit.run/docs/start/from-mprocs) to switch to
`dekit.yaml`.

## Links

- Documentation: https://dekit.run/docs
- Changelog: https://github.com/pvolok/dekit/blob/master/CHANGELOG.md
- Repository: https://github.com/pvolok/dekit
