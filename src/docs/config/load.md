---
title: Fragments
summary: Split dekit.yaml with load, and mount a fragment's tasks at a path.
related: [config, config/tasks]
order: 20
---

`load` pulls tasks from other files, called fragments. Each entry is a
glob relative to the declaring file. A fragment may contain only `tasks`
and its own `load`.

```yaml
load:
  - services/*.yaml
  - file: packages/web/tasks.yaml
    at: web
```

`at` puts the fragment's tasks under a path: a task `dev` there becomes
`web/dev`. Its `deps` move the same way, so `deps: [db]` means `web/db`;
write `deps: [/db]` for the project's own `db`.

A glob that matches no file is an error, and so are two tasks with the
same path. A fragment must be inside the project and must not be named
`dekit.yaml`.
