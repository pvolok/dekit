---
title: std.path
summary: Path manipulation without touching the file system.
related: [js, js/fs]
order: 30
---

All functions are synchronous and never read the disk.

:::fields kind=js
- key: std.path.join
  signature: "(...parts: string[]) => string"
  desc: Join segments into one path.
- key: std.path.dirname
  signature: "(path: string) => string"
  desc: The parent directory of a path.
- key: std.path.basename
  signature: "(path: string) => string"
  desc: The last component of a path.
- key: std.path.extname
  signature: "(path: string) => string"
  desc: The extension including its dot, such as `.ts`.
- key: std.path.resolve
  signature: "(...parts: string[]) => string"
  desc: Resolve segments against the current working directory.
- key: std.path.isAbsolute
  signature: "(path: string) => boolean"
  desc: Whether a path is absolute.
:::
