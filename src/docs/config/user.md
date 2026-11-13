---
title: User config
summary: Key bindings, TUI settings, and your projects in the user config.yaml, shared by every project.
related: [config, cli/attach, cli/project]
order: 50
---

Key bindings, TUI settings, and the list of your projects are yours, so
they live in your user config rather than in `dekit.yaml`: `~/.config/dekit/config.yaml`
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
    e: null
    gg: {action: select-task, index: 0}
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

## Projects

`projects` lists the projects you switch between, so the console's
runner list (`<P>`) can show them and open one whether or not its runner
is up. An entry is a project root, or a dir and `/*` for every dir in it:

```yaml
projects:
  - ~/dev/*
  - ~/oss/mprocs
```

Hidden dirs are left out. Entries are absolute or start with `~/`; the
home dir itself is `"~"`, quoted, since a bare `~` is YAML's null. A
project goes by its folder's name. |cli/project/list| shows what the
list comes to.

:::fields kind=config
- key: projects.*
  type: string
  desc: A project root, or `dir/*` for every dir in `dir`.
:::

In the console, `<P>` lists the current runner, then the projects, then
the other running runners.

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

`keymap` has four groups: `tasks` when the task list has focus, `term`
when the task's terminal has focus, `term_copy` in copy mode, and
`global`, which holds in all three, after the group's own. Each maps
keys to an action: a name, or an object with `action` and its fields.
`null` removes a binding, and `reset: true` drops a group's defaults.

Keys are written as in vim. A character is its key, and `<...>` names
the others: `<C-a>`, `<M-1>`, `<Down>`, `<F5>`, `<Space>`, `<lt>` for
`<`. A name without its `<>` is an error, not its letters. Quote a key
that YAML would read as something else: `"?"`.

Several keys in a row are typed one after another: `gg`, `<C-w>v`.
Nothing times them out. The console waits for the next key, the hints
at the bottom show the keys that can follow, and any other key, `<Esc>`
for one, drops what was typed. So a group binds a key or the sequences
that start with it, not both: a binding takes the place of the ones it
starts and of the ones that start it. Of two such in one file the later
one stays, with a warning.

### Leader

`<leader>` in a binding is the leader, `` ` `` unless `keymap.leader`
names another key. The default bindings in `global` start with it, so they also work in the
task's terminal, where every other key goes to the task:

:::commands
- cmd: <leader>j, <leader>k
  desc: next-task, prev-task
- cmd: <leader>h, <leader>l
  desc: focus-tasks, focus-term, as the two sit on screen
- cmd: <leader>1 ... <leader>9
  desc: "select-task {index: 0} ... 8"
- cmd: <leader>z
  desc: zoom
- cmd: "<leader>["
  desc: copy-mode-enter
- cmd: "<leader>:"
  desc: show-commands-menu
- cmd: <leader>s
  desc: show-runner-select
- cmd: <leader>?
  desc: toggle-keymap-window
- cmd: <leader>d
  desc: detach
- cmd: <leader>q
  desc: show-quit
- cmd: <leader>t, then s, x, X, r, R, e, a, C, d, y
  desc: what the key does to the selected task in `tasks`, as `<leader>tr` is restart-task
- cmd: <leader><leader>
  desc: send-leader, which types the leader's own character into the task
:::

`keymap.leader` names another key, which stops being whatever the
groups had it as, and `global` takes any binding:

```yaml
keymap:
  leader: <C-b>
  global:
    <leader>r: restart-task
    <F5>: restart-all
```

:::fields kind=config
- key: keymap.leader
  type: string
  default: "`"
  desc: The key `<leader>` stands for.
:::

Default bindings in `tasks`:

:::commands
- cmd: <C-a>
  desc: toggle-focus, between the task list and the terminal (in every group)
- cmd: q, Q
  desc: show-quit
- cmd: j <Down>, k <Up>
  desc: next-task, prev-task
- cmd: h <Left>, l <Right>
  desc: collapse, expand, a group in the task list
- cmd: H, L
  desc: "fold {op: close, target: '**'}, fold {op: open, target: '**'}: every group"
- cmd: <M-1> ... <M-8>
  desc: "select-task {index: 0} ... 7"
- cmd: s, x, X
  desc: start-task, stop-task, kill-task
- cmd: r, R
  desc: restart-task, force-restart-task
- cmd: a, C, d, e
  desc: show-add-task, show-duplicate-task, show-remove-task, show-edit-task
- cmd: <Space>
  desc: show-task-menu, also on a right click
- cmd: K, J
  desc: move-task-up, move-task-down, beside the tasks of its group
- cmd: y
  desc: copy-task-command
- cmd: p
  desc: show-commands-menu
- cmd: P
  desc: show-runner-select
- cmd: z
  desc: zoom
- cmd: v
  desc: copy-mode-enter
- cmd: "?"
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

In `term_copy`: `<Esc>` copy-mode-leave, `v` copy-mode-end, `c`
copy-mode-copy, `h` `j` `k` `l` or the arrows copy-mode-move, and
the same scroll keys. Other actions: focus-tasks, focus-term, restart-all,
force-restart-all, veto-task, close-current-modal, duplicate-task (a
copy without the dialog), show-reload, show-write-back, save-to-config,
show-main-menu (the
menu under the `dekit` badge, also by clicking it), detach, quit (stops
the runner without saving, like |cli/runner/stop|),
`{action: show-runner-select, tab: open}` to open a project from any
directory (`tab` is `projects`, `recent`, or `open`), `{action: switch-runner, kind: project, root: /abs/path}` to
move the attachment to another runner's console, and
`{action: command, command: {...}}` to run any command from
|config/hooks|.

`fold` opens, closes, or toggles groups: `{action: fold, op: close}`
closes the selected group, or the one the selected task is in, and
`{action: fold, op: toggle, target: 'apps/*'}` the groups a path or glob
names, as a target does (|start/targets|). `op` is `open`, `close`, or
`toggle`; toggling several closes them unless they are all closed.
