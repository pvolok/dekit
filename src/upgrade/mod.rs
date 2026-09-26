//! Live upgrade: freeze, snapshot, exec, resume.

#[cfg(unix)]
pub mod resume;
pub mod snapshot;

use std::sync::Arc;

use crate::{dekit::server::ServerCtx, protocol::RpcError};

/// Replaces the running runner with `binary`. Returns only on failure:
/// on success the process image is gone and the new one answers the
/// request.
#[cfg(not(unix))]
pub async fn upgrade(_ctx: Arc<ServerCtx>, _binary: String) -> RpcError {
  RpcError::new(
    crate::protocol::codes::UNSUPPORTED,
    "live upgrade is not available on this platform",
  )
}

#[cfg(unix)]
pub async fn upgrade(ctx: Arc<ServerCtx>, binary: String) -> RpcError {
  match unix::upgrade(ctx, binary).await {
    Ok(never) => match never {},
    Err(error) => error,
  }
}

#[cfg(unix)]
pub(crate) fn set_cloexec(fds: &[i32], on: bool) -> anyhow::Result<()> {
  for fd in fds {
    let flags = unsafe { libc::fcntl(*fd, libc::F_GETFD) };
    if flags < 0 {
      anyhow::bail!("fd {fd}: {}", std::io::Error::last_os_error());
    }
    let flags = if on {
      flags | libc::FD_CLOEXEC
    } else {
      flags & !libc::FD_CLOEXEC
    };
    if unsafe { libc::fcntl(*fd, libc::F_SETFD, flags) } < 0 {
      anyhow::bail!("fd {fd}: {}", std::io::Error::last_os_error());
    }
  }
  Ok(())
}

#[cfg(unix)]
mod unix {
  use std::{
    convert::Infallible,
    ffi::{CString, OsString},
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
    sync::{Arc, atomic::Ordering},
    time::{Duration, SystemTime},
  };

  use super::{set_cloexec, snapshot as snap};
  use crate::{
    dekit::server::{RunnerHandle, ServerCtx},
    kernel::kernel_message::{KernelCommand, KernelSnapshot},
    process::unix_processes_waiter::UnixProcessesWaiter,
    protocol::{RpcError, codes},
    runner::{atomic_write_with, lockfile, validate_binary},
  };

  const FREEZE_TIMEOUT: Duration = Duration::from_secs(10);
  const CHECK_TIMEOUT: Duration = Duration::from_secs(20);

  fn failed(message: impl std::fmt::Display) -> RpcError {
    RpcError::new(codes::INTERNAL, format!("upgrade failed: {message}"))
  }

  pub async fn upgrade(
    ctx: Arc<ServerCtx>,
    binary: String,
  ) -> Result<Infallible, RpcError> {
    let Some(runner) = ctx.runner.as_ref() else {
      return Err(RpcError::new(
        codes::UNSUPPORTED,
        "this runner cannot be upgraded live",
      ));
    };
    if !Path::new(&binary).is_absolute() {
      return Err(failed(format!("binary path must be absolute: {binary}")));
    }
    let binary = validate_binary(PathBuf::from(binary)).map_err(failed)?;
    if runner.upgrading.swap(true, Ordering::SeqCst) {
      return Err(RpcError::new(
        codes::BUSY,
        "an upgrade or pause is in progress",
      ));
    }
    let result = run(&ctx, runner, &binary).await;
    // Only reached on failure: thaw everything the failed attempt froze.
    runner.upgrading.store(false, Ordering::SeqCst);
    result
  }

  /// Freezes, writes the snapshot, checks it with the target, and execs.
  /// Returns only when that failed, with everything it did undone.
  async fn run(
    ctx: &Arc<ServerCtx>,
    runner: &RunnerHandle,
    binary: &Path,
  ) -> Result<Infallible, RpcError> {
    log::info!("Upgrading into {}", binary.display());

    // The next image takes the locks over as they are.
    match lockfile::holds(&runner.paths, runner.lock_fd, runner.live_fd) {
      Ok(true) => (),
      Ok(false) => {
        return Err(failed(
          "the runtime dir no longer has this runner's lock files; restart the runner",
        ));
      }
      Err(err) => return Err(failed(err)),
    }

    let (kernel, connections) = match freeze(ctx).await {
      Ok(frozen) => frozen,
      Err(err) => {
        thaw(ctx);
        return Err(failed(err));
      }
    };

    // The snapshot, checked by the target before anything is switched.
    let snapshot = snapshot_of(runner, kernel, connections);
    let path = &runner.paths.snapshot;
    let fds = snap::fds(&snapshot);
    let err = switch(binary, runner, &snapshot, path, &fds).await;
    // Each step is a no-op for what `switch` never did.
    let _ = set_cloexec(&fds, true);
    thaw(ctx);
    let _ = std::fs::remove_file(path);
    Err(failed(err))
  }

  fn snapshot_of(
    runner: &RunnerHandle,
    kernel: KernelSnapshot,
    connections: Vec<snap::Connection>,
  ) -> snap::Snapshot {
    snap::Snapshot {
      format: snap::FORMAT.to_string(),
      version: snap::CURRENT_VERSION,
      source_version: env!("CARGO_PKG_VERSION").to_string(),
      pid: std::process::id(),
      runner: snap::Runner {
        kind: runner.spec.kind.as_str().to_string(),
        root: runner.spec.root.to_string_lossy().into_owned(),
      },
      started_at: runner.started_at,
      lock_fd: runner.lock_fd,
      live_fd: runner.live_fd,
      listener_fd: runner.listener_fd,
      next_task_id: kernel.next_task_id,
      tasks: kernel.tasks,
      connections,
    }
  }

  /// Stops everything the snapshot describes from changing.
  async fn freeze(
    ctx: &Arc<ServerCtx>,
  ) -> anyhow::Result<(KernelSnapshot, Vec<snap::Connection>)> {
    // 1. Connections stop reading and report themselves.
    let replies = ctx.connections.freeze_all().await;
    let deadline = tokio::time::Instant::now() + FREEZE_TIMEOUT;
    let mut connections = Vec::with_capacity(replies.len());
    for reply in replies {
      match tokio::time::timeout_at(deadline, reply).await {
        Ok(Ok(Some(conn))) => connections.push(conn),
        Ok(Ok(None)) => anyhow::bail!("a connection cannot be carried across"),
        // A connection that closed meanwhile has nothing to carry.
        Ok(Err(_)) => (),
        Err(_) => anyhow::bail!("a connection did not freeze in time"),
      }
    }

    // 2. Nothing is reaped from here on. `pause` waits out a reap in
    //    progress, and the reaper hands each exit to its task's channel
    //    before letting go, so every exit collected so far is queued
    //    ahead of the Freeze sent below.
    UnixProcessesWaiter::pause();

    // 3. Tasks stop their I/O and the graph is captured.
    let (tx, rx) = tokio::sync::oneshot::channel();
    ctx.pc.send(KernelCommand::Freeze(tx));
    let kernel = match tokio::time::timeout(FREEZE_TIMEOUT, rx).await {
      Ok(Ok(Ok(kernel))) => kernel,
      Ok(Ok(Err(reason))) => anyhow::bail!(reason),
      Ok(Err(_)) => anyhow::bail!("the kernel has stopped"),
      Err(_) => anyhow::bail!("a task did not freeze in time"),
    };
    Ok((kernel, connections))
  }

  /// Writes the snapshot, has the target check it, and execs. Returns
  /// only the failure.
  async fn switch(
    binary: &Path,
    runner: &RunnerHandle,
    snapshot: &snap::Snapshot,
    path: &Path,
    fds: &[i32],
  ) -> anyhow::Error {
    let attempt = async {
      atomic_write_with(path, |out| snap::encode(snapshot, out))
        .map_err(|err| anyhow::anyhow!("cannot write the snapshot: {err}"))?;
      // The check runs the target with the arguments the exec will pass.
      let mut args: Vec<OsString> = vec![
        "runner".into(),
        "resume".into(),
        "--snapshot".into(),
        path.into(),
      ];
      if let Some(level) = &runner.log_level {
        args.extend(["--log-level".into(), level.into()]);
      }
      let version = file_version(binary)?;
      check(binary, &args).await?;
      // npm, say, replacing the binary meanwhile would exec code that
      // never read this snapshot.
      if file_version(binary)? != version {
        anyhow::bail!("the target binary changed during the check");
      }
      // Only the fds the snapshot names are inherited.
      set_cloexec(fds, false)?;
      log::info!("Exec {} with snapshot {}", binary.display(), path.display());
      anyhow::bail!("exec failed: {}", exec(binary, &args))
    };
    let failed: anyhow::Result<Infallible> = attempt.await;
    match failed {
      Ok(never) => match never {},
      Err(err) => err,
    }
  }

  fn thaw(ctx: &Arc<ServerCtx>) {
    ctx.pc.send(KernelCommand::Thaw);
    UnixProcessesWaiter::resume();
    ctx.connections.thaw_all();
  }

  /// Runs the target's `--check` as this process's own child. Only a dekit
  /// that got through every check answers `ok <our pid>`. The one child
  /// not handed to the reaper: reaping is paused while it runs, so tokio
  /// collects it.
  async fn check(binary: &Path, args: &[OsString]) -> anyhow::Result<()> {
    let output = tokio::time::timeout(
      CHECK_TIMEOUT,
      tokio::process::Command::new(binary)
        .args(args)
        .arg("--check")
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true)
        .output(),
    )
    .await;
    let output = match output {
      Ok(Ok(output)) => output,
      Ok(Err(err)) => anyhow::bail!("cannot run the target binary: {err}"),
      Err(_) => anyhow::bail!("the target binary did not finish its check"),
    };
    if !output.status.success() {
      let stderr = String::from_utf8_lossy(&output.stderr);
      anyhow::bail!(
        "the target binary rejected the snapshot: {}",
        stderr.trim()
      );
    }
    let answer = String::from_utf8_lossy(&output.stdout);
    if answer.trim() != format!("ok {}", std::process::id()) {
      anyhow::bail!(
        "the target binary did not confirm the check; is it dekit?"
      );
    }
    Ok(())
  }

  /// Changes whenever the file is replaced or written.
  fn file_version(path: &Path) -> std::io::Result<(u64, u64, u64, SystemTime)> {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::metadata(path)?;
    Ok((meta.dev(), meta.ino(), meta.len(), meta.modified()?))
  }

  /// Returns only if the exec failed.
  fn exec(binary: &Path, args: &[OsString]) -> std::io::Error {
    let argv = std::iter::once(binary.as_os_str())
      .chain(args.iter().map(OsString::as_os_str))
      .map(|arg| CString::new(arg.as_bytes()))
      .collect::<Result<Vec<_>, _>>();
    let argv = match argv {
      Ok(argv) => argv,
      Err(err) => return std::io::Error::other(err),
    };
    let mut ptrs: Vec<*const libc::c_char> =
      argv.iter().map(|arg| arg.as_ptr()).collect();
    ptrs.push(std::ptr::null());
    unsafe { libc::execv(argv[0].as_ptr(), ptrs.as_ptr()) };
    std::io::Error::last_os_error()
  }
}
