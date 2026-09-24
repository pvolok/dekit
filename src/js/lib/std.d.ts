declare const std: {
  /** Log a message to stderr at info level. */
  log(...args: unknown[]): void;
  /** Log a message to stderr at warn level. */
  warn(...args: unknown[]): void;
  /** Log a message to stderr at error level. */
  error(...args: unknown[]): void;

  /** File system operations (async). */
  readonly fs: {
    /** Read a file's entire contents as a UTF-8 string. */
    read(path: string): Promise<string>;
    /** Write a string to a file, creating or overwriting it. */
    write(path: string, content: string): Promise<void>;
    /** Check whether a path exists. */
    exists(path: string): Promise<boolean>;
    /** Create a directory. Pass `{recursive: true}` to create parent dirs. */
    mkdir(path: string, opts?: { recursive?: boolean }): Promise<void>;
    /** Remove a file or directory. Pass `{recursive: true}` for non-empty dirs. */
    rm(path: string, opts?: { recursive?: boolean }): Promise<void>;
    /** List entries in a directory, returns filenames. */
    readDir(path: string): Promise<string[]>;
    /** Get file/directory metadata. */
    stat(path: string): Promise<{
      size: number;
      mtime: number;
      isDir: boolean;
      isFile: boolean;
      isSymlink: boolean;
    }>;
    /** Rename/move a file or directory. */
    rename(from: string, to: string): Promise<void>;
    /** Copy a file. */
    copy(from: string, to: string): Promise<void>;
  };

  /** Path manipulation (sync, no I/O). */
  readonly path: {
    /** Join path segments into a single path. */
    join(...parts: string[]): string;
    /** Return the parent directory of a path. */
    dirname(path: string): string;
    /** Return the last component of a path. */
    basename(path: string): string;
    /** Return the file extension including the leading dot (e.g. ".ts"). */
    extname(path: string): string;
    /** Resolve path segments against the current working directory. */
    resolve(...parts: string[]): string;
    /** Check if a path is absolute. */
    isAbsolute(path: string): boolean;
  };

  /** Commands for the runner this script belongs to. Targets are written
   *  as on the command line; every call resolves to the number of tasks
   *  it acted on. */
  readonly dekit: {
    /** Pin and start the matching tasks and their dependencies. */
    start(target: string): Promise<number>;
    /** Force-restart the matching tasks, whether or not they are wanted. */
    run(target: string): Promise<number>;
    /** Unpin and stop; a task restarts if a dependent still needs it. */
    stop(target: string): Promise<number>;
    /** Unpin only. */
    down(target: string): Promise<number>;
    /** Stop with an immediate hard kill. */
    kill(target: string): Promise<number>;
    /** Force down and hold down until started again. */
    veto(target: string): Promise<number>;
    /** Restart the matching tasks. */
    restart(target: string): Promise<number>;
    /** Remove the matching tasks, killing running ones. */
    remove(target: string): Promise<number>;
    /** Register a process task at an exact path with the given argv and start it. */
    add(path: string, cmd: string[]): Promise<number>;
  };

  /** Environment variable access. */
  readonly env: {
    /** Get an environment variable's value, or `undefined` if unset. */
    get(key: string): string | undefined;
  };

  /** Process operations. */
  readonly process: {
    /** Execute a command and return its output. */
    exec(
      cmd: string,
      args?: string[],
    ): Promise<{ stdout: string; stderr: string; code: number }>;
    /** Get the current working directory. */
    cwd(): string;
    /** Exit the process with an optional code (default 0). */
    exit(code?: number): never;
    /** Command-line arguments. */
    readonly argv: string[];
    /** Current process ID. */
    readonly pid: number;
  };

  /** Terminal UI API (uses alternate screen + raw input). */
  readonly tui: {
    /** Open terminal UI mode. Safe to call multiple times. */
    open(): void;
    /** Close terminal UI mode and restore terminal state. */
    close(): void;
    /** Get current terminal size. */
    size(): { width: number; height: number };
    /** Wait for the next terminal input event. Optional timeout in ms. */
    input(timeoutMs?: number): Promise<
      | {
          type: "key";
          key: string;
          kind: "press" | "repeat" | "release";
        }
      | { type: "timeout" }
      | { type: "resize"; width: number; height: number }
      | {
          type: "mouse";
          x: number;
          y: number;
          kind:
            | "down-left"
            | "down-right"
            | "down-middle"
            | "up-left"
            | "up-right"
            | "up-middle"
            | "drag-left"
            | "drag-right"
            | "drag-middle"
            | "moved"
            | "scroll-down"
            | "scroll-up"
            | "scroll-left"
            | "scroll-right";
        }
      | { type: "focus"; focused: boolean }
      | { type: "paste"; text: string }
      | null
    >;
    /** Draw one frame. Callback receives a transient frame object. */
    draw(
      cb: (frame: {
        readonly width: number;
        readonly height: number;
        /** Draw text at x/y. */
        text(
          x: number,
          y: number,
          text: string,
          style?: {
            fg?:
              | number
              | {
                  r: number;
                  g: number;
                  b: number;
                }
              | "default"
              | "black"
              | "red"
              | "green"
              | "yellow"
              | "blue"
              | "magenta"
              | "cyan"
              | "white"
              | "brightBlack"
              | "brightRed"
              | "brightGreen"
              | "brightYellow"
              | "brightBlue"
              | "brightMagenta"
              | "brightCyan"
              | "brightWhite";
            bg?:
              | number
              | {
                  r: number;
                  g: number;
                  b: number;
                }
              | "default"
              | "black"
              | "red"
              | "green"
              | "yellow"
              | "blue"
              | "magenta"
              | "cyan"
              | "white"
              | "brightBlack"
              | "brightRed"
              | "brightGreen"
              | "brightYellow"
              | "brightBlue"
              | "brightMagenta"
              | "brightCyan"
              | "brightWhite";
            bold?: boolean;
            italic?: boolean;
            underline?: boolean;
            inverse?: boolean;
          },
        ): void;
        /** Fill whole frame with a character. */
        clear(
          ch?: string,
          style?: {
            fg?: number | { r: number; g: number; b: number } | string;
            bg?: number | { r: number; g: number; b: number } | string;
            bold?: boolean;
            italic?: boolean;
            underline?: boolean;
            inverse?: boolean;
          },
        ): void;
        /** Hide terminal cursor for this frame. */
        hideCursor(): void;
        /** Show cursor at x/y for this frame. */
        setCursor(x: number, y: number): void;
        /** Set cursor style for this frame. */
        setCursorStyle(
          style:
            | "default"
            | "blinkingBlock"
            | "steadyBlock"
            | "blinkingUnderline"
            | "steadyUnderline"
            | "blinkingBar"
            | "steadyBar",
        ): void;
      }) => void,
    ): void;
  };
};
