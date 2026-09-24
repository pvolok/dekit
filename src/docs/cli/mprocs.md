---
title: dekit mprocs
cli: dekit mprocs
related: [start/from-mprocs]
---

Runs the mprocs command line inside the dekit binary: `mprocs.yaml`,
`--ctl`, the classic keymap, and the rest, unchanged. Everything after
`mprocs` is passed through, including `--help`. |start/from-mprocs| lists
what changed on the way to dekit.

:::usage

```sh
dekit mprocs
dekit mprocs --config other.yaml
dekit mprocs --ctl '{c: quit}'
```
