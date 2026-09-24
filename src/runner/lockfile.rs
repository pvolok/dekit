//! Runner runtime registry.
//!
//! Two lock files per runner identity, because liveness probing must
//! never contend with a starting runner:
//!
//! - `.lock` serializes runners. Only a runner ever locks it, so a
//!   denied non-blocking acquire means another runner really holds it.
//! - `.live` is the liveness lock. The live runner holds it for its
//!   lifetime; probes (`is_runner_alive`) and cleanup take it only for
//!   an instant, so the runner acquires it with a blocking lock after
//!   winning `.lock` — every other holder releases it promptly.
//!
//! Cleanup deletes files only while holding `.live`; a runner re-checks
//! both files by inode after locking and starts over if cleanup swept
//! them from under it.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::runner::{RunnerKind, RunnerSpec, atomic_write};

#[derive(Debug, Serialize, Deserialize)]
pub struct RunnerRecord {
  pub schema: u32,
  pub protocol: u32,
  pub kind: RunnerKind,
  #[serde(flatten)]
  pub owner: OwnerInfo,
  pub socket: String,
  pub root: String,
  pub started_at: u64,
  pub version: String,
  pub binary: String,
  /// Non-fatal config problems, surfaced to clients (status, start).
  #[serde(default)]
  pub warnings: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct RunnerPaths {
  pub lock: PathBuf,
  pub live: PathBuf,
  pub record: PathBuf,
  pub socket: PathBuf,
  pub error: PathBuf,
  /// The upgrade snapshot handed from one runner image to the next.
  pub snapshot: PathBuf,
}

impl RunnerPaths {
  fn from_stem(runtime_dir: &Path, stem: &str) -> Self {
    #[cfg(unix)]
    let socket = runtime_dir.join(format!("{stem}.sock"));
    #[cfg(windows)]
    let socket = PathBuf::from(format!(r"\\.\pipe\dekit-{stem}"));
    Self {
      lock: runtime_dir.join(format!("{stem}.lock")),
      live: runtime_dir.join(format!("{stem}.live")),
      record: runtime_dir.join(format!("{stem}.json")),
      error: runtime_dir.join(format!("{stem}.error")),
      snapshot: runtime_dir.join(format!("{stem}.snapshot")),
      socket,
    }
  }
}

pub struct LockFileGuard {
  paths: RunnerPaths,
  #[cfg_attr(windows, allow(dead_code))]
  lock: std::fs::File,
  #[cfg_attr(windows, allow(dead_code))]
  live: std::fs::File,
  /// Published in the record; survives an upgrade with the process.
  started_at: u64,
}

pub(crate) fn now_secs() -> u64 {
  std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .unwrap_or_default()
    .as_secs()
}

impl LockFileGuard {
  pub fn socket_path(&self) -> &Path {
    &self.paths.socket
  }

  pub fn paths(&self) -> &RunnerPaths {
    &self.paths
  }

  pub fn started_at(&self) -> u64 {
    self.started_at
  }

  /// The lock descriptors, for inheritance across an upgrade: flock
  /// locks belong to the open file description, so they travel with it.
  #[cfg(unix)]
  pub fn fds(&self) -> (i32, i32) {
    use std::os::fd::AsRawFd;
    (self.lock.as_raw_fd(), self.live.as_raw_fd())
  }

  /// Locks inherited across an exec; nothing is re-acquired. The
  /// previous image checked `holds` before the switch.
  #[cfg(unix)]
  pub fn adopt(
    paths: RunnerPaths,
    lock_fd: i32,
    live_fd: i32,
    started_at: u64,
  ) -> Self {
    use std::os::fd::FromRawFd;
    LockFileGuard {
      paths,
      lock: unsafe { std::fs::File::from_raw_fd(lock_fd) },
      live: unsafe { std::fs::File::from_raw_fd(live_fd) },
      started_at,
    }
  }

  pub fn publish(
    &self,
    runner: &RunnerSpec,
    warnings: &[String],
  ) -> anyhow::Result<()> {
    // Canonical, so comparisons against the (canonical) selected kernel
    // don't report a restart over a symlinked install path.
    let binary = dunce::canonicalize(std::env::current_exe()?)?;
    let binary = binary
      .to_str()
      .ok_or_else(|| anyhow::anyhow!("dekit binary path is not valid UTF-8"))?;
    let contents = RunnerRecord {
      schema: 1,
      protocol: crate::protocol::ctl::PROTOCOL_VERSION,
      kind: runner.kind.clone(),
      owner: owner_info()?,
      socket: self.paths.socket.to_string_lossy().into_owned(),
      root: runner
        .root
        .to_str()
        .expect("validated runner root")
        .to_string(),
      started_at: self.started_at,
      version: env!("CARGO_PKG_VERSION").to_string(),
      binary: binary.to_string(),
      warnings: warnings.to_vec(),
    };
    let bytes = serde_json::to_vec_pretty(&contents)?;
    atomic_write(&self.paths.record, &bytes)?;
    let _ = std::fs::remove_file(&self.paths.error);
    Ok(())
  }

  pub fn publish_error(&self, error: &anyhow::Error) {
    let message = format!("{error:#}\n");
    let _ = atomic_write(&self.paths.error, message.as_bytes());
  }
}

impl Drop for LockFileGuard {
  fn drop(&mut self) {
    let _ = std::fs::remove_file(&self.paths.socket);
    let _ = std::fs::remove_file(&self.paths.record);
    let _ = std::fs::remove_file(&self.paths.live);
    let _ = std::fs::remove_file(&self.paths.lock);
  }
}

#[cfg(unix)]
fn current_sid() -> u32 {
  unsafe { libc::getsid(0) as u32 }
}

#[cfg(windows)]
fn current_sid() -> u32 {
  0
}

/// Identity written into the lock file the instant a runner wins it —
/// before any bootstrap work — so a runner wedged during startup (still
/// `Starting`, no discovery record yet) can still be force-killed by
/// verified identity.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct OwnerInfo {
  pub pid: u32,
  /// OS-assigned process start time; with `pid` it names one process
  /// instance, so a kill fallback never signals a recycled pid.
  pub start_time: u64,
  /// Unix session id. Equal to `pid` when the runner owns its session
  /// (the spawned daemon does), which makes a session-wide reap safe.
  pub sid: u32,
}

fn owner_info() -> anyhow::Result<OwnerInfo> {
  let pid = std::process::id();
  Ok(OwnerInfo {
    pid,
    start_time: crate::runner::kill::process_start_time(pid)?,
    sid: current_sid(),
  })
}

/// The owner identity a `Starting` runner wrote into its lock file.
pub fn runner_owner(runner: &RunnerSpec) -> Option<OwnerInfo> {
  let paths = runner_paths(runner).ok()?;
  let data = std::fs::read_to_string(&paths.lock).ok()?;
  serde_json::from_str(&data).ok()
}

pub struct RunnerInfo {
  pub contents: RunnerRecord,
  pub is_running: bool,
}

pub enum RunnerState {
  Absent,
  Starting,
  Ready(RunnerRecord),
  Stale(RunnerRecord),
  Failed(String),
}

fn runner_hash(runner: &RunnerSpec) -> String {
  let identity = format!(
    "{}\0{}",
    runner.kind.as_str(),
    runner.root.to_str().expect("validated runner root")
  );
  let digest = Sha256::digest(identity.as_bytes());
  URL_SAFE_NO_PAD.encode(&digest[..12])
}

/// Where `pause` leaves the runner's tasks for its next start: under the
/// user data dir, which outlives the runtime dir and a reboot.
pub fn paused_path(runner: &RunnerSpec) -> anyhow::Result<PathBuf> {
  Ok(
    crate::runner::user_data_dir()?
      .join("paused")
      .join(format!("{}.json", runner_hash(runner))),
  )
}

pub fn get_runtime_dir() -> anyhow::Result<PathBuf> {
  #[cfg(unix)]
  {
    if let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR") {
      return Ok(PathBuf::from(dir).join("dekit"));
    }
    let uid = rustix::process::getuid().as_raw();
    Ok(std::env::temp_dir().join(format!("dekit-{uid}")))
  }
  #[cfg(windows)]
  {
    let local_app_data = std::env::var_os("LOCALAPPDATA")
      .ok_or_else(|| anyhow::anyhow!("LOCALAPPDATA not set"))?;
    Ok(PathBuf::from(local_app_data).join("dekit").join("run"))
  }
}

pub fn runner_paths(runner: &RunnerSpec) -> anyhow::Result<RunnerPaths> {
  Ok(RunnerPaths::from_stem(
    &get_runtime_dir()?,
    &runner_hash(runner),
  ))
}

fn prepare_runtime_dir(path: &Path) -> anyhow::Result<()> {
  std::fs::create_dir_all(path)?;
  #[cfg(unix)]
  {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
  }
  Ok(())
}

fn open_lock_file(path: &Path) -> std::io::Result<std::fs::File> {
  std::fs::OpenOptions::new()
    .read(true)
    .write(true)
    .create(true)
    .truncate(false)
    .open(path)
}

pub fn create_lock_file(runner: &RunnerSpec) -> anyhow::Result<LockFileGuard> {
  let paths = runner_paths(runner)?;
  lock_runner_paths(paths)
}

fn lock_runner_paths(paths: RunnerPaths) -> anyhow::Result<LockFileGuard> {
  let runtime_dir = paths.lock.parent().expect("lock path has a parent");
  prepare_runtime_dir(runtime_dir)?;

  for _ in 0..8 {
    let lock = open_lock_file(&paths.lock)?;
    acquire_flock(&lock)?;
    let live = open_lock_file(&paths.live)?;
    acquire_flock_blocking(&live)?;
    if !path_matches_file(&paths.lock, &lock)?
      || !path_matches_file(&paths.live, &live)?
    {
      continue;
    }
    let _ = std::fs::remove_file(&paths.record);
    let _ = std::fs::remove_file(&paths.socket);
    let _ = std::fs::remove_file(&paths.error);
    // Best-effort: the owner identity only aids force-killing a wedged
    // Starting runner, so a failure to compute or write it must not keep
    // the runner from starting. Written in place (never via atomic
    // rename, which would swap the inode this flock guards).
    if let Ok(owner) = owner_info() {
      let _ = write_owner(&lock, &owner);
    }
    return Ok(LockFileGuard {
      paths,
      lock,
      live,
      started_at: now_secs(),
    });
  }
  anyhow::bail!("runner lock changed repeatedly during startup")
}

fn write_owner(file: &std::fs::File, owner: &OwnerInfo) -> anyhow::Result<()> {
  use std::io::{Seek, SeekFrom, Write};
  let bytes = serde_json::to_vec(owner)?;
  let mut file = file;
  file.set_len(0)?;
  file.seek(SeekFrom::Start(0))?;
  file.write_all(&bytes)?;
  file.sync_data()?;
  Ok(())
}

pub fn read_runner_record(record_path: &Path) -> Option<RunnerRecord> {
  let data = std::fs::read_to_string(record_path).ok()?;
  serde_json::from_str(&data).ok()
}

pub fn is_runner_alive(live_path: &Path) -> bool {
  let file = match std::fs::OpenOptions::new().read(true).open(live_path) {
    Ok(file) => file,
    Err(_) => return false,
  };
  !try_acquire_flock(&file)
}

fn validate_record(
  runner: &RunnerSpec,
  record: RunnerRecord,
) -> anyhow::Result<RunnerRecord> {
  if record.kind != runner.kind || Path::new(&record.root) != runner.root {
    anyhow::bail!("runtime record identity does not match its filename")
  }
  Ok(record)
}

pub fn runner_state(
  runner: &RunnerSpec,
  paths: &RunnerPaths,
) -> anyhow::Result<RunnerState> {
  let record = read_runner_record(&paths.record)
    .map(|record| validate_record(runner, record))
    .transpose()?;
  let alive = is_runner_alive(&paths.live);
  match (record, alive) {
    (Some(record), true) => Ok(RunnerState::Ready(record)),
    (Some(record), false) => Ok(RunnerState::Stale(record)),
    (None, true) => Ok(RunnerState::Starting),
    // No record and the live lock is free. A failing runner writes an
    // error file before exiting, so that is the only sound "it failed"
    // signal — file existence alone is not, because a runner in the
    // middle of creating its own lock files (before it flocks them)
    // would otherwise read as a crash. Without an error file this is
    // Absent: nothing running, or a runner not up yet, and a waiting
    // client keeps polling until its own deadline.
    (None, false) => match std::fs::read_to_string(&paths.error) {
      Ok(error) => Ok(RunnerState::Failed(error.trim().to_string())),
      Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
        Ok(RunnerState::Absent)
      }
      Err(err) => Err(err.into()),
    },
  }
}

pub fn get_runner_state(runner: &RunnerSpec) -> anyhow::Result<RunnerState> {
  let paths = runner_paths(runner)?;
  runner_state(runner, &paths)
}

pub fn list_runners() -> anyhow::Result<Vec<RunnerInfo>> {
  let runtime_dir = get_runtime_dir()?;
  let entries = match std::fs::read_dir(&runtime_dir) {
    Ok(entries) => entries,
    Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
      return Ok(Vec::new());
    }
    Err(err) => return Err(err.into()),
  };
  let mut runners = Vec::new();
  for entry in entries {
    let path = entry?.path();
    if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
      continue;
    }
    if let Some(contents) = read_runner_record(&path) {
      runners.push(RunnerInfo {
        is_running: is_runner_alive(&path.with_extension("live")),
        contents,
      });
    }
  }
  Ok(runners)
}

pub fn cleanup_paths(paths: &RunnerPaths) -> anyhow::Result<bool> {
  let existed = paths.lock.exists()
    || paths.live.exists()
    || paths.record.exists()
    || paths.error.exists()
    || paths.socket.exists();
  let runtime_dir = paths.lock.parent().expect("lock path has a parent");
  prepare_runtime_dir(runtime_dir)?;
  for _ in 0..8 {
    let live = open_lock_file(&paths.live)?;
    if !try_acquire_flock(&live) {
      return Ok(false);
    }
    if !path_matches_file(&paths.live, &live)? {
      continue;
    }
    let _ = std::fs::remove_file(&paths.socket);
    let _ = std::fs::remove_file(&paths.record);
    let _ = std::fs::remove_file(&paths.error);
    let _ = std::fs::remove_file(&paths.lock);
    let _ = std::fs::remove_file(&paths.live);
    return Ok(existed);
  }
  anyhow::bail!("runner lock changed repeatedly during cleanup")
}

pub fn cleanup_all_stale() -> anyhow::Result<u32> {
  let runtime_dir = get_runtime_dir()?;
  let entries = match std::fs::read_dir(&runtime_dir) {
    Ok(entries) => entries,
    Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(0),
    Err(err) => return Err(err.into()),
  };
  let mut stems = HashSet::new();
  for entry in entries {
    let path = entry?.path();
    if let Some("lock" | "live" | "json" | "sock" | "error") =
      path.extension().and_then(|ext| ext.to_str())
      && let Some(stem) = path.file_stem().and_then(|stem| stem.to_str())
    {
      stems.insert(stem.to_string());
    }
  }
  let mut count = 0;
  for stem in stems {
    if cleanup_paths(&RunnerPaths::from_stem(&runtime_dir, &stem))? {
      count += 1;
    }
  }
  Ok(count)
}

/// Whether the runtime dir still names the lock files a runner holds by
/// these descriptors (see `LockFileGuard::fds`); a sweep of the dir while
/// the runner ran leaves it holding unlinked ones.
#[cfg(unix)]
pub fn holds(
  paths: &RunnerPaths,
  lock_fd: i32,
  live_fd: i32,
) -> anyhow::Result<bool> {
  use std::{mem::ManuallyDrop, os::fd::FromRawFd};
  // Borrowed: closing them would release the runner's locks.
  let lock = ManuallyDrop::new(unsafe { std::fs::File::from_raw_fd(lock_fd) });
  let live = ManuallyDrop::new(unsafe { std::fs::File::from_raw_fd(live_fd) });
  Ok(
    path_matches_file(&paths.lock, &lock)?
      && path_matches_file(&paths.live, &live)?,
  )
}

#[cfg(unix)]
fn path_matches_file(
  path: &Path,
  file: &std::fs::File,
) -> anyhow::Result<bool> {
  use std::os::unix::fs::MetadataExt;
  let path = match std::fs::metadata(path) {
    Ok(metadata) => metadata,
    Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(false),
    Err(err) => return Err(err.into()),
  };
  let file = file.metadata()?;
  Ok(path.dev() == file.dev() && path.ino() == file.ino())
}

#[cfg(windows)]
fn path_matches_file(
  path: &Path,
  file: &std::fs::File,
) -> anyhow::Result<bool> {
  let path_file = match std::fs::OpenOptions::new().read(true).open(path) {
    Ok(file) => file,
    Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(false),
    Err(err) => return Err(err.into()),
  };
  Ok(windows_file_id(&path_file)? == windows_file_id(file)?)
}

#[cfg(windows)]
fn windows_file_id(file: &std::fs::File) -> anyhow::Result<(u32, u64)> {
  use std::os::windows::io::AsRawHandle;
  use windows::Win32::Foundation::HANDLE;
  use windows::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
  };
  let mut info = BY_HANDLE_FILE_INFORMATION::default();
  unsafe {
    GetFileInformationByHandle(HANDLE(file.as_raw_handle() as _), &mut info)?;
  }
  let index = ((info.nFileIndexHigh as u64) << 32) | info.nFileIndexLow as u64;
  Ok((info.dwVolumeSerialNumber, index))
}

#[cfg(unix)]
fn acquire_flock(file: &std::fs::File) -> anyhow::Result<()> {
  use std::os::fd::AsFd;
  rustix::fs::flock(
    file.as_fd(),
    rustix::fs::FlockOperation::NonBlockingLockExclusive,
  )
  .map_err(|err| {
    if err == rustix::io::Errno::WOULDBLOCK {
      anyhow::anyhow!("Another runner is already running for this identity")
    } else {
      anyhow::anyhow!("Failed to acquire lock: {err}")
    }
  })
}

#[cfg(unix)]
fn acquire_flock_blocking(file: &std::fs::File) -> anyhow::Result<()> {
  use std::os::fd::AsFd;
  rustix::fs::flock(file.as_fd(), rustix::fs::FlockOperation::LockExclusive)
    .map_err(|err| anyhow::anyhow!("Failed to acquire lock: {err}"))
}

#[cfg(unix)]
fn try_acquire_flock(file: &std::fs::File) -> bool {
  use std::os::fd::AsFd;
  rustix::fs::flock(
    file.as_fd(),
    rustix::fs::FlockOperation::NonBlockingLockExclusive,
  )
  .is_ok()
}

#[cfg(windows)]
fn acquire_flock(file: &std::fs::File) -> anyhow::Result<()> {
  if try_acquire_flock(file) {
    Ok(())
  } else {
    anyhow::bail!("Another runner is already running for this identity")
  }
}

#[cfg(windows)]
fn acquire_flock_blocking(file: &std::fs::File) -> anyhow::Result<()> {
  use std::os::windows::io::AsRawHandle;
  use windows::Win32::Foundation::HANDLE;
  use windows::Win32::Storage::FileSystem::{
    LOCKFILE_EXCLUSIVE_LOCK, LockFileEx,
  };
  let handle = HANDLE(file.as_raw_handle() as _);
  let mut overlapped = unsafe { std::mem::zeroed() };
  unsafe {
    LockFileEx(
      handle,
      LOCKFILE_EXCLUSIVE_LOCK,
      Some(0),
      1,
      0,
      &mut overlapped,
    )
  }
  .map_err(|err| anyhow::anyhow!("Failed to acquire lock: {err}"))
}

#[cfg(windows)]
fn try_acquire_flock(file: &std::fs::File) -> bool {
  use std::os::windows::io::AsRawHandle;
  use windows::Win32::Foundation::HANDLE;
  use windows::Win32::Storage::FileSystem::{
    LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY, LockFileEx,
  };
  let handle = HANDLE(file.as_raw_handle() as _);
  let mut overlapped = unsafe { std::mem::zeroed() };
  unsafe {
    LockFileEx(
      handle,
      LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
      Some(0),
      1,
      0,
      &mut overlapped,
    )
  }
  .is_ok()
}

#[cfg(test)]
mod tests {
  use super::*;

  fn temp_paths(name: &str) -> (PathBuf, RunnerPaths) {
    let runtime = std::env::temp_dir()
      .join(format!("dekit-lock-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&runtime);
    std::fs::create_dir_all(&runtime).unwrap();
    let paths = RunnerPaths::from_stem(&runtime, "t");
    (runtime, paths)
  }

  #[test]
  fn cleanup_removes_a_record_without_a_lock() {
    let (runtime, paths) = temp_paths("cleanup");
    std::fs::write(&paths.record, "{}").unwrap();

    assert!(cleanup_paths(&paths).unwrap());
    assert!(!paths.record.exists());
    assert!(!paths.lock.exists());
    let _ = std::fs::remove_dir_all(runtime);
  }

  #[test]
  fn probes_and_cleanup_leave_a_held_guard_alone() {
    let (runtime, paths) = temp_paths("held");
    let guard = lock_runner_paths(paths.clone()).unwrap();

    // A probe momentarily locks `.live`; the guard must stay intact and
    // still read as alive afterwards.
    assert!(is_runner_alive(&paths.live));
    assert!(is_runner_alive(&paths.live));
    assert!(!cleanup_paths(&paths).unwrap());
    assert!(paths.lock.exists());
    assert!(paths.live.exists());

    drop(guard);
    assert!(!is_runner_alive(&paths.live));
    let _ = std::fs::remove_dir_all(runtime);
  }
}
