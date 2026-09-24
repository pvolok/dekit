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
painted, with `bye` (`task_exited`) carrying the task's final `state` and
final `screen` in optional fields. The runner removes the attached task at
that point, which is what the foreground `run` verb relies on.

## Screen stream

`Out` frames carry a terminal byte stream: the client owns a raw-mode
terminal and writes every frame to it verbatim. The server keeps the
client's terminal in sync with the screen by emitting cursor moves, SGR,
cursor visibility and style, and text: a delta per flush, a full repaint
whenever it chooses. It addresses cells from the top-left corner and never
scrolls the client terminal; the screen renders at the smallest attached
size and cells outside it are left alone. Bell is `BEL`, a title change is
OSC 2, copied text is OSC 52. A client that is not a terminal runs the
stream through a terminal emulator; there is no cell-level encoding.

## Client events

- `input` with a terminal event as params; resizes travel as a resize event. Keys, mouse, and pastes are interpreted by the attached task: a process task feeds them to its pty (in copy mode they drive the selection), the console treats them as UI input.
- `screen` with a screen command as params: `scroll {delta, unit}`, `copy-enter`, `copy-leave`, `copy-move {dir}`, `copy-select`, `copy-yank`. These act on the attached screen for every observer, as a tmux window would.

Events that fail to decode are dropped, not fatal.
