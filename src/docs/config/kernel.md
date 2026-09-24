---
title: Kernel
summary: Pin the dekit binary a project runs, with kernel in dekit.yaml or a registered default.
related: [config, cli/kernel, cli/runner/upgrade]
order: 30
---

The kernel is the binary that runs a project's runner. The `dekit` you type
is only a client; the runner it starts may be another build, so a project
can stay on the version it was written for while you upgrade your global
install.

Selection, in order:

1. `kernel: npm` in `dekit.yaml` resolves the project's native dekit package (`@dekit/dekit-<platform>`) through Node, so the version in `package.json` is the one that runs.
2. `kernel: {path: ...}` points at a binary, relative to the file.
3. Otherwise the default registered with |cli/kernel/set-default|.
4. Otherwise the binary you invoked.

```yaml
kernel: npm
```

|cli/kernel/status| shows all three: selected, running, and default. When
the selection changes while a runner is up, |cli/runner/upgrade| switches
it live.
