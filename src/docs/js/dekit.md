---
title: std.dekit
summary: Drive the script's own runner from JavaScript, with the same verbs as the CLI.
related: [js, cli, start/targets]
order: 10
---

`std.dekit` sends commands to the runner the script belongs to: the
project runner for a `script:` task, or the project found from the
current directory for `dekit script.js`. Targets are written as on the
command line (|start/targets|) but cannot name another runner. Every
call resolves to the number of tasks it acted on, like the CLI's
`{matched}`.

```js
await std.dekit.add("tmp/build", ["npm", "run", "build"]);
const started = await std.dekit.start("+workers");
std.log(`started ${started} workers`);
```

:::fields kind=js
- key: std.dekit.start
  signature: "(target: string) => Promise<number>"
  desc: Pin and start the matching tasks and their dependencies.
- key: std.dekit.run
  signature: "(target: string) => Promise<number>"
  desc: Force-restart the matching tasks, whether or not they are wanted.
- key: std.dekit.stop
  signature: "(target: string) => Promise<number>"
  desc: Unpin and stop; a task restarts if a dependent still needs it.
- key: std.dekit.down
  signature: "(target: string) => Promise<number>"
  desc: Unpin only.
- key: std.dekit.kill
  signature: "(target: string) => Promise<number>"
  desc: Stop with an immediate hard kill.
- key: std.dekit.veto
  signature: "(target: string) => Promise<number>"
  desc: Force down and hold down until started again.
- key: std.dekit.restart
  signature: "(target: string) => Promise<number>"
  desc: Restart the matching tasks.
- key: std.dekit.remove
  signature: "(target: string) => Promise<number>"
  desc: Remove the matching tasks, killing running ones.
- key: std.dekit.add
  signature: "(path: string, cmd: string[]) => Promise<number>"
  desc: Register a process task at an exact path with the given argv, tagged `dynamic`, and start it.
:::
