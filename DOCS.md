# dekit documentation

Status: agreed 2026-09-24; steps 1-5 of the migration are implemented.
Supersedes the OSS side of `dekit-web/design/docs-system.md` (draft of
2026-08-30); the website side of that draft still applies except where
"Website" below says otherwise.

## Goal

One documentation tree in this repo covers everything a user or an agent
can touch: the CLI, `dekit.yaml` and the user config, the JS API, the
target grammar, runners and kernels, and (hidden for now) the RPC
protocol. The same sources reach four surfaces:

- `dekit help [topic]` in the binary: rendered for a terminal, plain
  markdown when piped (agents), JSON with `--json`.
- dekit.run/docs, rendered by dekit-web from JSON the binary exports.
- later, a help pager inside the console TUI.
- later, MCP tools, once dekit has an agent-control feature to host them.

Design notes at the repo root (PLAN.md, PROTOCOL.md, TARGETS.md, ...)
stay design notes. User docs are written for users; they are not copies
of the design notes.

## Decisions

1. **Sources live here, under `src/docs/`, in DekitDoc** (the dialect
   from the website draft, with the changes listed under "Dialect"). The
   package root is `src/` (`src/Cargo.toml`), so a tree at the repo root
   would never reach crates.io.
2. **One parser, in Rust, in this crate** (`src/help/`). The website's
   TypeScript compiler is retired; dekit-web keeps only the IR types and
   the React renderer and consumes `docs.json` produced by
   `dekit help --json`. One interpretation, no generated blob committed
   here, no cross-repo CI step.
3. **The binary embeds the markdown sources** (a dependency-free
   build.rs), not compiled JSON, and parses on demand. Sources are about
   half the size of the IR and are what agents want to read. Validation
   runs in `cargo test`; export fails on any error.
4. **Agents get the CLI.** `dekit help <topic>` on a non-TTY prints
   markdown. Nothing to install or configure, and every agent has a
   shell. MCP is deferred until dekit exposes runner control to agents;
   the docs tools then wrap the same functions.
5. **What code knows, code generates; what needs prose, docs own.** Clap
   owns the command tree (names, arguments, flags, one-line abouts) and
   the `:::usage` block pulls it in at render and export time. Docs own
   everything else. Tests enforce coverage in both directions.
6. **Identity is the path.** `docs/cli/up.md` is topic `cli/up`, URL
   `/docs/cli/up`, `dekit help cli/up`. Directories are sections.
   Aliases come from `tags` and, for CLI topics, from the command itself
   (`dekit help up`, `dekit help runner stop`).
7. **`dekit help` takes over clap's implicit `help` subcommand.**
   `dekit up --help` and `dekit --help` stay clap and are a subset of the
   topic.
8. **The website vendors `docs.json` and `schemas/dekit.json` per
   release**; `yarn docs:pull` refreshes them. Website builds are
   hermetic and the diff shows documentation changes.

## Architecture

```
src/docs/**/*.md ─src/build.rs─▶ embedded sources ─parse─▶ IR (in memory)
                                                    ├─▶ terminal renderer   dekit help up            (TTY)
                                                    ├─▶ markdown renderer   dekit help up | cat      (agents)
                                                    ├─▶ JSON via serde      dekit help --json ─▶ dekit-web src/gen/docs.json ─▶ React
                                                    └─▶ later: TUI pager (same layout code), MCP tools
```

## Repository layout

```
src/docs/
  index.yaml          sections in nav order, with titles
  index.md            home topic
  README.md           pointer for GitHub readers; skipped by the parser
  start/ cli/ config/ js/ rpc/          one directory per section
src/help/
  mod.rs              embedded table (from build.rs), load()
  ir.rs               IR types, serde Serialize, export()
  error.rs            DocError (path:line[:column]: message)
  patterns.rs         the identifier and tag matchers
  parse.rs            block parser        inline.rs   inline scanner
  compile.rs          frontmatter, identity, tags, nav, catalogs, validation
  generate.rs         :::usage from clap; later :::keymap, :::js-api
  layout.rs           IR -> lines of styled spans at a width; shared with the TUI later
  term.rs             lines -> ANSI through src/term/vt/emit.rs::sgr
  markdown.rs         IR -> markdown
  help.rs             the subcommand: resolution, search, pager
  tests.rs            the parser corpus (in-memory sources) and coverage tests
src/build.rs          walks src/docs/, emits a static table of (path, include_str!)
```

`src/build.rs` has no dependencies and does no parsing: it only makes the
tree available to the crate, with `cargo:rerun-if-changed`. Slugs use `/`
on every platform. The clap tree lives in `dekit::main::cli()` so
`generate.rs` and the tests can walk it.

`README.md` files anywhere under `src/docs/` are skipped; every other
`.md` there is a topic.

## Dialect

DekitDoc as specified in the website draft: a CommonMark subset, YAML
frontmatter, `:::` containers, `|tag|` links. The block and inline
grammar, the forbidden constructs, and the validation table carry over
unchanged unless listed here.

### Identity and frontmatter

- Topic id = slug = path under `docs/` without `.md`. `<dir>/index.md` is
  `<dir>`; `docs/index.md` is `index`, the home. Only `index.md` may sit
  at the top level; every other topic lives in a section directory.
  Nested directories nest (`cli/runner/stop`); the website nav already
  nests by path.
- `docs/index.yaml` is `sections: [{dir, title}]` in nav order. A section
  directory absent from the manifest, or an entry without a directory,
  fails validation. A section whose topics are all hidden is hidden.
- Frontmatter keys; anything else fails:

| key       | rule                                                                                                                         |
| --------- | ---------------------------------------------------------------------------------------------------------------------------- |
| `title`   | required                                                                                                                     |
| `summary` | one sentence for nav, the index, and the page description; required unless `cli` is set, forbidden when it is                |
| `cli`     | `dekit`, `dekit up`, `dekit runner stop`: binds the topic to a clap command; the summary is that command's about              |
| `tags`    | aliases, `^[a-z][a-z0-9-]*$`, globally unique across ids, tags, and record tags                                              |
| `related` | topic ids                                                                                                                    |
| `order`   | within the section; filename breaks ties                                                                                     |
| `hidden`  | reachable by exact name in `dekit help`; absent from the index, nav, search, and the export                                  |

Dropped from the draft: `id`, `slug`, `section`, `draft`, and `home:` in
the manifest.

### Blocks

As in the draft: `##`/`###` headings with optional `{#id}`, paragraphs,
fenced code, flat lists, `:::callout note|tip|warning [title=...]`,
`:::commands` (YAML `{cmd, desc, tag?}`), `:::fields kind=config|js|cli-flag`
(YAML records, required keys per kind as in the draft), `:::footnote`
(one, last), image (`![alt](src)` or `:::image`, web-only).

Void containers are one line with no closer: `:::usage`,
`:::image src=... alt=...`. The draft required a closer for `:::image`;
the corpus is adjusted when ported.

Generated blocks expand at parse time from the running binary and are
ordinary blocks in the IR, so no renderer learns a new type:

- `:::usage` requires `cli`. It expands to a `code` block with clap's
  usage line, a `fields kind=cli-flag` block with the command's own
  arguments and options (global flags excluded; they are documented once
  on the `cli` topic), and, for a group command, a `commands` block of
  its subcommands with their abouts. Every `cli` topic must contain it
  (validation), so every command page shows its synopsis and the CLI
  overview's command list can never go stale.
- Reserved, same mechanism, added with their topics: `:::keymap` (default
  console bindings from `Keymap` and `Action::name`) and
  `:::js-api <module>` (from the binding registry once the JSVM.md macro
  exists).

Unknown container types fail validation; the block set is closed.

### Inline

Unchanged: `` `code` ``, `**strong**`, `_em_` with flanking, `|tag|`,
`[label](https://...)`. The tag charset gains `/` so `|cli/up|` and
`|cli/up#semantics|` resolve.

### Validation additions

`cli` names a real clap path; every `cli` topic has `:::usage`;
`summary` and `cli` are exclusive; manifest and directories agree; a file
is at most 64 KiB; `hidden` topics may be linked only from other hidden
topics (a visible page must not dangle on the website).

## IR

Rust types in `src/docs/ir.rs` with `Serialize`. `dekit-web/src/lib/docs/ir.ts`
mirrors them and is the only contract the website has. The shape is the
draft's `DocsIR` v1 with:

- root gains `dekit: "<CARGO_PKG_VERSION>"`. Still no timestamp: the same
  sources and binary produce identical bytes; keys serialize in
  declaration order.
- `Topic` keeps `id`, `slug`, and `section` for the renderer: `id == slug`,
  `section` is the first path segment and `""` for the home.
- hidden topics are not in the export at all.
- the deduped `commands[]` and `fields[]` catalogs stay, for search and
  MCP later.

## `dekit help`

- `dekit help`: the index. Sections with `slug  summary` per topic, then
  `Run dekit help <topic>. dekit <command> --help shows flags only.`
- `dekit help <words...>`: the words are joined with spaces and resolved
  in this order: `dekit <words>` matches a `cli` topic; exact id; tag;
  unique last path segment (`up` resolves to `cli/up`); title,
  case-insensitively; `id#heading`. No match: search titles, tags,
  headings, and record keys, print the candidates, exit 1.
- TTY output: layout at `min(terminal width, 100)` columns. Headings bold,
  inline code colored, callouts as a left bar with a colored label,
  commands and fields as definition lists (bold key, dim type and
  default on the same line, wrapped description indented), code indented,
  related topics as `dekit help x` lines at the end. `NO_COLOR` and
  `TERM=dumb` turn styling off. The text goes through `$PAGER` (default
  `less`, with `LESS=FRX` when unset, so a short topic never waits for
  `q`); an unusable pager falls back to plain printing.
- Not a TTY: markdown re-rendered from the IR. Generated blocks are
  expanded, callouts become `> **Note:**` quotes, records become GFM
  tables, `|tag|` becomes `` `tag` `` plus a trailing "Related" list of
  `dekit help` commands so an agent knows how to follow a link.
- `--json` is the existing global flag. With no topic it exports the whole
  IR (the website's input); with a topic, that topic. Hidden topics are
  exported only when named explicitly.
- Never contacts the runner; the docs are those of the invoking binary.
- Clap: `disable_help_subcommand(true)` and an own `help` subcommand with
  trailing words. `after_help` shrinks to a pointer at `dekit help targets`
  and `dekit help`; the TARGETS and BRINGING TASKS DOWN prose moves to
  `start/targets`.

## Code and docs

Direction rules:

- clap to docs: command tree, abouts, argument and flag help, through
  `:::usage`. Nothing in `main.rs` describes a command beyond its
  one-liner and its arguments.
- docs generate nothing back into code. `schemas/dekit.json` and
  `src/js/lib/std.d.ts` stay hand-written; tests keep the sets equal.

Coverage tests in `src/docs/tests.rs`:

- every clap subcommand path has exactly one `cli` topic, and vice versa;
- `ROOT_KEYS`, `TASK_KEYS`, and `TASK_SETTING_KEYS` equal the keys of the
  `fields kind=config` records (prefix stripped) and the properties in
  `schemas/dekit.json`, per object. This already catches `stop` and `log`
  missing from the schema today;
- every function reachable from the `std` global in a test VM has a
  `fields kind=js` record;
- every fixture parses to its golden IR; every forbidden fixture fails
  with the expected message.

CI needs nothing new: `cargo test` already runs on three platforms.

## Website (dekit-web)

Keep `ir.ts`, `load.ts`, `nav-tree.ts`, `components/docs/*`, and the
routes. Remove `lib/docs/compiler/*`, `scripts/docs-compile.ts`, the
`docs/` tree, and the `yaml` devDependency. Change:

- `vite-plugin-dekit-docs.ts` stops compiling. It serves
  `src/gen/docs.json` (path overridable with `DEKIT_DOCS_JSON`) as
  `virtual:dekit-docs` and watches the file.
- `yarn docs:pull [--from <binary>]` runs `dekit help --json` into
  `src/gen/docs.json` and copies `schemas/dekit.json` to
  `public/schemas/dekit.json` (its `$id` already names that URL). Images
  are `https://` URLs only. Default source is
  `cargo run --manifest-path ../mprocs/src/Cargo.toml -- help --json`;
  for a release, the installed binary of `SITE.version`.
- `src/gen/` is committed. The docs footer shows `docs for dekit <ir.dekit>`.
- The six starter pages move to `mprocs/docs` with corrections: host
  runner wording per PLAN.md, the full `runner` subcommand list, and the
  "`dekit help` (later)" notes.
- `design/docs-system.md` gets a status line pointing here.

## Agents

`dekit help` is the agent surface: markdown when piped, `--json` when
structure is wanted. A `start/agents` topic explains driving dekit from
an agent: `--json` shapes for `ls` and `why`, targets, `screen` to read a
task's output, `run` for one-offs, exit codes, and a snippet for a
CLAUDE.md. MCP later: `docs_get` and `docs_search` over the same
functions, inside the agent-control feature, plus the runner-control
tools that feature is actually about.

## TUI, later

`layout.rs` is the seam: `fn layout(topic, width) -> Vec<Line>` where a
`Line` is spans of `(String, Attrs)` using `src/term/attrs.rs`. stdout
converts with `emit::sgr`; the console draws the same lines into its
grid. Pager behavior (j/k, gg/G, `/` search, tag jump with history) as in
the draft's TUI section, opened by a console action `OpenHelp { topic }`.

## Content outline

| section          | topics                                                                                                                            |
| ---------------- | --------------------------------------------------------------------------------------------------------------------------------- |
| `index`          | home                                                                                                                              |
| `start`          | getting-started, targets, runners (project and host, records, `runner` verbs), agents, from-mprocs                                |
| `cli`            | index (global flags, `.js` files, exit codes, `--json`) and one topic per subcommand, with `runner/` and `kernel/` nested         |
| `config`         | index (dekit.yaml, fragments, user config.yaml), tasks (every task key), hooks, kernel, load, user (tui and keymap), schema (link) |
| `js`             | index (running scripts), std, fs, path, env, process, tui, dekit                                                                  |
| `rpc` (hidden)   | protocol, framing, requests, attach; written from PROTOCOL.md                                                                     |

## Migration

Each step ships on its own.

1. Parser, IR, validation, fixtures, build.rs embed; `docs/` seeded from
   the six starter pages; `cargo test` gates. No CLI change yet.
2. `dekit help`: resolution, terminal and markdown output, `--json`, the
   clap takeover, `after_help` trimmed.
3. Coverage tests and the full CLI and config topic set, `:::usage` in
   every command topic.
4. dekit-web: retire the compiler, `docs:pull`, vendored JSON and schema,
   deploy.
5. `js` and `rpc` sections; the JS coverage test.
6. Later: `:::keymap`, the TUI pager, MCP.

## Resolved questions

- Long topics on a TTY go through `$PAGER`, as git does.
- `summary` cannot override clap's about on a `cli` topic: one source.
- Terminal width is capped at 100 columns.
