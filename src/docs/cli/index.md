---
title: CLI
cli: dekit
related: [cli/up, start/targets, config]
---

Every command talks to the runner of the project you are in: the nearest
`dekit.yaml` above the current directory, else the nearest git repository or
`package.json`. `-C <dir>` picks another project root, and `--json` switches
a command to machine-readable output.

With no command, `dekit` attaches to the runner's console. A `.js` file
argument runs that script on the embedded JavaScript runtime.

:::usage

Most commands take a target (a task path, a glob, or a `+tag`); see
|start/targets|.
