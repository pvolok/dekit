---
title: std.tui
summary: Draw a terminal UI from a script, frame by frame, and read its input.
related: [js, cli/attach]
order: 60
---

A script can take over the terminal it runs in: `open` switches to the
alternate screen and raw input, `draw` paints one frame through a
callback, and `input` waits for the next key, mouse, resize, focus, or
paste event. A `script:` task draws into its own screen in the runner, so
`dekit attach <task>` shows it like any other task.

```js
std.tui.open();
for (;;) {
  const { width, height } = std.tui.size();
  std.tui.draw((frame) => {
    frame.clear();
    frame.text(0, 0, `${width}x${height}`, { bold: true });
  });
  const event = await std.tui.input(1000);
  if (event?.type === "key" && event.key === "q") break;
}
std.tui.close();
```

:::fields kind=js
- key: std.tui.open
  signature: "() => void"
  desc: Enter terminal UI mode; safe to call more than once.
- key: std.tui.close
  signature: "() => void"
  desc: Leave terminal UI mode and restore the terminal.
- key: std.tui.size
  signature: "() => {width: number, height: number}"
  desc: The current terminal size in cells.
- key: std.tui.input
  signature: "(timeoutMs?: number) => Promise<Event | null>"
  desc: "The next input event: `{type: \"key\", key, kind}`, `{type: \"mouse\", x, y, kind}`, `{type: \"resize\", width, height}`, `{type: \"focus\", focused}`, `{type: \"paste\", text}`, or `{type: \"timeout\"}`."
- key: std.tui.draw
  signature: "(cb: (frame: Frame) => void) => void"
  desc: Paint one frame; the callback receives a transient frame object.
:::

## The frame

Inside `draw`, the frame has `width` and `height` and these methods:

:::fields kind=js
- key: frame.text
  signature: "(x: number, y: number, text: string, style?: Style) => void"
  desc: Draw text at a cell. `Style` has `fg`, `bg` (a color name, an index, or `{r, g, b}`), `bold`, `italic`, `underline`, and `inverse`.
- key: frame.clear
  signature: "(ch?: string, style?: Style) => void"
  desc: Fill the whole frame with a character.
- key: frame.hideCursor
  signature: "() => void"
  desc: Hide the cursor for this frame.
- key: frame.setCursor
  signature: "(x: number, y: number) => void"
  desc: Show the cursor at a cell for this frame.
- key: frame.setCursorStyle
  signature: "(style: CursorStyle) => void"
  desc: One of default, blinkingBlock, steadyBlock, blinkingUnderline, steadyUnderline, blinkingBar, steadyBar.
:::
