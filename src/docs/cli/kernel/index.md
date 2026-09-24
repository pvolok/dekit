---
title: dekit kernel
cli: dekit kernel
order: 16
related: [config/kernel, cli/runner/upgrade]
---

The kernel is the dekit binary that runs a project's runner. It is selected
per project: a `kernel:` pin in `dekit.yaml`, else the registered user
default, else the binary you invoked. |config/kernel| has the details.

:::usage
