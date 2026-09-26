---
title: std.process
summary: Run commands, and read the script's own arguments and identity.
related: [js, js/dekit]
order: 50
---

`exec` runs a program directly, without a shell, and waits for it to
finish. The program is not a task; for long-running work, add a task with
|js/dekit| instead.

```js
export async function main() {
  const { stdout, code } = await std.process.exec("git", ["rev-parse", "HEAD"]);
  if (code !== 0) std.process.exit(1);
  std.log(stdout.trim());
}
```

:::fields kind=js
- key: std.process.exec
  signature: "(cmd: string, args?: string[]) => Promise<{stdout, stderr, code}>"
  desc: Run a program and return its output and exit code (-1 when killed by a signal).
- key: std.process.cwd
  signature: "() => string"
  desc: The current working directory.
- key: std.process.exit
  signature: "(code?: number) => never"
  desc: Exit the script right away with a status, 0 by default.
- key: std.process.argv
  signature: "readonly string[]"
  desc: The command line, starting with the dekit binary and the script path.
- key: std.process.pid
  signature: "readonly number"
  desc: The script's process id.
:::
