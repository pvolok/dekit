use std::time::Duration;

use anyhow::Context;

use crate::protocol::{ConnReceiver, ConnSender};
use crate::runner::{RunnerSpec, resolve_kernel_binary};
use crate::runner::{
  lockfile::{self, RunnerState, cleanup_paths, runner_paths},
  spawn::spawn_runner,
};

pub async fn connect_client_socket(
  runner: &RunnerSpec,
  start_runner: bool,
) -> anyhow::Result<(ConnSender, ConnReceiver)> {
  let paths = runner_paths(runner)?;
  let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
  let mut spawn_attempted = false;
  let mut connect_error = None;
  loop {
    match lockfile::runner_state(runner, &paths)? {
      RunnerState::Ready(record) => {
        match connect_socket(&record.socket).await {
          Ok(conn) => return Ok(conn),
          Err(error) => connect_error = Some(error),
        }
      }
      RunnerState::Starting => {}
      RunnerState::Absent | RunnerState::Stale(_) | RunnerState::Failed(_)
        if start_runner && !spawn_attempted =>
      {
        cleanup_paths(&paths)?;
        match lockfile::runner_state(runner, &paths)? {
          RunnerState::Ready(_) | RunnerState::Starting => continue,
          RunnerState::Absent
          | RunnerState::Stale(_)
          | RunnerState::Failed(_) => {}
        }
        let executable = resolve_kernel_binary(runner)?;
        spawn_runner(runner, &executable)?;
        spawn_attempted = true;
      }
      RunnerState::Failed(error) => {
        anyhow::bail!("runner failed to start: {error}");
      }
      RunnerState::Absent | RunnerState::Stale(_) if !start_runner => {
        anyhow::bail!("Runner is not running. Start it with `dekit up`.");
      }
      // A record without a live lock after our spawn: the runner came up
      // and then died without reporting an error.
      RunnerState::Stale(_) => {
        cleanup_paths(&paths)?;
        anyhow::bail!("runner exited unexpectedly after starting");
      }
      // The spawned runner has not created its lock yet.
      RunnerState::Absent => {}
    }
    if tokio::time::Instant::now() >= deadline {
      if let Some(error) = connect_error {
        return Err(error)
          .context("runner stayed registered but could not be reached");
      }
      if spawn_attempted {
        anyhow::bail!("spawned runner did not become ready within 15s");
      }
      anyhow::bail!("Timed out waiting for runner to become ready.");
    }
    tokio::time::sleep(Duration::from_millis(20)).await;
  }
}

#[cfg(unix)]
pub use self::unix::{ServerSocket, bind_server_socket, connect_socket};
#[cfg(windows)]
pub use self::windows::{ServerSocket, bind_server_socket, connect_socket};

/// An accepted client connection and, where the transport is a file
/// descriptor, its number (for inheritance across an upgrade).
pub struct Accepted {
  pub sender: ConnSender,
  pub receiver: ConnReceiver,
  pub fd: Option<i32>,
}

#[cfg(unix)]
pub use self::unix::adopt_connection;

#[cfg(unix)]
mod unix {
  use std::os::fd::{AsRawFd, FromRawFd};
  use std::path::Path;

  use tokio::net::{UnixListener, UnixStream};

  use super::Accepted;
  use crate::protocol::{ConnReceiver, ConnSender};

  pub async fn bind_server_socket(
    socket_path: &Path,
  ) -> anyhow::Result<ServerSocket> {
    let bind = || UnixListener::bind(socket_path);
    let listener = match bind() {
      Ok(listener) => listener,
      Err(err) => match err.kind() {
        std::io::ErrorKind::AddrInUse => {
          std::fs::remove_file(socket_path)?;
          bind()?
        }
        _ => return Err(err.into()),
      },
    };

    // Only the owner may talk to the runner.
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(
      socket_path,
      std::fs::Permissions::from_mode(0o600),
    )?;

    Ok(ServerSocket { listener })
  }

  pub struct ServerSocket {
    listener: UnixListener,
  }

  impl ServerSocket {
    pub async fn accept(&mut self) -> anyhow::Result<Accepted> {
      let (stream, _addr) = self.listener.accept().await?;
      let fd = stream.as_raw_fd();
      let (read, write) = stream.into_split();
      Ok(Accepted {
        sender: ConnSender::new(write),
        receiver: ConnReceiver::new(read),
        fd: Some(fd),
      })
    }

    pub fn raw_fd(&self) -> Option<i32> {
      Some(self.listener.as_raw_fd())
    }

    /// A listener inherited across an exec.
    pub fn adopt(fd: i32) -> anyhow::Result<Self> {
      let std_listener =
        unsafe { std::os::unix::net::UnixListener::from_raw_fd(fd) };
      std_listener.set_nonblocking(true)?;
      Ok(ServerSocket {
        listener: UnixListener::from_std(std_listener)?,
      })
    }
  }

  /// A client connection inherited across an exec, with the bytes the
  /// previous image had read but not consumed, and those it still owed.
  pub fn adopt_connection(
    fd: i32,
    input: &[u8],
    output: &[u8],
  ) -> anyhow::Result<(ConnSender, ConnReceiver)> {
    let std_stream = unsafe { std::os::unix::net::UnixStream::from_raw_fd(fd) };
    std_stream.set_nonblocking(true)?;
    let stream = UnixStream::from_std(std_stream)?;
    let (read, write) = stream.into_split();
    Ok((
      ConnSender::with_pending(write, output),
      ConnReceiver::with_buffered(read, input),
    ))
  }

  pub async fn connect_socket(
    socket: &str,
  ) -> anyhow::Result<(ConnSender, ConnReceiver)> {
    let stream = UnixStream::connect(socket)
      .await
      .map_err(|e| anyhow::anyhow!("Failed to connect to runner: {}", e))?;
    let (read, write) = stream.into_split();
    Ok((ConnSender::new(write), ConnReceiver::new(read)))
  }
}

#[cfg(windows)]
mod windows {
  use std::{path::Path, time::Duration};

  use tokio::net::windows::named_pipe::{
    ClientOptions, NamedPipeServer, ServerOptions,
  };

  use super::Accepted;
  use crate::protocol::{ConnReceiver, ConnSender};

  // ERROR_PIPE_BUSY: all pipe instances are taken; retry shortly.
  const PIPE_BUSY: i32 = 231;

  pub async fn bind_server_socket(
    socket_path: &Path,
  ) -> anyhow::Result<ServerSocket> {
    let pipe_name = socket_path.to_string_lossy().into_owned();
    let next = ServerOptions::new()
      .first_pipe_instance(true)
      .create(&pipe_name)?;
    Ok(ServerSocket { pipe_name, next })
  }

  pub struct ServerSocket {
    pipe_name: String,
    next: NamedPipeServer,
  }

  impl ServerSocket {
    pub async fn accept(&mut self) -> anyhow::Result<Accepted> {
      self.next.connect().await?;
      let connected = std::mem::replace(
        &mut self.next,
        ServerOptions::new().create(&self.pipe_name)?,
      );
      let (read, write) = tokio::io::split(connected);
      Ok(Accepted {
        sender: ConnSender::new(write),
        receiver: ConnReceiver::new(read),
        fd: None,
      })
    }

    pub fn raw_fd(&self) -> Option<i32> {
      None
    }
  }

  pub async fn connect_socket(
    socket: &str,
  ) -> anyhow::Result<(ConnSender, ConnReceiver)> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    let pipe = loop {
      match ClientOptions::new().open(socket) {
        Ok(pipe) => break pipe,
        Err(err) if err.raw_os_error() == Some(PIPE_BUSY) => {
          if tokio::time::Instant::now() >= deadline {
            anyhow::bail!("Failed to connect to runner: pipe is busy");
          }
          tokio::time::sleep(Duration::from_millis(20)).await;
        }
        Err(err) => {
          anyhow::bail!("Failed to connect to runner: {}", err);
        }
      }
    };
    let (read, write) = tokio::io::split(pipe);
    Ok((ConnSender::new(write), ConnReceiver::new(read)))
  }
}
