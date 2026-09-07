<h1 align="center">dekit</h1>

<p align="center"><b>Process runner and scripting toolkit with CLI and TUI</b></p>

**dekit** is the next evolution of **mprocs**, a TUI tool for running multiple
commands, viewing their output separately, and interacting with each process.
The project continues in this repository under its new name.

While **mprocs** is a foreground TUI app, **dekit** uses client-server
architecture, similar to tmux. This allows dekit to be controlled via TUI, CLI,
or by agents.

> **In development:** the first dekit release is not available yet. The CLI,
> configuration format, and scripting API are still being designed and may change.
> Published mprocs releases remain available for use today.

## What dekit adds

mprocs brings your project's commands into one terminal interface. dekit builds
on that foundation with a broader process runner and scripting toolkit:

- Keep processes running independently of the terminal, and reconnect when needed.
- Coordinate services and tasks through dependencies and readiness checks.
- Automate workflows with scripts, alongside interactive control through a CLI and TUI.

## TODOs before first dekit release

- [ ] Finalize the CLI, config format, and JavaScript API.
- [ ] Live upgrade.
- [ ] Finalize per-project dekit versioning.
- [ ] Fix Windows bugs and missing features.

## Use mprocs for now

Before dekit is released you can still use mprocs.

Development builds of dekit support (and will support after release) mprocs CLI
via:

```sh
dekit mprocs ...
```

- [mprocs v0.9.6 release and binaries](https://github.com/pvolok/dekit/releases/tag/v0.9.6)
- [mprocs installation and usage](README-mprocs.md)
- [Release history](CHANGELOG.md)

<img src="img/mprocs1.png" alt="mprocs terminal interface" width="900" />
