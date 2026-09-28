"""Takes the demo screenshots; run it through screenshots.sh."""

import fcntl
import json
import os
import pty
import re
import struct
import subprocess
import sys
import termios
import threading
import time

DEKIT = os.environ["DEKIT"]
FREEZE = os.environ["FREEZE"]

COLS, ROWS = 110, 30
CLI_COLS = 40

# What `dekit up` leaves behind, as `dekit ls --json` reports it.
SETTLED = {
    "db": "ready",
    "cache": "ready",
    "migrate": "done",
    "api": "ready",
    "worker": "ready",
    "web": "ready",
    "lint": "done",
    "test": "exited",
    "e2e": "idle",
}

BACKGROUND = "#161b22"
FOREGROUND = "#c9d1d9"
PALETTE = [
    "#484f58", "#ff7b72", "#3fb950", "#d29922",
    "#58a6ff", "#bc8cff", "#39c5cf", "#b1bac4",
    "#7d8590", "#ffa198", "#56d364", "#e3b341",
    "#79c0ff", "#d2a8ff", "#56d4dd", "#ffffff",
]


def fail(message):
    sys.exit(f"screenshots: {message}")


def dekit(*args):
    result = subprocess.run([DEKIT, *args], capture_output=True, text=True)
    if result.returncode != 0:
        fail(f"`dekit {' '.join(args)}` failed:\n{result.stderr.strip()}")
    return result.stdout


def spawn(args):
    pid, fd = pty.fork()
    if pid == 0:
        try:
            fcntl.ioctl(0, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))
            os.environ["TERM"] = "xterm-256color"
            os.execv(DEKIT, [DEKIT, *args])
        finally:
            os._exit(127)
    return pid, fd


def read_all(fd):
    out = []
    while True:
        try:
            data = os.read(fd, 65536)
        except OSError:
            break
        if not data:
            break
        out.append(data)
    return b"".join(out).decode(errors="replace").replace("\r\n", "\n")


def in_terminal(*args):
    pid, fd = spawn(args)
    output = read_all(fd)
    os.close(fd)
    _, status = os.waitpid(pid, 0)
    if os.waitstatus_to_exitcode(status) != 0:
        fail(f"`dekit {' '.join(args)}` failed:\n{output.strip()}")
    return output


def wait_for(what, check, timeout=30):
    end = time.monotonic() + timeout
    while not check():
        if time.monotonic() > end:
            fail(f"timed out waiting for {what}")
        time.sleep(0.1)


def wait_still(what, read, quiet=0.5, timeout=30):
    end = time.monotonic() + timeout
    last, since = read(), time.monotonic()
    while time.monotonic() - since < quiet:
        if time.monotonic() > end:
            fail(f"timed out waiting for {what}")
        time.sleep(0.1)
        value = read()
        if value != last:
            last, since = value, time.monotonic()
    return last


def settle():
    end = time.monotonic() + 30
    while True:
        tasks = json.loads(dekit("ls", "--json"))["tasks"]
        seen = {task["path"]: task["state"] for task in tasks}
        if seen == SETTLED:
            break
        if time.monotonic() > end:
            fail(f"the tasks did not settle\n  expected: {SETTLED}\n  last:     {seen}")
        time.sleep(0.1)
    # The services keep printing for a moment after they report ready.
    wait_still("the task screens to stop changing", lambda: [dekit("screen", name) for name in SETTLED])


def plain_lines(screen):
    return re.sub(r"\x1b\[[0-9;:?]*[A-Za-z]", "", screen).rstrip("\n").split("\n")


def task_rows(screen):
    rows = []
    for line in plain_lines(screen)[1:]:
        cell = line[1:].split("┃")[0].split("│")[0]
        if not cell.strip():
            break
        rows.append((cell[1:].split()[0], cell[0] == "•"))
    return rows


class Console:
    """A `dekit attach` client in a terminal of COLS x ROWS."""

    def __enter__(self):
        self.pid, self.fd = spawn(["attach"])
        self.reader = threading.Thread(target=read_all, args=(self.fd,), daemon=True)
        self.reader.start()

        def attached():
            screen = self.screen()
            lines = plain_lines(screen)
            return len(lines) == ROWS and len(lines[0]) == COLS and len(task_rows(screen)) == len(SETTLED)

        wait_for("the terminal UI to attach", attached)
        return self

    def __exit__(self, *exc):
        try:
            os.write(self.fd, b"q")
        except OSError:
            pass
        end = time.monotonic() + 5
        while not os.waitpid(self.pid, os.WNOHANG)[0]:
            if time.monotonic() > end:
                os.kill(self.pid, 9)
                os.waitpid(self.pid, 0)
                break
            time.sleep(0.05)
        self.reader.join(1)
        os.close(self.fd)

    def screen(self):
        return dekit("screen", "@dekit/console")

    def select(self, name):
        rows = task_rows(self.screen())
        names = [row[0] for row in rows]
        if name not in names:
            fail(f"no task {name} in the terminal UI: {names}")
        moves = names.index(name) - [row[1] for row in rows].index(True)
        os.write(self.fd, (b"j" if moves > 0 else b"k") * abs(moves))
        wait_for(f"{name} to be selected", lambda: (name, True) in task_rows(self.screen()))
        return wait_still("the terminal UI to finish drawing", self.screen)


def color(spec):
    kind, value = spec
    if kind == "rgb":
        return value
    if value < 16:
        return PALETTE[value]
    if value < 232:
        levels = [0, 95, 135, 175, 215, 255]
        value -= 16
        return "#%02x%02x%02x" % (levels[value // 36], levels[value // 6 % 6], levels[value % 6])
    gray = 8 + (value - 232) * 10
    return "#%02x%02x%02x" % (gray, gray, gray)


def apply_sgr(style, params):
    codes = [int(p) if p else 0 for p in params.replace(":", ";").split(";")]
    i = 0
    while i < len(codes):
        code = codes[i]
        if code == 0:
            style.clear()
        elif code in (1, 2, 3, 4, 7):
            style[code] = True
        elif code == 22:
            style.pop(1, None)
            style.pop(2, None)
        elif code in (23, 24, 27):
            style.pop(code - 20, None)
        elif 30 <= code <= 37 or 90 <= code <= 97:
            style["fg"] = ("index", code - 30 if code < 90 else code - 82)
        elif 40 <= code <= 47 or 100 <= code <= 107:
            style["bg"] = ("index", code - 40 if code < 100 else code - 92)
        elif code == 39:
            style.pop("fg", None)
        elif code == 49:
            style.pop("bg", None)
        elif code in (38, 48) and i + 2 < len(codes):
            key = "fg" if code == 38 else "bg"
            if codes[i + 1] == 5:
                style[key] = ("index", codes[i + 2])
                i += 2
            elif codes[i + 1] == 2 and i + 4 < len(codes):
                style[key] = ("rgb", "#%02x%02x%02x" % tuple(codes[i + 2 : i + 5]))
                i += 4
        i += 1


def mix(a, b):
    return "#" + "".join("%02x" % ((int(a[i : i + 2], 16) + int(b[i : i + 2], 16)) // 2) for i in (1, 3, 5))


def resolve(style):
    fg = style.get("fg")
    if style.get(1):
        if fg is None:
            fg = ("index", 15)
        elif fg[0] == "index" and fg[1] < 8:
            fg = ("index", fg[1] + 8)
    fg = color(fg) if fg else FOREGROUND
    bg = color(style["bg"]) if "bg" in style else None
    if style.get(7):
        fg, bg = bg or BACKGROUND, fg
    if style.get(2):
        fg = mix(fg, bg or BACKGROUND)
    return fg, bg, bool(style.get(4))


def rgb(hex):
    return "%d;%d;%d" % tuple(int(hex[i : i + 2], 16) for i in (1, 3, 5))


def for_freeze(text, width=0):
    """Rewrites terminal output so that freeze draws it the way a terminal does.

    freeze takes one SGR sequence per run, ignores bold, inverse, and the
    22/39/49 resets, and has its own colors for the 256 indexed ones. So
    every run gets one sequence with the colors spelled out, inverse is
    swapped by hand, and bold brightens the color as in many terminals."""
    lines = []
    for raw in text.rstrip("\n").split("\n"):
        style, cells = {}, []
        for part in re.split(r"(\x1b\[[0-9;:?]*[A-Za-z])", raw.replace("\r", "")):
            if part.startswith("\x1b["):
                if part.endswith("m"):
                    apply_sgr(style, part[2:-1])
                continue
            for ch in part:
                if ch == "\t":
                    cells += [(" ", resolve({}))] * (8 - len(cells) % 8)
                else:
                    cells.append((ch, resolve(style)))
        while cells and cells[-1][0] == " " and cells[-1][1][1] is None:
            cells.pop()
        cells += [(" ", resolve({}))] * (width - len(cells))
        out, last = [], None
        for ch, run in cells:
            if run != last:
                fg, bg, underline = run
                codes = ["0", "38;2;" + rgb(fg)]
                if bg:
                    codes.append("48;2;" + rgb(bg))
                if underline:
                    codes.append("4")
                out.append("\x1b[" + ";".join(codes) + "m")
                last = run
            out.append(ch)
        lines.append("".join(out) + "\x1b[0m")
    return "\n".join(lines)


def render(name, text, outdir, width=0):
    path = os.path.join(outdir, name + ".png")
    # freeze writes PNG at 4 times these sizes: a 14px font at 2x. It leaves
    # about two font sizes of space under the last line, so the bottom
    # padding is small to even the margins out.
    args = [
        "--config", "base",
        "--background", BACKGROUND,
        "--font.size", "7",
        "--no-font.ligatures",
        "--line-height", "1.2",
        "--padding", "11,12,1,12",
        "--border.radius", "4",
        "--output", path,
    ]
    # With rsvg-convert on PATH freeze uses it for PNG, and rsvg-convert
    # ignores the embedded font; freeze's own renderer keeps it.
    env = dict(os.environ, PATH="")
    ansi = for_freeze(text, width)
    result = subprocess.run([FREEZE, *args], input=ansi.encode(), capture_output=True, env=env)
    if result.returncode != 0:
        fail(f"freeze failed on {path}:\n{result.stderr.decode().strip()}")
    os.chmod(path, 0o644)
    root = os.path.dirname(os.path.abspath(outdir))
    print(f"wrote {os.path.relpath(path, root)}")


def session(*commands):
    return "".join(f"\x1b[90m$\x1b[0m \x1b[1m{command}\x1b[0m\n{output}" for command, output in commands)


def main():
    outdir = sys.argv[1]
    up = in_terminal("up")
    settle()
    ls = in_terminal("ls")
    with Console() as console:
        tui = console.select("api")
    os.makedirs(outdir, exist_ok=True)
    render("dekit-tui", tui, outdir)
    render("dekit-up", session(("dekit up", up), ("dekit ls", ls)), outdir, CLI_COLS)


if __name__ == "__main__":
    try:
        main()
    except KeyboardInterrupt:
        sys.exit(1)
