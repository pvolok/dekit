# dekit demo

A small made-up web shop, Larkspur, used to take the terminal UI and CLI
screenshots for the README and dekit.run. Every task is a script on
dekit's built-in JavaScript runtime that prints fixed output: nothing
listens on a port, and nothing needs installing besides dekit.

```sh
cd demo
dekit up            # every task has filled its screen within 2 seconds
dekit attach        # the terminal UI, made for 110x30
dekit runner stop   # stop without saving, so the next run starts fresh
```

After `up`: `db`, `cache`, `api`, `worker`, and `web` are up, `migrate` and
`lint` are done, `test` failed with exit code 1, and `e2e` is idle
(`dekit start e2e` runs it).

The output is the same on every run: fixed times and dates, no randomness.
About 4 seconds after its first screen, each service adds a line every few
seconds so a recording looks alive. For still screenshots, start the
runner with `DEMO_STILL=1 dekit up` and the services stop after their
first screen. Your own `~/.config/dekit/config.yaml` also changes the TUI;
`XDG_CONFIG_HOME=/nonexistent dekit up` leaves it out.

## Screenshots

`screenshots.sh` renders the two pictures of the README into `img/` at the
repo root: `dekit-tui.png`, the TUI with `api` selected, and
`dekit-up.png`, `dekit up` followed by `dekit ls`. Both are PNGs at twice
the terminal's size, the same bytes on every run. It needs Python 3 and
[freeze](https://github.com/charmbracelet/freeze), and runs the `dekit`
named by `DEKIT` (default: `dekit` on PATH):

```sh
cargo build -p dekit --locked
DEKIT=target/debug/dekit demo/screenshots.sh
```

It starts the demo in still mode without your config or saved tasks,
waits for the tasks to settle, and stops the runner with
`dekit runner stop` at the end, also when it fails. It refuses to run
while a runner for `demo/` is already up.
