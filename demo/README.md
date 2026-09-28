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
