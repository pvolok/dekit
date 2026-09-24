---
title: Fragments
summary: Split dekit.yaml with load, and mount a fragment's tasks at a path.
related: [config, config/tasks]
order: 20
---

`load` lists globs, relative to the declaring file. Each matched file is a
fragment: it may contain `tasks` and further `load` entries, nothing else.
Matches load in sorted order.

```yaml
load:
  - services/*.dekit.yaml
  - file: packages/web/tasks.yaml
    at: web
```

`at` mounts the fragment's tasks under a path: a task `dev` in the second
fragment becomes `web/dev`, and `deps` inside that fragment are rebased the
same way, so `deps: [db]` there means `web/db`. A dep that starts with `/`
addresses the project root instead: `deps: [/db]`.

:::callout note
A fragment must stay inside the project root, a glob that matches no file
is an error, and so are load cycles and duplicate task paths. A file named
`dekit.yaml` is never a fragment: it is another project.
:::
