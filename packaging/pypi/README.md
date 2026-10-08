# dekit

Process manager for dev and prod.

dekit runs your project's tasks, such as servers, databases and workers, in development and in production. You define the tasks in a config file, and dekit handles dependencies, crashes and restarts.

## Install

```sh
pip install dekit-cli
```

Or as a tool, with uv or pipx:

```sh
uv tool install dekit-cli
pipx install dekit-cli
```

This installs the `dekit` command, a native binary. Wheels are published for macOS (arm64, x64), Linux (arm64, x64) and Windows (x64).

dekit also installs with a script (`curl -fsSL https://dekit.run/install.sh | sh`, or `irm https://dekit.run/install.ps1 | iex` on Windows) or from npm (`npm install -g @dekit/cli`).

`dekit update` updates only a dekit from the script. Update this one with `pip install -U dekit-cli` (or `uv tool upgrade dekit-cli`), then run `dekit runner upgrade --all` to switch running runners to it.

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

The tasks keep running after you close the terminal. `dekit help` shows the docs in your terminal.

## Coming from mprocs

dekit is the next version of mprocs. `dekit mprocs` runs your `mprocs.yaml` with the same flags and keys. See [Coming from mprocs](https://dekit.run/docs/start/from-mprocs) to switch to `dekit.yaml`.

## Links

- Documentation: https://dekit.run/docs
- Changelog: https://github.com/pvolok/dekit/blob/master/CHANGELOG.md
- Repository: https://github.com/pvolok/dekit
