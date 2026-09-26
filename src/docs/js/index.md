---
title: JavaScript
summary: Run scripts on dekit's embedded runtime, with the std API for files, processes, the terminal, and the runner.
related: [js/dekit, js/fs, config/tasks]
---

dekit has a built-in JavaScript engine. `dekit script.js` runs a file as
an ES module, and a task with `script:` in `dekit.yaml` runs one under the
runner (|config/tasks|). The module exports a `main` function; dekit calls
it and waits for the promise it returns. Scripts see one global, `std`,
described in this section. There is no Node.js API.

```js
export async function main() {
  const result = await std.process.exec("git", ["status", "--short"]);
  if (result.stdout.trim() !== "") {
    std.warn("working tree is dirty");
  }
  await std.dekit.start("+workers");
}
```

## Logging

:::fields kind=js
- key: std.log
  signature: "(...args: string[]) => void"
  desc: Print the strings to stderr, separated by spaces.
- key: std.warn
  signature: "(...args: string[]) => void"
  desc: Print to stderr, like `std.log`.
- key: std.error
  signature: "(...args: string[]) => void"
  desc: Print to stderr, like `std.log`.
:::

:::callout warning
The `std` global is an early API and will change before the first dekit
release: built-in modules will be imported as `dekit/v1/*` instead of read
from a global.
:::
