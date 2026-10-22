---
title: Attach sessions
summary: Attaching a connection to a task's screen, the screen byte stream, and input events.
related: [rpc, rpc/requests]
hidden: true
order: 20
---

`attach` must be the first and only request on its connection. It
attaches the connection to the screen of any screen-bearing task; the
terminal client defaults to `@dekit/console`, the runner's built-in
console. After the ok response the connection is in session mode until
`bye` (`quit`), sent whether the client detached, the task went away, or
the runner quit.

With `until_exit: true` the server also ends the session when the
attached task's execution finishes, after the final screen state is
painted. It has finished when it has no process and won't start by
itself (|rpc/requests#waits|), so a task still waiting on its deps has
not. The session ends with `bye` (`task_exited`) carrying the task's
final `state` and final `screen` in optional fields. The runner removes
the attached task at that point, which is what the foreground `run` verb
relies on.

The console's runner selector ends a session with `bye` (`quit`)
carrying `switch: {kind, root}`: the client connects to that runner
(`project` or `host`, at the absolute `root`) and attaches to its console,
starting the runner when it is not up.

## Screen stream

`Out` frames carry a terminal byte stream: the client owns a raw-mode
terminal and writes every frame to it verbatim. The server keeps the
client's terminal in sync with the screen by emitting cursor moves, SGR,
cursor visibility and style, and text: a delta per flush, a full repaint
whenever it chooses. It addresses cells from the top-left corner and never
scrolls the client terminal; the screen renders at the smallest attached
size and cells outside it are left alone. Bell is `BEL`, a title change is
OSC 2. Copied text is not in the stream: it arrives as a `copy` event.
A client that is not a terminal runs the stream through a terminal
emulator; there is no cell-level encoding.

## Geometry

`width` and `height` on `attach` are the client's size in cells;
`cell_width` and `cell_height` the size of one cell in pixels, when the
client's terminal said. Later sizes come as `resize` events. The runner
gives the task's pty the pixel size the cell size implies, so programs
can size images; a client that never says a cell size leaves the pty
without one.

## Terminal capabilities

`caps` on `attach` is what the client's terminal can show, as the client
measured it; the runner paints the session accordingly. Every field has
a default, so a client may send any subset. One that sends none is
painted as a true-color terminal with `italic`, `strikethrough`, and
`hyperlinks`.

- `colors`: `ansi16`, `ansi256`, or `true-color`.
- `features`: a list of names. A feature listed is drawn and one not listed is not; a name the runner does not know is dropped. The names: `italic` (SGR 3), `strikethrough` (SGR 9), `underline-styles` (SGR 4:2 to 4:5), `underline-colors` (SGR 58), `hyperlinks` (OSC 8), `sync` (mode 2026, paints wrapped so they show at once), `focus` (mode 1004, focus events arrive), `sixel`.
- `kitty`: `{keyboard, graphics?}`: the kitty keyboard protocol flags the terminal accepted, and when it speaks the graphics protocol, a list of what of it works beyond placing images: `placeholders` (Unicode placeholders) and `animation` (frames and controls).
- `widths`: `{vs16_wide, zwj_joins, flags_wide}`: whether the terminal shows an emoji with VS16, a ZWJ sequence, and a pair of regional indicators as two columns, each `true`, `false`, or left out when not measured. Cells not known to agree are addressed explicitly, which is safe on any terminal.

## Client events

- `key` with `{code, mods, kind, state, unshifted?, base?, text?}`. `code` is a key name such as `Esc` or `Enter`, or `{Char: c}`, `{F: n}`, `{Media: name}`, `{Modifier: name}`. `mods` and `state` are lists of names, left out when empty: `shift`, `control`, `alt`, `super`, `hyper`, `meta`, and `keypad`, `caps-lock`, `num-lock`; a name the runner does not know is dropped, not the key. `kind` is `Press`, `Repeat`, or `Release`. `unshifted`, `base`, and `text` carry what the kitty keyboard protocol reports: the key without Shift, the same key on a US layout, and the text the key produced.
- `mouse` with `{kind, x, y, mods}`, `mods` as for `key`.
- `paste` with `{text}`.
- `focus` with `{gained}`.
- `resize` with `{width, height, cell_width?, cell_height?}`: the client's new size in cells and, when its terminal said, the size of a cell in pixels.
- `screen` with a screen command as params: `scroll {delta, unit}`, `copy-enter`, `copy-leave`, `copy-move {dir}`, `copy-select`, `copy-yank`. These act on the attached screen for every observer, as a tmux window would.

Keys, mouse, and pastes are interpreted by the attached task: a process
task feeds them to its pty (in copy mode they drive the selection), the
console treats them as UI input. Events that fail to decode are dropped,
not fatal.

## Server events

- `copy` with `{text}`: text copied from the attached screen, for the client to put on the user's clipboard.
