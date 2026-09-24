---
title: std.env
summary: Read environment variables.
related: [js, js/process]
order: 40
---

:::fields kind=js
- key: std.env.get
  signature: "(key: string) => string | undefined"
  desc: The value of an environment variable, or `undefined` when unset.
:::

A script task also finds `DEKIT_RUNNER_ROOT` and `DEKIT_RUNNER_KIND` in
its environment (|start/runners|).
