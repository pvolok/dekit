---
title: std.process
summary: Run commands, and read the script's own arguments and identity.
related: [js, js/dekit]
order: 50
---

`exec` runs a program to completion and collects its output; it does not
go through the runner, so the command is not a task. For long-running
work, register a task with |js/dekit| instead.

:::fields kind=js
- key: std.process.exec
  signature: "(cmd: string, args?: string[]) => Promise<{stdout, stderr, code}>"
  desc: Run a command and return its output and exit code (-1 when killed by a signal).
- key: std.process.cwd
  signature: "() => string"
  desc: The current working directory.
- key: std.process.exit
  signature: "(code?: number) => never"
  desc: Exit the script with a status, 0 by default.
- key: std.process.argv
  signature: "readonly string[]"
  desc: The command line, starting with the dekit binary and the script path.
- key: std.process.pid
  signature: "readonly number"
  desc: The script's process id.
:::
