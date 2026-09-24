---
title: User config
summary: Key bindings and TUI settings in the user config.yaml, shared by every project.
related: [config, cli/attach]
order: 50
---

Presentation settings are yours, not a project's, so `dekit.yaml` rejects
them. They live in the user config: `$XDG_CONFIG_HOME/dekit/config.yaml`
(by default `~/.config/dekit/config.yaml`; `%APPDATA%\dekit\config.yaml`
on Windows). Every runner reads it. A broken user config is reported as a
warning and skipped rather than stopping a runner.

```yaml
tui:
  sidebar:
    title: Services
    width: 24
keymap:
  tasks:
    <C-r>: restart-task
    <C-d>: {action: scroll-down, n: 3, unit: line}
    <e>: null
```

## TUI

:::fields kind=config
- key: tui.sidebar.title
  type: string
  default: Tasks
  desc: Title of the task list.
- key: tui.sidebar.width
  type: integer
  default: "30"
  desc: Width of the task list in columns.
- key: tui.tips.show
  type: boolean
  default: "true"
  desc: Show the key tips line.
- key: tui.zoom_tip
  type: boolean
  default: "true"
  desc: Show the hint about zooming.
:::

## Keymap

`keymap` has three groups: `tasks` (the task list has focus), `term` (the
task's terminal has focus), and `term_copy` (copy mode). Each maps a key
to an action: a bare name for actions without arguments, or an object with
`action` and its fields. `null` unbinds a key, and `reset: true` at the
top of a group drops the defaults before yours apply. Keys are written as
`<q>`, `<C-a>`, `<M-1>`, `<Down>`, `<F5>`.

Default bindings in `tasks`:

:::commands
- cmd: <C-a>
  desc: toggle-focus, between the task list and the terminal (in every group)
- cmd: <q>, <Q>
  desc: quit, force-quit
- cmd: <j> <Down>, <k> <Up>
  desc: next-task, prev-task
- cmd: <M-1> ... <M-8>
  desc: "select-task {index: 0} ... 7"
- cmd: <s>, <x>, <X>
  desc: start-task, stop-task, kill-task
- cmd: <r>, <R>
  desc: restart-task, force-restart-task
- cmd: <a>, <C>, <d>, <e>
  desc: show-add-task, duplicate-task, show-remove-task, show-rename-task
- cmd: <p>
  desc: show-commands-menu
- cmd: <z>
  desc: zoom
- cmd: <v>
  desc: copy-mode-enter
- cmd: <?>
  desc: toggle-keymap-window
- cmd: <C-c>
  desc: "send-key {key: <C-c>}, to the task"
- cmd: <C-y>, <C-e>
  desc: "scroll-up, scroll-down {n: 3, unit: line}"
- cmd: <C-u>, <C-d>
  desc: "scroll-up, scroll-down {unit: half-screen}"
- cmd: <PageUp>, <PageDown>
  desc: "scroll-up, scroll-down {unit: screen}"
:::

In `term_copy`: `<Esc>` copy-mode-leave, `<v>` copy-mode-end, `<c>`
copy-mode-copy, and `<h>` `<j>` `<k>` `<l>` or the arrows copy-mode-move.
The scroll bindings work there too. Other actions available to a binding:
focus-tasks, focus-term, restart-all, force-restart-all, veto-task,
close-current-modal, quit-or-ask, and `{action: command, command: {...}}`
to run any kernel command (|config/hooks|).
