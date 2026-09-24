---
title: JavaScript
summary: Run scripts on dekit's embedded runtime, with the std API for files, processes, the terminal, and the runner.
related: [js/dekit, js/fs, js/tui, config/tasks]
---

dekit embeds a JavaScript engine. `dekit script.js` runs a file as an ES
module, and a task with `script:` in `dekit.yaml` runs one under the
runner (|config/tasks|). Scripts see one global, `std`, whose modules are
documented in this section; there is no Node.js API and no `node_modules`
resolution. `std.dekit` drives the runner the script belongs to.

```js
const result = await std.process.exec("git", ["status", "--short"]);
if (result.stdout.trim() !== "") {
  std.warn("working tree is dirty");
}
await std.dekit.start("+workers");
```

## Logging

:::fields kind=js
- key: std.log
  signature: "(...args: unknown[]) => void"
  desc: Log a message to stderr at info level.
- key: std.warn
  signature: "(...args: unknown[]) => void"
  desc: Log a message to stderr at warn level.
- key: std.error
  signature: "(...args: unknown[]) => void"
  desc: Log a message to stderr at error level.
:::

:::callout warning
The `std` global is an early API and will change before the first dekit
release: built-in modules will be imported as `dekit/v1/*` instead of read
from a global.
:::
