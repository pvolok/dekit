//! The snapshot a runner writes before it execs its replacement.
//!
//! `v1` grows by addition only. A new field carries
//! `#[serde(default, skip_serializing_if = ...)]` with a default equal to
//! the state an older runner had, so it is written only when a value the
//! old code cannot reproduce is in use; every type refuses unknown
//! fields, so an older binary accepts a newer snapshot exactly when none
//! of that state is in use. A new variant is added freely; an older
//! binary refuses it by name. An integer field widens freely: a value
//! that does not fit an older reader's type is refused like an unknown
//! field, so bits for state it does not know go above its range.
//! Removing, renaming, narrowing, or changing the meaning of a field is
//! a new version: `v2` next to `v1` plus a pure
//! `v1 -> v2` conversion, and `decode` walks the chain to the current
//! version. The header fields checked in `decode` never change meaning.
//!
//! `spec`, `run_spec`, and `config_base` hold task entries as `TaskSpec`
//! keeps them: free mappings a reader parses as dekit.yaml would. So a
//! key the entry parser stops taking, or reads another way, is a new
//! version too, with a conversion for the entries older runners saved;
//! a live upgrade refuses a saved entry it cannot read, by name. Beside
//! each entry the task's resolved fields (`tags`, `restart`, the process
//! `spec`, `stop`, `ready_probe`) are still written, for a reader that
//! has no entry; a writer derives both from the same definition.

use anyhow::{Context, bail};

use crate::kernel::{
  task_key::{TaskKey, TaskSpaceId},
  task_path::TaskPath,
};

pub const FORMAT: &str = "dekit-snapshot";
pub const CURRENT_VERSION: u32 = 1;

pub use v1::*;

pub mod v1 {
  use serde::{Deserialize, Serialize};

  #[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
  #[serde(deny_unknown_fields)]
  pub struct Snapshot {
    pub format: String,
    pub version: u32,
    pub source_version: String,
    /// The runner's pid: the check must be its child, and only it resumes.
    pub pid: u32,
    pub runner: Runner,
    pub started_at: u64,
    pub lock_fd: i32,
    pub live_fd: i32,
    pub listener_fd: i32,
    pub next_task_id: usize,
    /// In list order.
    pub tasks: Vec<Task>,
    pub connections: Vec<Connection>,
    /// The config as the runner last took it in (`crate::spec::Base`);
    /// an older runner kept none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_base: Option<crate::spec::Base>,
  }

  #[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
  #[serde(deny_unknown_fields)]
  pub struct Runner {
    pub kind: String,
    pub root: String,
  }

  #[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
  #[serde(deny_unknown_fields)]
  pub struct Task {
    pub id: usize,
    pub space: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    pub pinned: bool,
    #[serde(default)]
    pub deps: Vec<usize>,
    pub restart: Restart,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub job: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ready_timeout_ms: Option<u64>,
    /// None is the default grace.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_timeout_ms: Option<u64>,
    pub state: TaskState,
    pub vetoed: bool,
    pub killed: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub start_failed: bool,
    /// Started at the last save, and not pinned since: `up` starts it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub saved_pin: bool,
    pub attempts: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_start_secs_ago: Option<u64>,
    /// Remaining stop-grace, backoff, or ready-timeout time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timer_ms: Option<u64>,
    /// The definition as written (`crate::spec::TaskSpec`); an older
    /// runner kept none. A reader with it builds the task from it, and
    /// takes the resolved fields beside it only when it is absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spec: Option<crate::spec::TaskSpec>,
    /// The definition the run under way started from, when the task was
    /// redefined since: the run goes on as it started.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_spec: Option<crate::spec::TaskSpec>,
    /// The config entry the task is written in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<crate::spec::Source>,
    pub kind: TaskKind,
  }

  #[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
  #[serde(deny_unknown_fields)]
  #[serde(rename_all = "snake_case")]
  pub enum Restart {
    Never,
    OnFailure,
    Always,
  }

  #[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
  #[serde(deny_unknown_fields)]
  #[serde(tag = "state", rename_all = "snake_case")]
  pub enum TaskState {
    Idle {},
    Starting {},
    Running {},
    Ready {},
    Stopping {},
    /// The exit it backs off from; an older runner wrote none, which
    /// reads as an exit with no detail.
    Backoff(ExitInfo),
    Done(ExitInfo),
    Exited(ExitInfo),
  }

  #[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Serialize)]
  #[serde(deny_unknown_fields)]
  pub struct ExitInfo {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal: Option<i32>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub ready_timeout: bool,
  }

  #[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
  #[serde(deny_unknown_fields)]
  #[serde(tag = "kind", rename_all = "snake_case")]
  pub enum TaskKind {
    Process(ProcessTask),
    Console {},
  }

  #[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
  #[serde(deny_unknown_fields)]
  pub struct ProcessTask {
    pub spec: ProcessSpec,
    pub stop: StopSignal,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log: Option<LogSpec>,
    /// The `log` ready check.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ready_log: Option<String>,
    /// A polled ready check; never together with `ready_log`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ready_probe: Option<ReadyProbe>,
    /// The socket of an sd_notify check, once a run with the check
    /// started; inherited with the task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sd_notify: Option<SdNotifySocket>,
    pub scrollback_len: usize,
    pub mouse_scroll_speed: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance: Option<Instance>,
    pub screen: Screen,
  }

  #[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
  #[serde(deny_unknown_fields)]
  pub struct ReadyProbe {
    pub check: ReadyCheck,
    pub interval_ms: u64,
  }

  #[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
  #[serde(deny_unknown_fields)]
  #[serde(tag = "check", rename_all = "snake_case")]
  pub enum ReadyCheck {
    Tcp {
      #[serde(default, skip_serializing_if = "Option::is_none")]
      host: Option<String>,
      port: u16,
    },
    Http {
      url: String,
    },
    Cmd {
      argv: Vec<String>,
    },
    File {
      path: String,
    },
  }

  #[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
  #[serde(deny_unknown_fields)]
  pub struct ProcessSpec {
    pub prog: String,
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default)]
    pub env: Vec<(String, Option<String>)>,
  }

  /// `program` is a program run without a shell (`stop: {cmd}`). Older
  /// binaries wrote `cmd`, a line for the system shell (`/bin/sh -c`,
  /// PowerShell on Windows), and `shutdown` and `kill`: SIGTERM and
  /// SIGKILL to the group.
  #[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
  #[serde(deny_unknown_fields)]
  #[serde(tag = "stop", rename_all = "snake_case")]
  pub enum StopSignal {
    Shutdown {},
    Kill {},
    Signal { sig: String, group: bool },
    SendKeys { keys: Vec<crate::term::key::Key> },
    Cmd { cmd: String },
    Program { argv: Vec<String> },
  }

  #[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
  #[serde(deny_unknown_fields)]
  pub struct LogSpec {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dir: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    pub truncate: bool,
  }

  #[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
  #[serde(deny_unknown_fields)]
  pub struct SdNotifySocket {
    pub path: String,
    pub fd: i32,
  }

  #[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
  #[serde(deny_unknown_fields)]
  pub struct Instance {
    pub pid: u32,
    pub master_fd: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit: Option<ExitInfo>,
    pub stdout_eof: bool,
    pub ready_sent: bool,
    /// A stop was sent: no ready check runs.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub stop_sent: bool,
    /// Base64.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub ready_line: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_path: Option<String>,
  }

  #[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
  #[serde(deny_unknown_fields)]
  pub struct Screen {
    pub grid: Grid,
    pub alt_grid: Grid,
    pub attrs: Attrs,
    pub saved_attrs: Attrs,
    /// The modes as bits: keypad 1, cursor keys 2, hidden cursor 4,
    /// alternate screen 8, bracketed paste 16, and from bit 8 the rest
    /// (`term::screen::Modes`), each set when it differs from power-on.
    /// Bits 5..8 are zero: an older binary read a byte, so a value that
    /// fits one is exactly a snapshot it can read.
    pub modes: u32,
    pub mouse_mode: String,
    pub mouse_encoding: String,
    pub g0: String,
    pub g1: String,
    #[serde(default = "ascii", skip_serializing_if = "is_ascii")]
    pub g2: String,
    #[serde(default = "ascii", skip_serializing_if = "is_ascii")]
    pub g3: String,
    /// G1 is in GL. Written as `gl == 1`.
    pub shift_out: bool,
    /// Which of G0..G3 is in GL.
    #[serde(default, skip_serializing_if = "is_default")]
    pub gl: u8,
    pub insert: bool,
    pub kitty_flags: Vec<u8>,
    pub alt_kitty_flags: Vec<u8>,
    pub title: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub title_stack: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// The hyperlink new cells belong to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<LinkSnapshot>,
    /// Cell size in pixels, as a client reported it; left out at the
    /// assumed default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cell_pixels: Option<(u16, u16)>,
    /// OSC 10/11/12: the default foreground, background, and cursor
    /// colors a program set, as RGB.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dynamic_colors: Option<[Option<(u8, u8, u8)>; 3]>,
    /// Images programs transmitted (kitty graphics, sixel).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<ImageSnapshot>,
    /// Base64: an escape sequence the parser had started but not finished.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub pending_input: String,
    /// Cell attributes referenced by index from every row.
    pub attrs_table: Vec<Attrs>,
  }

  fn ascii() -> String {
    "ascii".to_string()
  }

  fn is_ascii(s: &String) -> bool {
    s == "ascii"
  }

  /// For `skip_serializing_if`: a value at its default is left out.
  fn is_default<T: Default + PartialEq>(value: &T) -> bool {
    *value == T::default()
  }

  #[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
  #[serde(deny_unknown_fields)]
  pub struct ImageSnapshot {
    pub id: u32,
    #[serde(default, skip_serializing_if = "is_default")]
    pub number: u32,
    /// 24 RGB, 32 RGBA, 100 PNG.
    pub format: u32,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub zlib: bool,
    pub width: u32,
    pub height: u32,
    /// Base64, as transmitted.
    pub data: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub frames: Vec<ImageFrameSnapshot>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub controls: Vec<String>,
    /// When interleaved, the kind of each command in arrival order.
    /// Absent means all frames, then all controls (the legacy replay).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub order: Vec<ImageCommandKind>,
  }

  #[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
  #[serde(rename_all = "snake_case")]
  pub enum ImageCommandKind {
    Frame,
    Control,
  }

  #[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
  #[serde(deny_unknown_fields)]
  pub struct ImageFrameSnapshot {
    pub control: String,
    pub format: u32,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub zlib: bool,
    pub width: u32,
    pub height: u32,
    pub data: String,
  }

  #[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
  #[serde(deny_unknown_fields)]
  pub struct PlacementSnapshot {
    /// The image's id in `Screen::images`.
    pub image: u32,
    #[serde(default, skip_serializing_if = "is_default")]
    pub id: u32,
    pub row: u64,
    pub col: u16,
    pub cols: u16,
    pub rows: u16,
    pub src: (u32, u32, u32, u32),
    pub offset: (u16, u16),
    #[serde(default, skip_serializing_if = "is_default")]
    pub z: i32,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub virtual_: bool,
    /// Drawn into its cells (sixel): erased by writing over them.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub cells: bool,
  }

  #[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
  #[serde(deny_unknown_fields)]
  pub struct LinkSnapshot {
    pub uri: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
  }

  #[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
  #[serde(deny_unknown_fields)]
  pub struct Grid {
    pub width: u16,
    pub height: u16,
    pub pos: (u16, u16),
    pub saved_pos: (u16, u16),
    pub scroll_top: u16,
    pub scroll_bottom: u16,
    /// The scrollback rows (wrapped at `width`, oldest first) followed by
    /// the screen's rows: a reader takes the last `height` as the screen
    /// and the rest as history.
    pub rows: Vec<Row>,
    pub used_rows: u16,
    pub origin_mode: bool,
    pub saved_origin_mode: bool,
    pub scrollback_len: usize,
    pub scrollback_offset: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor_pos: Option<(u16, u16)>,
    pub cursor_style: u8,
    /// Tab stops, when not the default every 8 columns.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tabs: Option<Vec<u16>>,
    /// Left and right margins, when not the full width.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub margins: Option<(u16, u16)>,
    /// Rows ever scrolled off the top (the absolute row of row 0), kept
    /// while placements are anchored to it.
    #[serde(default, skip_serializing_if = "is_default")]
    pub row_base: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub placements: Vec<PlacementSnapshot>,
    /// `(image id, placement id)` pairs placeholder cells refer to, by
    /// slot.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub placeholder_slots: Vec<(u32, u32)>,
    /// Placeholder cells of the screen rows: `(row, col, slot, image
    /// row, image column)`. Their text is the placeholder character.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub image_cells: Vec<(u16, u16, u32, u16, u16)>,
  }

  /// One allocation per row, not per cell: scrollback is most of a
  /// snapshot.
  #[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
  #[serde(deny_unknown_fields)]
  pub struct Row {
    /// The text of every cell, concatenated.
    pub text: String,
    /// Runs of `(chars, cells)`: that many cells, each holding that many
    /// chars of `text` (0 for an empty cell). Boundaries are stored, not
    /// recomputed, so decoding needs no Unicode tables.
    pub cells: Vec<(u32, u16)>,
    /// Runs of `(attrs_table index, cells)`.
    pub attrs: Vec<(usize, u16)>,
    pub wrapped: bool,
    pub size: u16,
    /// Columns that are padding before a wide character that wrapped,
    /// not text: an older binary read them as blanks.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fillers: Vec<u16>,
  }

  #[derive(
    Clone, Debug, Default, Deserialize, Eq, Hash, PartialEq, Serialize,
  )]
  #[serde(deny_unknown_fields)]
  pub struct Attrs {
    pub fg: Color,
    pub bg: Color,
    /// The style bits: bold 1, italic 2, underline 4, inverse 8, and
    /// above the byte an older binary read, dim 256, blink 512, hidden
    /// 1024, strikethrough 2048, overline 4096, then the underline shape
    /// in bits 13..15: a value that fits a byte is exactly what that
    /// binary can show.
    pub mode: u16,
    #[serde(default, skip_serializing_if = "is_default")]
    pub ul: Color,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<LinkSnapshot>,
  }

  #[derive(
    Clone, Copy, Debug, Default, Deserialize, Eq, Hash, PartialEq, Serialize,
  )]
  #[serde(deny_unknown_fields)]
  #[serde(rename_all = "snake_case")]
  pub enum Color {
    #[default]
    Default,
    Idx(u8),
    Rgb(u8, u8, u8),
  }

  #[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
  #[serde(deny_unknown_fields)]
  pub struct Connection {
    pub fd: i32,
    /// None until the client's hello has arrived.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hello: Option<Hello>,
    /// Base64: bytes read from the socket but not yet consumed. Starts at
    /// a frame boundary.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub buffered_input: String,
    /// Base64: bytes owed to the client, written first by the next image.
    /// May start in the middle of a frame.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub buffered_output: String,
    pub kind: ConnectionKind,
  }

  #[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
  #[serde(deny_unknown_fields)]
  pub struct Hello {
    pub protocol: u32,
    pub version: String,
    pub app: String,
    #[serde(default)]
    pub features: Vec<String>,
  }

  #[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
  #[serde(deny_unknown_fields)]
  #[serde(tag = "kind", rename_all = "snake_case")]
  pub enum ConnectionKind {
    Rpc {
      /// Request id of an `upgrade` awaiting its reply.
      #[serde(default, skip_serializing_if = "Option::is_none")]
      pending_upgrade: Option<u64>,
    },
    Attach {
      task: usize,
      width: u16,
      height: u16,
      until_exit: bool,
      /// The client terminal's capabilities, as sent with `attach`.
      #[serde(default, skip_serializing_if = "Option::is_none")]
      caps: Option<crate::protocol::TermCaps>,
      /// The size of a cell on the client's terminal, as it last said.
      #[serde(default, skip_serializing_if = "Option::is_none")]
      cell_pixels: Option<(u16, u16)>,
    },
  }
}

impl From<crate::kernel::task::ExitInfo> for ExitInfo {
  fn from(info: crate::kernel::task::ExitInfo) -> Self {
    ExitInfo {
      code: info.code,
      signal: info.signal,
      ready_timeout: info.ready_timeout,
    }
  }
}

impl From<ExitInfo> for crate::kernel::task::ExitInfo {
  fn from(info: ExitInfo) -> Self {
    crate::kernel::task::ExitInfo {
      code: info.code,
      signal: info.signal,
      ready_timeout: info.ready_timeout,
    }
  }
}

impl From<crate::kernel::task::RestartMode> for Restart {
  fn from(mode: crate::kernel::task::RestartMode) -> Self {
    use crate::kernel::task::RestartMode;
    match mode {
      RestartMode::Never => Restart::Never,
      RestartMode::OnFailure => Restart::OnFailure,
      RestartMode::Always => Restart::Always,
    }
  }
}

impl From<Restart> for crate::kernel::task::RestartMode {
  fn from(restart: Restart) -> Self {
    use crate::kernel::task::RestartMode;
    match restart {
      Restart::Never => RestartMode::Never,
      Restart::OnFailure => RestartMode::OnFailure,
      Restart::Always => RestartMode::Always,
    }
  }
}

impl Task {
  /// Where the task was, refused when its space or path does not read.
  pub fn key(&self) -> anyhow::Result<Option<TaskKey>> {
    let space = if self.space.is_empty() {
      TaskSpaceId::default_space()
    } else {
      TaskSpaceId::new(self.space.clone())
        .map_err(|err| anyhow::anyhow!("task {}: {err}", self.id))?
    };
    let Some(path) = &self.path else {
      return Ok(None);
    };
    let path = TaskPath::new(path.as_str())
      .map_err(|err| anyhow::anyhow!("task {}: {err}", self.id))?;
    Ok(Some(TaskKey::new(space, path)))
  }
}

impl Snapshot {
  /// The tasks as the kernel saved them, by this binary, for `runner`:
  /// what a save writes. An upgrade adds what it hands over: the
  /// descriptors and the connections.
  pub fn new(
    runner: &crate::runner::RunnerSpec,
    kernel: crate::kernel::kernel_message::KernelSnapshot,
    config_base: crate::spec::Base,
  ) -> Snapshot {
    Snapshot {
      format: FORMAT.to_string(),
      version: CURRENT_VERSION,
      source_version: env!("CARGO_PKG_VERSION").to_string(),
      pid: std::process::id(),
      runner: Runner {
        kind: runner.kind.as_str().to_string(),
        root: runner.root.to_string_lossy().into_owned(),
      },
      started_at: 0,
      lock_fd: -1,
      live_fd: -1,
      listener_fd: -1,
      next_task_id: kernel.next_task_id,
      tasks: kernel.tasks,
      connections: Vec::new(),
      config_base: Some(config_base),
    }
  }
}

#[derive(Debug, serde::Deserialize)]
struct Header {
  format: String,
  version: u32,
  #[serde(default)]
  source_version: String,
}

/// Decodes any supported snapshot version into the current one.
pub fn decode(bytes: &[u8]) -> anyhow::Result<Snapshot> {
  let header: Header =
    serde_json::from_slice(bytes).context("unreadable snapshot header")?;
  if header.format != FORMAT {
    bail!("not a dekit snapshot (format '{}')", header.format);
  }
  match header.version {
    1 => serde_json::from_slice::<v1::Snapshot>(bytes).with_context(|| {
      format!(
        "cannot read the v1 snapshot written by dekit {} with dekit {}",
        header.source_version,
        env!("CARGO_PKG_VERSION")
      )
    }),
    version => bail!(
      "snapshot version {version} is newer than this binary supports ({CURRENT_VERSION})"
    ),
  }
}

#[cfg_attr(windows, allow(dead_code))]
pub fn encode(
  snapshot: &Snapshot,
  out: impl std::io::Write,
) -> anyhow::Result<()> {
  Ok(serde_json::to_writer(out, snapshot)?)
}

/// Every inherited descriptor the snapshot names.
#[cfg_attr(windows, allow(dead_code))]
pub fn fds(snapshot: &Snapshot) -> Vec<i32> {
  let mut fds = vec![snapshot.lock_fd, snapshot.live_fd, snapshot.listener_fd];
  for task in &snapshot.tasks {
    if let TaskKind::Process(process) = &task.kind {
      if let Some(instance) = &process.instance {
        fds.push(instance.master_fd);
      }
      if let Some(sd_notify) = &process.sd_notify {
        fds.push(sd_notify.fd);
      }
    }
  }
  for conn in &snapshot.connections {
    fds.push(conn.fd);
  }
  fds
}

pub fn to_base64(bytes: &[u8]) -> String {
  use base64::Engine as _;
  base64::engine::general_purpose::STANDARD.encode(bytes)
}

pub fn from_base64(text: &str) -> anyhow::Result<Vec<u8>> {
  use base64::Engine as _;
  Ok(base64::engine::general_purpose::STANDARD.decode(text)?)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn golden_v1_decodes() {
    let bytes = include_bytes!("fixtures/v1.json");
    let snapshot = decode(bytes).unwrap();
    assert_eq!(snapshot.version, 1);
    assert_eq!(snapshot.tasks.len(), 2);
    assert_eq!(snapshot.connections.len(), 3);
    // Frozen before the client's hello arrived.
    assert_eq!(snapshot.connections[2].hello, None);
    let mut reencoded = Vec::new();
    encode(&snapshot, &mut reencoded).unwrap();
    assert_eq!(decode(&reencoded).unwrap(), snapshot);
  }

  /// Every object in the fixture, as a path of keys and indexes.
  fn objects(
    value: &serde_json::Value,
    path: Vec<String>,
    out: &mut Vec<Vec<String>>,
  ) {
    match value {
      serde_json::Value::Object(map) => {
        out.push(path.clone());
        for (key, value) in map {
          let mut path = path.clone();
          path.push(key.clone());
          objects(value, path, out);
        }
      }
      serde_json::Value::Array(items) => {
        for (i, item) in items.iter().enumerate() {
          let mut path = path.clone();
          path.push(i.to_string());
          objects(item, path, out);
        }
      }
      _ => (),
    }
  }

  fn at_path<'a>(
    value: &'a mut serde_json::Value,
    path: &[String],
  ) -> &'a mut serde_json::Value {
    let mut cur = value;
    for key in path {
      cur = match cur {
        serde_json::Value::Object(map) => map.get_mut(key).unwrap(),
        serde_json::Value::Array(items) => {
          &mut items[key.parse::<usize>().unwrap()]
        }
        _ => unreachable!(),
      };
    }
    cur
  }

  /// A newer binary's field, unknown here, is refused wherever it is.
  #[test]
  fn unknown_field_is_refused_everywhere() {
    for bytes in [
      &include_bytes!("fixtures/v1.json")[..],
      &include_bytes!("fixtures/v1-orchestration.json")[..],
    ] {
      let fixture: serde_json::Value = serde_json::from_slice(bytes).unwrap();
      let mut paths = Vec::new();
      objects(&fixture, Vec::new(), &mut paths);
      assert!(paths.len() > 20, "{}", paths.len());
      for path in paths {
        let mut planted = fixture.clone();
        at_path(&mut planted, &path)
          .as_object_mut()
          .unwrap()
          .insert("from_the_future".to_string(), serde_json::json!(1));
        let err = decode(&serde_json::to_vec(&planted).unwrap())
          .err()
          .unwrap_or_else(|| panic!("accepted an unknown field at {path:?}"))
          .to_string();
        assert!(err.contains("written by dekit 0.9.6"), "{err}");
      }
    }
  }

  /// The writer emits no key the first v1 fixture lacks: every field
  /// added since is skipped at its default.
  #[test]
  fn golden_v1_reencodes_without_new_keys() {
    let fixture: serde_json::Value =
      serde_json::from_slice(include_bytes!("fixtures/v1.json")).unwrap();
    let snapshot = decode(include_bytes!("fixtures/v1.json")).unwrap();
    let reencoded: serde_json::Value = serde_json::to_value(&snapshot).unwrap();
    let mut paths = Vec::new();
    objects(&reencoded, Vec::new(), &mut paths);
    let mut fixture = fixture;
    for path in paths {
      let expected = at_path(&mut fixture, &path).as_object().unwrap().clone();
      let mut reencoded = reencoded.clone();
      for key in at_path(&mut reencoded, &path).as_object().unwrap().keys() {
        assert!(expected.contains_key(key), "new key {key} at {path:?}");
      }
    }
  }

  /// A task's definitions, where its config entry is, and the config as
  /// the runner last took it in, as this binary writes them: read and
  /// written back unchanged.
  #[test]
  fn golden_v1_definitions_round_trip() {
    let bytes = include_bytes!("fixtures/v1-definitions.json");
    let snapshot = decode(bytes).unwrap();
    let web = snapshot
      .tasks
      .iter()
      .find(|task| task.path.as_deref() == Some("web"))
      .unwrap();
    let argv = |spec: &Option<crate::spec::TaskSpec>| {
      spec.as_ref().and_then(crate::spec::TaskSpec::argv)
    };
    assert_eq!(
      argv(&web.spec),
      Some(vec!["npm".into(), "run".into(), "dev".into()])
    );
    assert_eq!(
      argv(&web.run_spec),
      Some(vec!["npm".into(), "start".into()])
    );
    assert_eq!(web.origin.as_ref().map(|o| o.key.as_str()), Some("web"));
    let base = snapshot.config_base.as_ref().unwrap();
    let keys: Vec<(&str, &str)> = base
      .sources()
      .map(|s| (s.file.as_str(), s.mount.as_str()))
      .collect();
    assert_eq!(keys, [("dekit.yaml", ""), ("web/tasks.yaml", "web")]);
    let fixture: serde_json::Value = serde_json::from_slice(bytes).unwrap();
    assert_eq!(serde_json::to_value(&snapshot).unwrap(), fixture);
  }

  /// The fields added for task orchestration (ORCHESTRATION.md
  /// "Snapshot"), each in use, read and write back unchanged.
  #[test]
  fn golden_v1_orchestration_round_trips() {
    let bytes = include_bytes!("fixtures/v1-orchestration.json");
    let snapshot = decode(bytes).unwrap();
    let [db, migrate, api, web, worker] = &snapshot.tasks[..] else {
      panic!("five tasks");
    };
    assert!(db.start_failed);
    assert_eq!(
      (db.ready_timeout_ms, db.stop_timeout_ms),
      (Some(30_000), Some(2_000))
    );
    assert!(migrate.job);
    assert!(worker.saved_pin && !worker.pinned);
    assert_eq!(
      api.state,
      TaskState::Backoff(ExitInfo {
        code: Some(0),
        signal: None,
        ready_timeout: true,
      })
    );
    assert_eq!(
      web.state,
      TaskState::Exited(ExitInfo {
        code: None,
        signal: Some(15),
        ready_timeout: true,
      })
    );
    let process = |task: &Task| match &task.kind {
      TaskKind::Process(process) => process.clone(),
      TaskKind::Console {} => panic!("a process"),
    };
    let StopSignal::Program { .. } = process(db).stop else {
      panic!("db stops with a program");
    };
    assert!(process(db).instance.is_some_and(|i| i.stop_sent));
    let checks: Vec<ReadyCheck> = [db, api, web, worker]
      .into_iter()
      .map(|task| process(task).ready_probe.unwrap().check)
      .collect();
    let [
      ReadyCheck::Tcp { .. },
      ReadyCheck::Http { .. },
      ReadyCheck::Cmd { .. },
      ReadyCheck::File { .. },
    ] = &checks[..]
    else {
      panic!("one check of each kind: {checks:?}");
    };
    let fixture: serde_json::Value = serde_json::from_slice(bytes).unwrap();
    assert_eq!(serde_json::to_value(&snapshot).unwrap(), fixture);
    // An older runner's backoff: an exit with no detail, written back as
    // it was.
    let older = serde_json::json!({"state": "backoff"});
    let state: TaskState = serde_json::from_value(older.clone()).unwrap();
    assert_eq!(state, TaskState::Backoff(ExitInfo::default()));
    assert_eq!(serde_json::to_value(state).unwrap(), older);
  }

  #[test]
  fn an_sd_notify_socket_is_inherited_with_its_task() {
    let mut snapshot = decode(include_bytes!("fixtures/v1.json")).unwrap();
    assert!(!fds(&snapshot).contains(&42));
    let index = snapshot
      .tasks
      .iter()
      .position(|task| matches!(task.kind, TaskKind::Process(_)))
      .unwrap();
    let TaskKind::Process(process) = &mut snapshot.tasks[index].kind else {
      unreachable!()
    };
    assert_eq!(process.sd_notify, None);
    let sd_notify = SdNotifySocket {
      path: "/run/user/1/dekit/x.sd_notify/3.sock".to_string(),
      fd: 42,
    };
    process.sd_notify = Some(sd_notify.clone());
    assert!(fds(&snapshot).contains(&42));
    let json = serde_json::to_value(&snapshot).unwrap();
    assert_eq!(
      json["tasks"][index]["kind"]["sd_notify"],
      serde_json::json!({"path": sd_notify.path, "fd": 42})
    );
    let back = decode(&serde_json::to_vec(&json).unwrap()).unwrap();
    let TaskKind::Process(process) = &back.tasks[index].kind else {
      unreachable!()
    };
    assert_eq!(process.sd_notify, Some(sd_notify));
  }

  #[test]
  fn stop_forms_round_trip() {
    assert_eq!(
      serde_json::to_value(StopSignal::Cmd {
        cmd: "podman compose down".to_string()
      })
      .unwrap(),
      serde_json::json!({"stop": "cmd", "cmd": "podman compose down"})
    );
    assert_eq!(
      serde_json::to_value(StopSignal::Program {
        argv: vec!["podman".to_string(), "stop".to_string()]
      })
      .unwrap(),
      serde_json::json!({"stop": "program", "argv": ["podman", "stop"]})
    );
    for stop in [
      StopSignal::Shutdown {},
      StopSignal::Kill {},
      StopSignal::Signal {
        sig: "SIGINT".to_string(),
        group: false,
      },
      StopSignal::SendKeys {
        keys: vec![crate::term::key::Key::parse("<C-c>").unwrap()],
      },
      StopSignal::Cmd {
        cmd: "podman compose down".to_string(),
      },
      StopSignal::Program {
        argv: vec!["podman".to_string(), "stop".to_string()],
      },
    ] {
      let mut snapshot = decode(include_bytes!("fixtures/v1.json")).unwrap();
      let TaskKind::Process(process) = &mut snapshot.tasks[1].kind else {
        panic!("the fixture's second task is a process");
      };
      process.stop = stop;
      let mut encoded = Vec::new();
      encode(&snapshot, &mut encoded).unwrap();
      assert_eq!(decode(&encoded).unwrap(), snapshot);
    }
  }

  /// A snapshot from before `ready:` keeps its log check.
  #[test]
  fn ready_log_still_decodes() {
    let mut fixture: serde_json::Value =
      serde_json::from_slice(include_bytes!("fixtures/v1.json")).unwrap();
    fixture["tasks"][1]["kind"]["ready_log"] = serde_json::json!("listening");
    let snapshot = decode(&serde_json::to_vec(&fixture).unwrap()).unwrap();
    let TaskKind::Process(process) = &snapshot.tasks[1].kind else {
      panic!("the fixture's second task is a process");
    };
    assert_eq!(process.ready_log.as_deref(), Some("listening"));
    assert_eq!(process.ready_probe, None);
  }

  /// `atch2`: the carried attach connection keeps what the client said
  /// of its terminal, and says nothing new when it said nothing.
  #[test]
  fn attach_connection_carries_caps_and_cell_size() {
    let kind = super::v1::ConnectionKind::Attach {
      task: 3,
      width: 80,
      height: 24,
      until_exit: false,
      caps: Some(crate::protocol::TermCaps::full()),
      cell_pixels: Some((10, 20)),
    };
    let json = serde_json::to_string(&kind).unwrap();
    assert!(json.contains(r#""cell_pixels":[10,20]"#), "{json}");
    assert!(json.contains(r#""caps":{"colors":"true-color""#), "{json}");
    assert_eq!(
      serde_json::from_str::<super::v1::ConnectionKind>(&json).unwrap(),
      kind
    );
    let plain = super::v1::ConnectionKind::Attach {
      task: 3,
      width: 80,
      height: 24,
      until_exit: false,
      caps: None,
      cell_pixels: None,
    };
    assert_eq!(
      serde_json::to_string(&plain).unwrap(),
      r#"{"kind":"attach","task":3,"width":80,"height":24,"until_exit":false}"#
    );
  }

  #[test]
  fn unknown_version_is_refused() {
    let err = decode(br#"{"format":"dekit-snapshot","version":999}"#)
      .unwrap_err()
      .to_string();
    assert!(err.contains("999"), "{err}");
  }

  #[test]
  fn other_format_is_refused() {
    let err = decode(br#"{"format":"json","version":1}"#)
      .unwrap_err()
      .to_string();
    assert!(err.contains("not a dekit snapshot"), "{err}");
  }
}
