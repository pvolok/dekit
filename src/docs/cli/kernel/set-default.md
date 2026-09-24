---
title: dekit kernel set-default
cli: dekit kernel set-default
order: 2
related: [cli/kernel, cli/kernel/clear-default, config/kernel]
---

Registers a dekit binary as the default kernel for projects that do not pin
one. With no path, the binary you are running is registered. The
registration is installation state, not runtime state: it persists until
|cli/kernel/clear-default|.

:::usage
