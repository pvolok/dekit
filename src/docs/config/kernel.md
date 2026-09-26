---
title: Kernel
summary: Pin the dekit binary a project runs, with kernel in dekit.yaml or a registered default.
related: [config, cli/kernel, cli/runner/upgrade]
order: 30
---

The kernel is the dekit binary that runs a project's runner. It can differ
from the `dekit` you type, so a project can stay on its own version while
you upgrade your global install.

```yaml
kernel: npm
```

dekit picks the first of:

1. `kernel: npm`: the `dekit` npm package installed in the project, found through Node.
2. `kernel: {path: bin/dekit}`: a binary, relative to `dekit.yaml`.
3. The default set with |cli/kernel/set-default|.
4. The `dekit` you ran.

|cli/kernel/status| shows which one is selected and which one is running.
|cli/runner/upgrade| switches a running runner to the selected one.
