//! The snapshot a runner writes before it execs its replacement.
//!
//! `v1` is frozen: a schema change adds `v2` next to it plus a pure
//! `v1 -> v2` conversion, and `decode` walks the chain to the current
//! version. The header fields checked in `decode` never change meaning.

use anyhow::{Context, bail};

pub const FORMAT: &str = "dekit-snapshot";
pub const CURRENT_VERSION: u32 = 1;

pub use v1::*;

pub mod v1 {
  use serde::{Deserialize, Serialize};

  #[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
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
    pub tasks: Vec<Task>,
    pub connections: Vec<Connection>,
  }

  #[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
  pub struct Runner {
    pub kind: String,
    pub root: String,
  }

  #[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
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
    pub state: TaskState,
    pub vetoed: bool,
    pub killed: bool,
    pub attempts: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_start_secs_ago: Option<u64>,
    /// Remaining stop-grace or backoff time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timer_ms: Option<u64>,
    pub kind: TaskKind,
  }

  #[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
  #[serde(rename_all = "snake_case")]
  pub enum Restart {
    Never,
    OnFailure,
    Always,
  }

  #[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
  #[serde(tag = "state", rename_all = "snake_case")]
  pub enum TaskState {
    Idle,
    Starting,
    Running,
    Ready,
    Stopping,
    Backoff,
    Done(ExitInfo),
    Exited(ExitInfo),
  }

  #[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Serialize)]
  pub struct ExitInfo {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal: Option<i32>,
  }

  #[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
  #[serde(tag = "kind", rename_all = "snake_case")]
  pub enum TaskKind {
    Process(ProcessTask),
    Console,
  }

  #[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
  pub struct ProcessTask {
    pub spec: ProcessSpec,
    pub stop: StopSignal,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log: Option<LogSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ready_log: Option<String>,
    pub scrollback_len: usize,
    pub mouse_scroll_speed: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance: Option<Instance>,
    pub screen: Screen,
  }

  #[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
  pub struct ProcessSpec {
    pub prog: String,
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default)]
    pub env: Vec<(String, Option<String>)>,
  }

  #[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
  #[serde(tag = "stop", rename_all = "snake_case")]
  pub enum StopSignal {
    Shutdown,
    Kill,
    Signal { sig: String, group: bool },
    SendKeys { keys: Vec<crate::term::key::Key> },
    Cmd { cmd: String },
  }

  #[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
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
  pub struct Instance {
    pub pid: u32,
    pub master_fd: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit: Option<ExitInfo>,
    pub stdout_eof: bool,
    pub ready_sent: bool,
    /// Base64.
    #[serde(default)]
    pub ready_line: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_path: Option<String>,
  }

  #[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
  pub struct Screen {
    pub grid: Grid,
    pub alt_grid: Grid,
    pub attrs: Attrs,
    pub saved_attrs: Attrs,
    pub modes: u8,
    pub mouse_mode: String,
    pub mouse_encoding: String,
    pub g0: String,
    pub g1: String,
    pub shift_out: bool,
    pub insert: bool,
    pub kitty_flags: Vec<u8>,
    pub alt_kitty_flags: Vec<u8>,
    pub title: String,
    /// Base64: an escape sequence the parser had started but not finished.
    #[serde(default)]
    pub pending_input: String,
    /// Cell attributes referenced by index from every row.
    pub attrs_table: Vec<Attrs>,
  }

  #[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
  pub struct Grid {
    pub width: u16,
    pub height: u16,
    pub pos: (u16, u16),
    pub saved_pos: (u16, u16),
    pub scroll_top: u16,
    pub scroll_bottom: u16,
    pub rows: Vec<Row>,
    pub used_rows: u16,
    pub origin_mode: bool,
    pub saved_origin_mode: bool,
    pub scrollback_len: usize,
    pub scrollback_offset: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor_pos: Option<(u16, u16)>,
    pub cursor_style: u8,
  }

  /// One allocation per row, not per cell: scrollback is most of a
  /// snapshot.
  #[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
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
  }

  #[derive(
    Clone, Copy, Debug, Default, Deserialize, Eq, Hash, PartialEq, Serialize,
  )]
  pub struct Attrs {
    pub fg: Color,
    pub bg: Color,
    pub mode: u8,
  }

  #[derive(
    Clone, Copy, Debug, Default, Deserialize, Eq, Hash, PartialEq, Serialize,
  )]
  #[serde(rename_all = "snake_case")]
  pub enum Color {
    #[default]
    Default,
    Idx(u8),
    Rgb(u8, u8, u8),
  }

  #[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
  pub struct Connection {
    pub fd: i32,
    /// None until the client's hello has arrived.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hello: Option<Hello>,
    /// Base64: bytes read from the socket but not yet consumed. Starts at
    /// a frame boundary.
    #[serde(default)]
    pub buffered_input: String,
    /// Base64: bytes owed to the client, written first by the next image.
    /// May start in the middle of a frame.
    #[serde(default)]
    pub buffered_output: String,
    pub kind: ConnectionKind,
  }

  #[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
  pub struct Hello {
    pub protocol: u32,
    pub version: String,
    pub app: String,
    #[serde(default)]
    pub features: Vec<String>,
  }

  #[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
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
    },
  }
}

impl From<crate::kernel::task::ExitInfo> for ExitInfo {
  fn from(info: crate::kernel::task::ExitInfo) -> Self {
    ExitInfo {
      code: info.code,
      signal: info.signal,
    }
  }
}

impl From<ExitInfo> for crate::kernel::task::ExitInfo {
  fn from(info: ExitInfo) -> Self {
    crate::kernel::task::ExitInfo {
      code: info.code,
      signal: info.signal,
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

#[derive(Debug, serde::Deserialize)]
struct Header {
  format: String,
  version: u32,
}

/// Decodes any supported snapshot version into the current one.
pub fn decode(bytes: &[u8]) -> anyhow::Result<Snapshot> {
  let header: Header =
    serde_json::from_slice(bytes).context("unreadable snapshot header")?;
  if header.format != FORMAT {
    bail!("not a dekit snapshot (format '{}')", header.format);
  }
  match header.version {
    1 => serde_json::from_slice::<v1::Snapshot>(bytes)
      .context("invalid v1 snapshot"),
    version => bail!(
      "snapshot version {version} is newer than this binary supports ({CURRENT_VERSION})"
    ),
  }
}

pub fn encode(
  snapshot: &Snapshot,
  out: impl std::io::Write,
) -> anyhow::Result<()> {
  Ok(serde_json::to_writer(out, snapshot)?)
}

/// Every inherited descriptor the snapshot names.
pub fn fds(snapshot: &Snapshot) -> Vec<i32> {
  let mut fds = vec![snapshot.lock_fd, snapshot.live_fd, snapshot.listener_fd];
  for task in &snapshot.tasks {
    if let TaskKind::Process(process) = &task.kind
      && let Some(instance) = &process.instance
    {
      fds.push(instance.master_fd);
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
