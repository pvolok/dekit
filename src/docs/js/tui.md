---
title: std.tui
summary: Draw a terminal UI from a script, frame by frame, and read its input.
related: [js, cli/attach]
hidden: true
order: 60
---

:::callout warning
`std.tui` is being redesigned and is only in debug builds for now.
:::

A script can take over its terminal: `open` switches to the alternate
screen and raw input, `draw` paints one frame, and `input` waits for the
next event. A `script:` task draws into its own screen in the runner, so
`dekit attach <task>` shows it like any other task.

```js
export async function main() {
  std.tui.open();
  for (;;) {
    const { width, height } = std.tui.size();
    std.tui.draw((frame) => {
      frame.clear();
      frame.text(0, 0, `${width}x${height}, press q to quit`, { bold: true });
    });
    const event = await std.tui.input(1000);
    if (event === null || (event.type === "key" && event.key === "<q>")) break;
  }
  std.tui.close();
}
```

:::fields kind=js
- key: std.tui.open
  signature: "() => void"
  desc: Enter terminal UI mode; calling it again does nothing.
- key: std.tui.close
  signature: "() => void"
  desc: Leave terminal UI mode and restore the terminal.
- key: std.tui.size
  signature: "() => {width: number, height: number}"
  desc: The terminal size in cells.
- key: std.tui.input
  signature: "(timeoutMs?: number) => Promise<Event | null>"
  desc: "The next event: `{type: \"key\", key, kind}` with keys like `<q>` or `<C-c>`, `{type: \"mouse\", x, y, kind}`, `{type: \"resize\", width, height}`, `{type: \"focus\", focused}`, `{type: \"paste\", text}`, `{type: \"timeout\"}`, or `null` when input ends."
- key: std.tui.draw
  signature: "(cb: (frame: Frame) => void) => void"
  desc: Paint one frame; the callback gets a blank frame that is valid only during the call.
:::

## The frame

Inside `draw`, the frame has `width` and `height` and these methods:

:::fields kind=js
- key: frame.text
  signature: "(x: number, y: number, text: string, style?: Style) => void"
  desc: Draw text on one line from a cell, cut off at the right edge. `Style` has `fg` and `bg` (a color name like `red` or `brightBlue`, an index 0-255, or `{r, g, b}`), `bold`, `italic`, `underline`, and `inverse`.
- key: frame.clear
  signature: "(ch?: string, style?: Style) => void"
  desc: Fill the whole frame with a character, a space by default.
- key: frame.hideCursor
  signature: "() => void"
  desc: Hide the cursor for this frame.
- key: frame.setCursor
  signature: "(x: number, y: number) => void"
  desc: Show the cursor at a cell for this frame.
- key: frame.setCursorStyle
  signature: "(style: CursorStyle) => void"
  desc: "The cursor shape for this frame: `default`, `blinkingBlock`, `steadyBlock`, `blinkingUnderline`, `steadyUnderline`, `blinkingBar`, or `steadyBar`."
:::
