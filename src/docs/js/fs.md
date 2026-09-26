---
title: std.fs
summary: Asynchronous file system access for scripts.
related: [js, js/path]
order: 20
---

Every function returns a promise. Relative paths start from the script's
working directory; |js/path| builds them.

```js
export async function main() {
  await std.fs.mkdir("out", { recursive: true });
  await std.fs.write("out/hello.txt", "hello\n");
  std.log(await std.fs.read("out/hello.txt"));
}
```

:::fields kind=js
- key: std.fs.read
  signature: "(path: string) => Promise<string>"
  desc: Read a whole file as a UTF-8 string.
- key: std.fs.write
  signature: "(path: string, content: string) => Promise<void>"
  desc: Write a string to a file, creating or replacing it.
- key: std.fs.exists
  signature: "(path: string) => Promise<boolean>"
  desc: Whether a path exists.
- key: std.fs.mkdir
  signature: "(path: string, opts?: {recursive?: boolean}) => Promise<void>"
  desc: Create a directory; `recursive` creates missing parents too.
- key: std.fs.rm
  signature: "(path: string, opts?: {recursive?: boolean}) => Promise<void>"
  desc: Remove a file or an empty directory; `recursive` also removes a directory with contents.
- key: std.fs.readDir
  signature: "(path: string) => Promise<string[]>"
  desc: The names of the entries in a directory.
- key: std.fs.stat
  signature: "(path: string) => Promise<{size, mtime, isDir, isFile, isSymlink}>"
  desc: Facts about a path, following symlinks; `mtime` is in milliseconds since the Unix epoch.
- key: std.fs.rename
  signature: "(from: string, to: string) => Promise<void>"
  desc: Rename or move a file or directory.
- key: std.fs.copy
  signature: "(from: string, to: string) => Promise<void>"
  desc: Copy a file.
:::
