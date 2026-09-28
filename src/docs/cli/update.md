---
title: dekit update
cli: dekit update
order: 17
related: [cli/runner/upgrade, config/kernel, start/getting-started]
---

Updates the dekit you ran, the way it was installed, and then switches its
running runners to the new version. Their tasks keep running and attached
terminals stay attached.

- Installed with the script: runs the same install script again, into the directory of this binary.
- Installed with `npm install -g`: runs `npm install -g dekit@latest`.
- Installed with `cargo install`: runs `cargo install dekit --locked`.
- Installed in a project's `node_modules`: nothing is changed. The project's `package.json` owns that version; update it with the project's package manager.

The last step is |cli/runner/upgrade| with `--all`. Runners on another
binary, such as a project with a `kernel:` pin (|config/kernel|), are
listed and left alone. Running `update` again when nothing is new changes
nothing.

A version installs that release instead of the latest, which is also the
way back from a bad one. `canary` is the latest build of the main branch;
it comes from the install script only.

:::callout note title="Windows"
Only script installs update themselves. For npm and cargo, `update` prints
the command to run after you stop the runners. Runners keep their version
until they restart: |cli/down|, then |cli/up|.
:::

If `update` itself fails, install again as in |start/getting-started|.

:::usage

```sh
dekit update
dekit update 1.2.3
dekit update canary
```
