---
title: JavaScript values
summary: Compute a value in dekit.yaml with !js, and share values between them with vars.
related: [config, config/load, js/path, cli/reload]
order: 25
---

A value tagged `!js` is a JavaScript expression. dekit evaluates it when
it loads the config, and its value takes the tag's place, as if it were
written there.

```yaml
vars:
  data: !js std.path.join(ctx.runner.root, ".data")
tasks:
  db:
    cmd: !js '["postgres", "-D", ctx.vars.data + "/db"]'
  api:
    cmd: cargo run -p api
    env:
      FIXTURES: !js std.path.join(ctx.dir, "fixtures")
      DATA_DIR: !js ctx.vars.data
```

The value is data: a string, number, boolean, null, array, or plain
object. A key reads it as it reads a value written in YAML, so `cmd`
takes an array or a string, and `autostart` a boolean. Any value can be
`!js`, in dekit.yaml and in fragments (|config/load|), but for `kernel`,
which picks the dekit that evaluates it. A key can't be.

An expression has no effects. It sees `ctx` and `std.path`, and nothing
that reads files or the environment, runs programs, or talks to the
runner. It runs synchronously, so it can't `await`, and for a second at
most. Loading the config twice gives the same values.

## ctx

:::fields kind=js
- key: ctx.dir
  signature: string
  desc: The directory of the file the value is in, which the file's relative paths are from.
- key: ctx.runner.root
  signature: string
  desc: The runner's root, the project's directory or the host runner's (|start/runners|).
- key: ctx.runner.kind
  signature: '"project" | "host"'
  desc: Which kind of runner loads the config.
- key: ctx.vars
  signature: "Record<string, unknown>"
  desc: The values of `vars`, as their `!js` computed them.
:::

`ctx` can't be changed. `std.path` is the one scripts have (|js/path|),
without `resolve`, which reads the working directory; join onto `ctx.dir`
instead.

## Vars {#vars}

`vars` in dekit.yaml names values for `!js` to read as `ctx.vars`. A var
is any value, plain or `!js`, and dekit reads them in order, so a `!js`
var sees the vars above it. The block can be one `!js` too, an expression
that returns an object, which sees `ctx.vars` empty. Fragments read them
too; only dekit.yaml sets them.

```yaml
vars:
  data: !js std.path.join(ctx.runner.root, ".data")
  db: !js ctx.vars.data + "/db"
  ports: {api: 3000, web: 3001}
tasks:
  api:
    cmd: !js '["cargo", "run", "--", "--port", String(ctx.vars.ports.api)]'
    ready: {http: !js '`http://localhost:${ctx.vars.ports.api}/health`'}
```

A value every task's environment needs is simpler in `defaults: {env:
...}` (|config|).

## When it runs

dekit evaluates `!js` when a runner starts fresh, and again for each
|cli/reload|, which then shows the values that changed. A restart, `down`
and `up`, and an upgrade bring the tasks back as they were, values and
all. When the console writes a task back to dekit.yaml, it leaves a task
with a `!js` value to the coding agent (|config/user|), so the
expression is never replaced by what it computed.

## Writing one

YAML reads the expression first, so one that starts with `[`, `{`, `'`,
`"`, or `` ` ``, or has `: ` or ` #` in it, goes in quotes, as in
`!js '["serve", "--port", "3000"]'`. In a flow mapping or list, such as
`env: {DATA: ...}`, a comma ends it too. An object needs no parentheses:
`!js '{port: 3000}'`. A longer one fits a block:

```yaml
env:
  LOG_DIR: !js |
    ctx.runner.kind === "host"
      ? std.path.join(ctx.runner.root, "logs")
      : std.path.join(ctx.dir, ".logs")
```

An error names where the value is:

```sh
!js: ReferenceError: dri is not defined at <config>.tasks.api.env.FOO_DIR
```

An editor that checks dekit.yaml against its JSON Schema reports `!js`
as an unknown tag until it is told about it. With the YAML extension in
VS Code, add `"yaml.customTags": ["!js scalar"]` to the settings.
