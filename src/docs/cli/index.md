---
title: CLI
cli: dekit
related: [cli/up, start/targets, config]
---

Every command talks to the runner of the project you are in: the nearest
`dekit.yaml` above the current directory, else the nearest git repository or
`package.json`. `-C <dir>` picks another project root, and `--json` switches
any command to machine-readable output.

With no command, `dekit` attaches the terminal to the runner's console. A
trailing `.js` file runs on the embedded JavaScript runtime.

:::usage

The workday verbs `up` and `down` default to the autostart set and to
everything. The surgical verbs (`start`, `stop`, `kill`, `veto`, `restart`,
`rm`) require a target: see |start/targets|.

:::footnote
`dekit help <command>` shows a command's page; `dekit <command> --help` shows
only its flags.
:::
