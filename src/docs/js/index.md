---
title: JavaScript
summary: Run scripts on dekit's embedded runtime, with the std API for files, processes, and the runner.
related: [js/dekit, js/fs, config/tasks]
---

dekit has a built-in JavaScript engine. `dekit script.js` runs a file as
an ES module, and a task with `script:` in `dekit.yaml` runs one under the
runner (|config/tasks|). The module runs from top to bottom, and
top-level `await` works. If the module exports a `main` function, dekit
then calls it and waits for the promise it returns. Scripts see one global,
`std`, described in this section. There is no Node.js API.

```js
const result = await std.process.exec("git", ["status", "--short"]);
if (result.stdout.trim() !== "") {
  std.warn("working tree is dirty");
}
await std.dekit.start("+workers");
```

The script exits with code 0 when it finishes. An error thrown at the top
level or in `main` is printed, and the script exits with code 1.

## Logging

:::fields kind=js
- key: std.log
  signature: "(...args: unknown[]) => void"
  desc: Print the values to stderr, separated by spaces. Strings print as they are, objects and arrays as JSON, and errors with their stack.
- key: std.warn
  signature: "(...args: unknown[]) => void"
  desc: Print to stderr, like `std.log`.
- key: std.error
  signature: "(...args: unknown[]) => void"
  desc: Print to stderr, like `std.log`.
:::

:::callout warning
The `std` API is experimental and will change. Built-in modules will later
be imported as `dekit/v1/*` instead of read from a global.
:::
