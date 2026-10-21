---
title: User config
summary: Key bindings and TUI settings in the user config.yaml, shared by every project.
related: [config, cli/attach]
order: 50
---

Key bindings and TUI settings are yours, so they live in your user config
rather than in `dekit.yaml`: `~/.config/dekit/config.yaml`
(`$XDG_CONFIG_HOME/dekit/config.yaml` when that is set,
`%APPDATA%\dekit\config.yaml` on Windows). A runner reads it when it
starts. If the file has an error, dekit warns and skips that part.

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
  desc: Show the key hints at the bottom. `<?>` toggles them.
- key: tui.zoom_tip
  type: boolean
  default: "true"
  desc: Show the hint line while a task is zoomed.
- key: tui.theme
  type: auto | dark | light
  default: auto
  desc: Console colors. `auto` follows the terminal's background (asked with OSC 11, else `COLORFGBG`), and is dark when neither says.
:::

## Agent

The console can hand a write-back the file editor can't do safely to a
coding agent: it runs the agent as a task, with a prompt that lists the
changes as its last argument, and once the task ends, checks that the
config reads back as asked (|start/runners|).

:::fields kind=config
- key: agent.cmd
  type: "string | string[]"
  default: "[claude] when it is installed"
  desc: The agent's command; the prompt is added after it.
:::

## Keymap

`keymap` has three groups: `tasks` when the task list has focus, `term`
when the task's terminal has focus, and `term_copy` in copy mode. Each
maps a key to an action: a name, or an object with `action` and its
fields. `null` removes a binding, and `reset: true` drops a group's
defaults. Keys look like `<q>`, `<C-a>`, `<M-1>`, `<Down>`, `<F5>`.

Default bindings in `tasks`:

:::commands
- cmd: <C-a>
  desc: toggle-focus, between the task list and the terminal (in every group)
- cmd: <q>, <Q>
  desc: show-quit
- cmd: <j> <Down>, <k> <Up>
  desc: next-task, prev-task
- cmd: <h> <Left>, <l> <Right>
  desc: collapse, expand, a group in the task list
- cmd: <H>, <L>
  desc: "fold {op: close, target: '**'}, fold {op: open, target: '**'}: every group"
- cmd: <M-1> ... <M-8>
  desc: "select-task {index: 0} ... 7"
- cmd: <s>, <x>, <X>
  desc: start-task, stop-task, kill-task
- cmd: <r>, <R>
  desc: restart-task, force-restart-task
- cmd: <a>, <C>, <d>, <e>
  desc: show-add-task, show-duplicate-task, show-remove-task, show-edit-task
- cmd: <Space>
  desc: show-task-menu, also on a right click
- cmd: <K>, <J>
  desc: move-task-up, move-task-down, beside the tasks of its group
- cmd: <y>
  desc: copy-task-command
- cmd: <p>
  desc: show-commands-menu
- cmd: <P>
  desc: show-runner-select
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
copy-mode-copy, `<h>` `<j>` `<k>` `<l>` or the arrows copy-mode-move, and
the same scroll keys. Other actions: focus-tasks, focus-term, restart-all,
force-restart-all, veto-task, close-current-modal, duplicate-task (a
copy without the dialog), show-reload, show-write-back, save-to-config,
show-main-menu (the
menu under the `dekit` badge, also by clicking it), detach, quit (stops
the runner without saving, like |cli/runner/stop|),
`{action: show-runner-select, tab: open}` to open a project from any
directory, `{action: switch-runner, kind: project, root: /abs/path}` to
move the attachment to another runner's console, and
`{action: command, command: {...}}` to run any command from
|config/hooks|.

`fold` opens, closes, or toggles groups: `{action: fold, op: close}`
closes the selected group, or the one the selected task is in, and
`{action: fold, op: toggle, target: 'apps/*'}` the groups a path or glob
names, as a target does (|start/targets|). `op` is `open`, `close`, or
`toggle`; toggling several closes them unless they are all closed.
