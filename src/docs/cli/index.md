---
title: CLI
cli: dekit
related: [cli/up, start/targets, config]
---

Every command talks to the runner of the project you are in: the nearest
`dekit.yaml` above the current directory, else the nearest git repository or
`package.json`. `-C <dir>` picks another project root, and `--json` switches
a command to machine-readable output.

With no command, `dekit` opens the runner's console. If the runner is not
running, it starts it like |cli/up| first; if it is, it only attaches
like |cli/attach|. A `.js` file argument runs that script on the
embedded JavaScript runtime.

:::usage

Most commands take a target (a task path, a glob, or a `+tag`); see
|start/targets|.
