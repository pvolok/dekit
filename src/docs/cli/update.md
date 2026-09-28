---
title: dekit update
cli: dekit update
order: 17
related: [cli/runner/upgrade, config/kernel, start/getting-started]
---

Updates the dekit you ran and then switches its running runners to the new
version. Their tasks keep running and attached terminals stay attached.
It runs the install script again, into the directory of this binary.

The last step is |cli/runner/upgrade| with `--all`. Runners on another
binary, such as a project with a `kernel:` pin (|config/kernel|), are
listed and left alone. Running `update` again when nothing is new changes
nothing.

A version installs that release instead of the latest, which is also the
way back from a bad one. `canary` is the latest build of the main branch.

:::callout note title="Windows"
Runners keep their version until they restart: |cli/down|, then |cli/up|.
:::

If `update` itself fails, install again as in |start/getting-started|.

:::usage

```sh
dekit update
dekit update 1.2.3
dekit update canary
```
