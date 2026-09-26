---
title: std.path
summary: Path manipulation without touching the file system.
related: [js, js/fs]
order: 30
---

All functions are synchronous and never read the disk.

```js
export function main() {
  const file = std.path.join("src", "main.ts");
  std.log(std.path.dirname(file), std.path.extname(file));
}
```

:::fields kind=js
- key: std.path.join
  signature: "(...parts: string[]) => string"
  desc: Join parts into one path.
- key: std.path.dirname
  signature: "(path: string) => string"
  desc: The parent directory of a path.
- key: std.path.basename
  signature: "(path: string) => string"
  desc: The last part of a path.
- key: std.path.extname
  signature: "(path: string) => string"
  desc: The extension with its dot, such as `.ts`, or an empty string.
- key: std.path.resolve
  signature: "(...parts: string[]) => string"
  desc: Join parts onto the current working directory into an absolute path.
- key: std.path.isAbsolute
  signature: "(path: string) => boolean"
  desc: Whether a path is absolute.
:::
