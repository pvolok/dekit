# mprocs

> **mprocs has been renamed to dekit.** This file is the legacy mprocs README,
> kept for reference. The legacy CLI is still available as `dekit mprocs …`.

_mprocs_ runs multiple commands in parallel and shows output of each command
separately.

When you work on a project you very often need the same list of commands to be
running. For example: `webpack serve`, `jest --watch`, `node src/server.js`.
With mprocs you can list these command in `mprocs.yaml` and run all of them by
running `mprocs`. Then you can switch between outputs of running commands and
interact with them.

It is similar to
[concurrently](https://github.com/open-cli-tools/concurrently) but _mprocs_
shows output of each command separately and allows to interact with processes
(you can even work in _vim_ inside _mprocs_).

<!--ts-->

- [Screenshots](#screenshots)
- [Installation](#installation)
  - [Download binary (Linux, Macos, Windows)](#download-binary-linux-macos-windows)
  - [npm (Linux, Macos, Windows)](#npm-linux-macos-windows)
  - [homebrew (Macos)](#homebrew-macos)
  - [cargo (All platforms)](#cargo-all-platforms)
  - [scoop (Windows)](#scoop-windows)
  - [AUR (Arch Linux)](#aur-arch-linux)
- [Usage](#usage)
  - [Config](#config)
    - [Keymap](#keymap)
    - [$select operator](#select-operator)
    - [Running scripts from package.json](#running-scripts-from-packagejson)
  - [Default keymap](#default-keymap)
  - [Remote control](#remote-control)
- [FAQ](#faq)
  - [mprocs vs tmux/screen](#mprocs-vs-tmuxscreen)

<!-- Created by https://github.com/ekalinin/github-markdown-toc -->
<!-- Added by: pvolok, at: Sun Jul  3 22:53:57 +07 2022 -->

<!--te-->

## Screenshots

<img src="img/mprocs1.png" width="900" height="645" />
<img src="img/mprocs2.png" width="900" height="645" />

## Installation

[![Packaging status](https://repology.org/badge/vertical-allrepos/mprocs.svg)](https://repology.org/project/mprocs/versions)

### Download binary (Linux, Macos, Windows)

[Download](https://github.com/pvolok/mprocs/releases) executable for your
platform and put it into a directory included in PATH.

### npm (Linux, Macos, Windows)

```sh
npm install -g mprocs
```

```sh
yarn global add mprocs
```

### homebrew (Macos, Linux)

```sh
brew install mprocs
```

### cargo (All platforms)

```sh
cargo install mprocs
```

### scoop (Windows)

```sh
scoop install mprocs
```

### AUR (Arch Linux)

```sh
yay mprocs
```

```sh
yay mprocs-bin
```

## Usage

1. Run `mprocs cmd1 cmd2 …` (example: `mprocs "yarn test -w" "webpack serve"`)

OR

1. Create `mprocs.yaml` file
2. Run `mprocs` command

Example `mprocs.yaml`:

```yaml
procs:
  nvim:
    cmd: ["nvim"]
  server:
    shell: "nodemon server.js"
  webpack: "webpack serve"
  tests:
    shell: "jest -w"
    env:
      NODE_ENV: test
```

### Config

[JSON/YAML Configuration Schema](https://raw.githubusercontent.com/pvolok/mprocs/master/schemas/mprocs.json)

There are two kinds of configs: global and local. _Global_ config is loaded
from `~/.config/mprocs/mprocs.yaml` (or
`~\AppData\Roaming\mprocs\mprocs.yaml` on Windows). _Local_ config
is loaded from `mprocs.yaml` from current directory (or set via cli argument:
`mprocs --config ./cfg/mprocs.yaml`). Settings in the _local_ config override
settings in the _global_ config.

- **procs**: _object_ - Processes to run. Only allowed in local config.
  - **shell**: _string_ - Shell command to run (exactly one of **shell** or
    **cmd** must be provided).
  - **cmd**: _array<string>_ - Array of command and args to run (exactly one of
    **shell** or **cmd** must be provided).
  - **cwd**: _string_ - Set working directory for the process. Prefix
    `<CONFIG_DIR>` will be replaced with the path of the directory where the
    config is located.
  - **env**: _object<string, string|null>_ - Set env variables. Object keys are
    variable names. Assign variable to null, to clear variables inherited from
    parent process.
  - **add_path**: _string|array<string>_ - Add entries to the _PATH_
    environment variable.
  - **autostart**: _bool_ - Start process when mprocs starts. Default: _true_.
  - **autorestart**: _bool_ - Restart process when it exits. Default: false. Note: If process exits within 1 second of starting, it will not be restarted.
  - **stop**: _"SIGINT"|"SIGTERM"|"SIGKILL"|{send-keys: array<key>}|{cmd: string}|"hard-kill"_ -
    A way to stop a process (using `x` key or when quitting mprocs). The `cmd`
    form runs a shell command (e.g. `podman compose down`) instead of
    signaling the proc. The proc is expected to exit on its own once the
    command takes effect.
  - **log**: _bool|string|object|null_ - Process logging config. A string is a
    shorthand for `{ dir: ... }`. `true` enables logging with defaults.
    - **dir**: _string|null_ - Directory for per-process log files.
    - **file**: _string|null_ - Log file path relative to **dir**.
      Supports `{name}`, `{id}`, `{pid}`, and `{ts}` placeholders.
      Default: _"{name}.log"_.
    - **enabled**: _bool_ - Enable or disable logging. Default: _true_.
    - **mode**: _"append"|"truncate"_ - Whether append to or replace an existing
      log file. Default: _"append"_.
    Prefix `<CONFIG_DIR>` will be replaced with the path of the directory where
    the config is located. Per-process **log** values merge with the top-level
    **proc_log** config.
- **hide_keymap_window**: _bool_ - Hide the pane at the bottom of the screen
  showing key bindings.
- **hide_zen_message**: _bool_ - Hide the zoom mode hint while zoomed.
  Default: _false_.
- **hide_proc_frame**: _bool_ - Hide process pane titles, borders, and padding.
  Default: _false_.
- **mouse_scroll_speed**: _integer_ - Number of lines to scroll per one mouse
  scroll.
- **scrollback**: _integer_ - Scrollback size. Default: _1000_.
- **proc_list_width**: _integer_ - Process list window width.
- **proc_log**: _bool|string|object|null_ - Default logging config for
  processes. Accepts the same shape as per-process **log**.
- **keymap_procs**: _object_ - Key bindings for process list. See
  [Keymap](#keymap).
- **keymap_term**: _object_ - Key bindings for terminal window. See
  [Keymap](#keymap).
- **keymap_copy**: _object_ - Key bindings for copy mode. See
  [Keymap](#keymap).

#### Keymap

Default key bindings can be overridden in config using _keymap_procs_,
_keymap_term_, or _keymap_copy_ fields. Available commands are documented in
the [Remote control](#remote-control) section.

There are three keymap levels:

- Default keymaps
- `~/.config/mprocs/mprocs.yaml` (or `~\AppData\Roaming\mprocs\mprocs.yaml` on Windows)
- `./mprocs.yaml` (can be overridden by the _-c/--config_ cli arg)

Lower levels override bindings from previous levels. Key bindings from previous
levels can be cleared by specifying `reset: true` field at the same level as
keys.

Key bindings are defined between `<` and `>`, e.g., `<Enter>` (enter key), `<Down>` (down arrow), `<Up>` (up arrow), `<C-q>` (CTRL + q).

```yaml
keymap_procs: # keymap when process list is focused
  <C-q>: { c: toggle-focus }
  <C-a>: null # unbind key
keymap_term: # keymap when terminal is focused
  reset: true
  <C-q>: { c: toggle-focus }
  <C-j>:
    c: batch
    cmds:
      - { c: focus-procs }
      - { c: next-proc }
```

#### `$select` operator

You can define different values depending on the current operating system. Any
value in config can be wrapped with a _$select_ operator. To provide different
values based on current OS define an object with:

- First field `$select: os`
- Fields defining values for different OSes: `macos: value`. Possible
  values are listed here:
  https://doc.rust-lang.org/std/env/consts/constant.OS.html.
- Field `$else: default value` will be matched if no value was defined for
  current OS. If current OS is not matched and field `$else` is missing, then
  mprocs will fail to load config.

Example `mprocs.yaml`:

```yaml
procs:
  my process:
    shell:
      $select: os
      windows: "echo %TEXT%"
      $else: "echo $TEXT"
    env:
      TEXT:
        $select: os
        windows: Windows
        linux: Linux
        macos: Macos
        freebsd: FreeBSD
```

#### Running scripts from package.json

If you run _mprocs_ with an `--npm` argument, it will load scripts from
`package.json`. But the scripts are not run by default, and you can launch
desired scripts manually.

```sh
# Run mprocs with scripts from package.json
mprocs --npm
```

#### Running processes from Procfile

You can specify a `Procfile` to load procs from:

```sh
mprocs --procfile ./Procfile.dev

# default: Profile
mprocs --procfile

# autostart all processes
mprocs --procfile --on-init '{c: restart-all}'
```

### Default keymap

Process list focused:

- `q` - Quit (soft kill processes and wait then to exit)
- `Q` - Force quit (terminate processes)
- `p` - All commands
- `C-a` - Focus output pane
- `x` - Soft kill selected process (send SIGTERM signal, hard kill on Windows)
- `X` - Hard kill selected process (send SIGKILL)
- `s` - Start selected process, if it is not running
- `r` - Soft kill selected process and restart it when it stops
- `R` - Hard kill selected process and restart it when it stops
- `a` - Add new process
- `C` - Duplicate selected process
- `d` - Remove selected process (process must be stopped first)
- `e` - Rename selected process
- `k` or `↑` - Select previous process
- `j` or `↓` - Select next process
- `M-1` - `M-8` - Select process 1-8
- `C-d` or `page down` - Scroll output down
- `C-u` or `page up` - Scroll output up
- `C-e` - Scroll output down by 3 lines
- `C-y` - Scroll output up by 3 lines
- `z` - Zoom into terminal window
- `v` - Enter copy mode

Process output focused:

- `C-a` - Focus processes pane

Copy mode:

- `v` - Start selecting end point
- `c` - Copy selected text
- `Esc` - Leave copy mode
- `C-a` - Focus processes pane
- `C-d` or `page down` - Scroll output down
- `C-u` or `page up` - Scroll output up
- `C-e` - Scroll output down by 3 lines
- `C-y` - Scroll output up by 3 lines
- `k` or `↑` - Move cursor up
- `l` or `→` - Move cursor right
- `j` or `↓` - Move cursor down
- `h` or `←` - Move cursor left

#### How to copy text

1. Press `v` to enter copy mode.
2. Move cursor to the beginning of the text you want to copy.
3. Press `v` to start selection.
4. Move cursor to the end of the text.
5. Press `c` to copy selected text to the clipboard.

### Remote control

Optionally, _mprocs_ can listen on TCP port for remote commands.
You have to define remote control server address in `mprocs.yaml`
(`server: 127.0.0.1:4050`) or via cli argument (`mprocs --server 127.0.0.1:4050`). To send a command to running _mprocs_ instance
use the **ctl** argument: `mprocs --ctl '{c: quit}'` or `mprocs --ctl '{c: send-key, key: <C-c>}'`.

Alternatively, you can use the **exec** argument to send an initial command to
the current foreground process on startup: `mprocs --on-init '{c:
restart-all}'`.

Commands are encoded as yaml. Available commands:

- `{c: quit-or-ask}` - Stop processes and quit. If any processes are running,
  show a confirmation dialog.
- `{c: quit}` - Stop processes and quit. Does not show confirm dialog.
- `{c: force-quit}`
- `{c: toggle-focus}` - Toggle focus between process list and terminal.
- `{c: focus-procs}` - Focus process list
- `{c: focus-term}` - Focus process terminal window
- `{c: zoom}` - Zoom into terminal window
- `{c: next-proc}`
- `{c: prev-proc}`
- `{c: select-proc, index: <PROCESS INDEX>}` - Select process by index, top process has index 0
- `{c: start-proc}`
- `{c: term-proc}`
- `{c: kill-proc}`
- `{c: restart-proc}` - Restart selected process
- `{c: restart-all}` - Restart all processes
- `{c: force-restart-proc}` - Restart selected process by sending SIGKILL
- `{c: force-restart-all}`
- `{c: show-commands-menu}` - All commands
- `{c: show-add-proc}`
- `{c: add-proc, cmd: "<SHELL COMMAND>", name: "<PROC NAME>"}` - Add proccess. `name` field is optional.
- `{c: duplicate-proc}`
- `{c: show-remove-proc}`
- `{c: remove-proc, id: "<PROCESS ID>"}`
- `{c: show-rename-proc}`
- `{c: rename-proc, name: "<NEW_NAME>"}` - Rename currently selected process
- `{c: scroll-down}`
- `{c: scroll-up}`
- `{c: scroll-down-lines, n: <COUNT>}`
- `{c: scroll-up-lines, n: <COUNT>}`
- `{c: copy-mode-enter}` - Enter copy mode
- `{c: copy-mode-leave}` - Leave copy mode
- `{c: copy-mode-move, dir: <DIRECTION> }` - Move starting or ending position
  of the selection. Available directions: `up/right/down/left`.
- `{c: copy-mode-end}` - Start selecting end point of the selection.
- `{c: copy-mode-copy}` - Copy selected text to the clipboard and leave copy
  mode.
- `{c: send-key, key: "<KEY>"}` - Send key to current process. Key examples:
  `<C-a>`, `<Enter>`
- `{c: batch, cmds: [{c: focus-procs}, …]}` - Send multiple commands

## FAQ

### mprocs vs tmux/screen

_mprocs_ is meant to make it easier to run specific commands that you end up
running repeatedly, such as compilers and test runners. This is in contrast
with _tmux_, which is usually used to run much more long-lived processes -
usually a shell - in each window/pane. Another difference is that _tmux_ runs a
server and a client, which allows the client to detach and reattach later,
keeping the processes running. _mprocs_ is meant more for finite lifetime
processes that you keep re-running, but when _mprocs_ ends, so do the processes
it is running within its windows.

### Copying doesn't work in tmux

Tmux doesn't have escape sequences for copying enabled by default. To enable it
add the following to `~/.tmux.conf`:

```
set -g set-clipboard on
```
