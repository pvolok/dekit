## Unreleased

First release of dekit, the next version of mprocs under a new name. It
keeps the mprocs terminal UI and adds a runner that keeps tasks running
after you close the terminal, a CLI to control them, dependencies and
ready checks in `dekit.yaml`, and built-in JavaScript. Existing
`mprocs.yaml` files still run with `dekit mprocs`.

## 0.9.6 - 2026-06-06

- Fix TUI freeze for unserializable keys (#226)

## 0.9.5 - 2026-06-01

- Fix unbounded memory growth when process output contains an unterminated
  terminal control sequence (#225)

## 0.9.4 - 2026-05-30

- Fixed panic when terminal reports zero screen size

## 0.9.3 - 2026-05-20

- Add `{cmd: <shell>}` value for `stop` to run a shell command as the stop
  action (useful for tools like `podman compose` that don't respond to
  signals reliably)
- Fix client hang under terminal backpressure by using async stdout writes
- Fix server panic when a client disconnects during render
- Fix arrow key double-fire on Windows Terminal Preview (#215)
- Less noicy logs, mprocs.log is only created on "error" level logs
- Show never-started procs in gray instead of red (#212)

## 0.9.2 - 2026-03-21

- Fixed Cyrillic input

## 0.9.1 - 2026-03-20

- Fixed double key event with kitty protocol

## 0.9.0 - 2026-03-19

- Add `proc_log` and `procs.*.log` configuration in mprocs.yaml and `--log-dir`
  cli arg
- Add `--on-init` cli arg (#205)
- Add quit-on-finish option (#188)
- Add Procfile support (#192)
- Implement OSC 0-2 (window title)
- A lot of terminal/pty related internals were rewritten

## 0.8.3 - 2026-01-21

- Fix CSI-u input parsing on Ghostty

## 0.8.2 - 2025-12-22

- Fix --cmd send-key when sending Space
- Fix unit tests

## 0.8.1 - 2025-12-20

- Fix #183
- Fix #184

## 0.8.0 - 2025-12-18

- Rewrite terminal input handling. This should fix all known bugs related to key
  handling.
- Add restart-all and force-restart-all commands
- Use pwsh.exe for shell commands (instead of cmd.exe)
- Fix missing resize event when toggling zoom mode
- Properly handles lines wrappings during screen resize

## 0.7.3 - 2025-05-21

- Add --just argument to load recipes from justfile
- Add title configuration for the proc list pane
- Add name optional argument to "add-proc"
- Fix nushell (Handle CPR sequence)

## 0.7.2 - 2025-01-20

- Add duplicate selected process command
- Use blue color for "DOWN (0)"
- Auto-restart only when exit code is not zero
- Add key binding for toggling keymap

## 0.7.1 - 2024-06-29

- Not using upx anymore
- Disable xterm modifyOtherKeys in iTerm2

## 0.7.0 - 2024-06-24

- Terminal sequences parser rewritten using termwiz vt parser
- Copying fixes for Wayland (#88)
- Support cursor shapes
- Various fixes for keyboard handling
- Accept configs with `.json` extension
- Introduce commands menu (press `p`)
- Add autorestart proccess config option
- Add scrollback config option

## 0.6.4 - 2023-02-17

- Add command for renaming the currently selected process (default: `e`)

## 0.6.3 - 2022-08-20

- Reimplement copying.

## 0.6.2 - 2022-08-09

- Fix global mprocs.yaml path when XDG_CONFIG_HOME env var is defined

## 0.6.1 - 2022-07-22

- Add copy mode
- Add `procs_list_width` to settings
- Add mouse scroll config
- Add quit confirmation dialog

## 0.6.0 - 2022-07-04

- Add `hide_keymap_window` to settings
- Add `--npm` argument
- Add `add_path` to proc config
- Highlight changed unselected processes
- Keymap help now uses actual keys (respecting config)
- Clears the terminal before the first render.

## 0.5.0 - 2022-06-20

- Add command for scrolling by N lines (`C-e`/`C-y`)
- Add mouse support
- Add autostart field to the process config

## 0.4.1 - 2022-06-17

- Zoom mode
- Support batching commands
- Allow passing `null` to clear key bindings

## 0.4.0 - 2022-06-08

- Add _--names_ cli argument
- Add stop field to the process config
- Add cwd field to the process config
- Add key bindings for selecting procs by index (`M-1` - `M-8`)
- Add keymap settings

## 0.3.0 - 2022-05-30

- Add "Remove process"
- Change default config path to mprocs.yaml
- Parse config file as yaml

## 0.2.3 - 2022-05-28

- Add "Add process" feature
- Use only indexed colors

## 0.2.2 - 2022-05-22

- Add experimental remote control
- Add $select operator in config
- Add restart command
- Add new arrow and page keybindings
- Fix build on rust stable

## 0.2.1 - 2022-05-15

- Fix terminal size on Windows

## 0.2.0 - 2022-05-15

- Scrolling terminal with <C-u>/<C-d>
- Environment variables per process in config
- Set commands via cli args

## 0.1.0 - 2022-04-05

- Full rewrite in Rust. Now compiles well on Windows
