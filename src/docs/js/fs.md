---
title: std.fs
summary: Asynchronous file system access for scripts.
related: [js, js/path]
order: 20
---

Every function returns a promise. Paths are resolved against the script's
working directory; |js/path| builds them.

:::fields kind=js
- key: std.fs.read
  signature: "(path: string) => Promise<string>"
  desc: Read a file's entire contents as a UTF-8 string.
- key: std.fs.write
  signature: "(path: string, content: string) => Promise<void>"
  desc: Write a string to a file, creating or overwriting it.
- key: std.fs.exists
  signature: "(path: string) => Promise<boolean>"
  desc: Whether a path exists.
- key: std.fs.mkdir
  signature: "(path: string, opts?: {recursive?: boolean}) => Promise<void>"
  desc: Create a directory; `recursive` creates parents too.
- key: std.fs.rm
  signature: "(path: string, opts?: {recursive?: boolean}) => Promise<void>"
  desc: Remove a file or directory; `recursive` removes a non-empty directory.
- key: std.fs.readDir
  signature: "(path: string) => Promise<string[]>"
  desc: The names of a directory's entries.
- key: std.fs.stat
  signature: "(path: string) => Promise<{size, mtime, isDir, isFile, isSymlink}>"
  desc: Metadata for a file or directory; `mtime` is a Unix timestamp.
- key: std.fs.rename
  signature: "(from: string, to: string) => Promise<void>"
  desc: Rename or move a file or directory.
- key: std.fs.copy
  signature: "(from: string, to: string) => Promise<void>"
  desc: Copy a file.
:::
