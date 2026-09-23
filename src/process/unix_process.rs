use std::{
  ffi::CString,
  os::fd::{FromRawFd, OwnedFd},
  ptr::{null, null_mut},
};

use std::os::fd::{AsRawFd, RawFd};

use rustix::termios::Pid;
use tokio::io::unix::AsyncFd;

use crate::{
  kernel::task::{ExitInfo, TaskId},
  process::{process::Process, unix_processes_waiter::UnixProcessesWaiter},
  term::Winsize,
};

use super::process_spec::ProcessSpec;

pub struct UnixProcess {
  pub pid: Pid,
  master: AsyncFd<OwnedFd>,
}

impl UnixProcess {
  pub fn spawn(
    _id: TaskId,
    spec: &ProcessSpec,
    size: Winsize,
    on_wait_returned: Box<dyn Fn(ExitInfo) + Send + Sync>,
  ) -> std::io::Result<Self> {
    let prog = CString::new(spec.prog.as_str()).unwrap_or_default();

    let mut argv: Vec<CString> = Vec::new();
    argv.push(prog.clone());
    for arg in &spec.args {
      argv.push(CString::new(arg.as_str()).unwrap_or_default());
    }
    let argv_ptrs = {
      let mut v: Vec<*const libc::c_char> =
        argv.iter().map(|a| a.as_ptr()).collect();
      v.push(null());
      v
    };

    let cwd_c = spec
      .get_cwd()
      .as_ref()
      .map(|cwd| CString::new(cwd.as_str()).unwrap_or_default());

    let env_c: Vec<(CString, Option<CString>)> = spec
      .env
      .iter()
      .filter_map(|(k, v)| {
        let k_c = CString::new(k.as_str()).ok()?;
        let v_c = v.as_ref().and_then(|v| CString::new(v.as_str()).ok());
        Some((k_c, v_c))
      })
      .collect();

    unsafe {
      let mut block_set: libc::sigset_t = std::mem::zeroed();
      let mut old_set: libc::sigset_t = std::mem::zeroed();
      libc::sigfillset(&mut block_set);
      libc::pthread_sigmask(libc::SIG_SETMASK, &block_set, &mut old_set);

      let mut empty_set: libc::sigset_t = std::mem::zeroed();
      libc::sigemptyset(&mut empty_set);

      let mut master_fd = -1;
      // Some args are *mut on some BSD variants.
      #[allow(clippy::unnecessary_mut_passed)]
      let pid =
        libc::forkpty(&mut master_fd, null_mut(), null_mut(), &mut size.into());
      if pid < 0 {
        libc::pthread_sigmask(libc::SIG_SETMASK, &old_set, null_mut());
        return Err(std::io::Error::last_os_error());
      }

      if pid == 0 {
        for signo in &[
          libc::SIGCHLD,
          libc::SIGHUP,
          libc::SIGINT,
          libc::SIGQUIT,
          libc::SIGTERM,
          libc::SIGALRM,
        ] {
          libc::signal(*signo, libc::SIG_DFL);
        }
        libc::pthread_sigmask(libc::SIG_SETMASK, &empty_set, null_mut());

        if let Some(cwd) = &cwd_c {
          if libc::chdir(cwd.as_ptr()) != 0 {
            libc::_exit(1);
          }
        }

        for (key, value) in &env_c {
          match value {
            Some(v) => {
              libc::setenv(key.as_ptr(), v.as_ptr(), 1);
            }
            None => {
              libc::unsetenv(key.as_ptr());
            }
          }
        }

        libc::execvp(prog.as_ptr(), argv_ptrs.as_ptr());
        libc::perror(null());
        libc::_exit(1);
      }

      libc::pthread_sigmask(libc::SIG_SETMASK, &old_set, null_mut());

      let flags = libc::fcntl(master_fd, libc::F_GETFD, 0);
      if flags < 0 {
        return Err(std::io::Error::last_os_error());
      }
      if libc::fcntl(master_fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) < 0 {
        return Err(std::io::Error::last_os_error());
      }

      let flags = libc::fcntl(master_fd, libc::F_GETFL, 0);
      if flags < 0 {
        return Err(std::io::Error::last_os_error());
      }
      if libc::fcntl(master_fd, libc::F_SETFL, flags | libc::O_NONBLOCK) < 0 {
        return Err(std::io::Error::last_os_error());
      }

      let pid = Pid::from_raw_unchecked(pid);
      let master = OwnedFd::from_raw_fd(master_fd);

      UnixProcessesWaiter::wait_for(pid, on_wait_returned);

      Ok(UnixProcess {
        pid,
        master: AsyncFd::new(master)?,
      })
    }
  }
}

impl UnixProcess {
  /// Takes over a child and its PTY master inherited across an exec. The
  /// fd is already non-blocking. Unlike `spawn`, the caller registers
  /// for the exit: the child may already be reaped.
  pub fn adopt(pid: u32, master_fd: RawFd) -> std::io::Result<Self> {
    let pid = Pid::from_raw(pid as i32).ok_or_else(|| {
      std::io::Error::new(std::io::ErrorKind::InvalidInput, "pid 0")
    })?;
    let master = unsafe { OwnedFd::from_raw_fd(master_fd) };
    Ok(UnixProcess {
      pid,
      master: AsyncFd::new(master)?,
    })
  }

  pub fn master_fd(&self) -> RawFd {
    self.master.as_raw_fd()
  }
}

impl Process for UnixProcess {
  fn on_exited(&mut self) {}

  fn pid(&self) -> u32 {
    let raw: i32 = self.pid.as_raw_nonzero().into();
    raw as u32
  }

  async fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
    loop {
      let mut guard = self.master.readable().await?;
      match guard.try_io(|fd| Ok(rustix::io::read(fd, &mut *buf)?)) {
        Ok(result) => {
          break Ok(result?);
        }
        Err(_would_block) => {
          continue;
        }
      }
    }
  }

  async fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
    loop {
      let mut guard = self.master.writable().await?;
      match guard.try_io(|fd| Ok(rustix::io::write(fd, buf)?)) {
        Ok(result) => {
          break Ok(result?);
        }
        Err(_would_block) => {
          continue;
        }
      }
    }
  }

  async fn write_all(&mut self, buf: &[u8]) -> std::io::Result<()> {
    let mut count = 0;
    while count < buf.len() {
      count += self.write(&buf[count..]).await?;
    }
    Ok(())
  }

  fn send_signal(&mut self, sig: i32, group: bool) -> std::io::Result<()> {
    // forkpty puts the child in its own session/process group (pgid == pid).
    // Signaling the whole group reaches children that outlive the shell — e.g.
    // `sh -c "...; tail -f /dev/null"`, which would otherwise keep the pty slave
    // open so the master never EOFs and the task never reports as stopped.
    let pid: i32 = self.pid.as_raw_nonzero().into();
    let target = if group { -pid } else { pid };
    if unsafe { libc::kill(target, sig) } < 0 {
      return Err(std::io::Error::last_os_error());
    }
    Ok(())
  }

  async fn kill(&mut self, group: bool) -> std::io::Result<()> {
    self.send_signal(libc::SIGKILL, group)
  }

  fn resize(&mut self, size: Winsize) -> std::io::Result<()> {
    rustix::termios::tcsetwinsize(&self.master, size.into())?;
    Ok(())
  }
}

#[cfg(test)]
mod tests {
  use std::time::{Duration, Instant};

  use crate::process::process_spec::ProcessSpec;
  use crate::term::Winsize;

  use super::*;

  // The shell forks `sleep` as a child that inherits the pty. A group SIGTERM
  // must reap the child too; otherwise the orphan keeps the slave open and the
  // master never EOFs — the "task won't stop" bug. (Exit is detected here via
  // the pty EOF, not the SIGCHLD waiter, which is only set up in the real app.)
  #[tokio::test]
  async fn group_signal_reaps_lingering_child() {
    let spec = ProcessSpec::from_argv(vec![
      "sh".into(),
      "-c".into(),
      "echo hi; sleep 100; true".into(),
    ]);
    let size = Winsize {
      x: 80,
      y: 24,
      x_px: 0,
      y_px: 0,
    };
    let mut proc =
      UnixProcess::spawn(TaskId(0), &spec, size, Box::new(|_| {})).unwrap();

    // Let the shell print and fork the child before signaling.
    let mut buf = [0u8; 1024];
    tokio::time::timeout(Duration::from_secs(2), proc.read(&mut buf))
      .await
      .expect("no initial output")
      .expect("read failed");
    tokio::time::sleep(Duration::from_millis(100)).await;

    proc.send_signal(libc::SIGTERM, true).unwrap();

    let deadline = Instant::now() + Duration::from_secs(3);
    let eof = loop {
      let remaining = deadline.saturating_duration_since(Instant::now());
      if remaining.is_zero() {
        break false;
      }
      match tokio::time::timeout(remaining, proc.read(&mut buf)).await {
        Err(_) => break false,
        Ok(Ok(0)) | Ok(Err(_)) => break true,
        Ok(Ok(_)) => continue,
      }
    };

    // Reap any straggler if the assert is about to fail.
    unsafe {
      libc::kill(-(proc.pid() as i32), libc::SIGKILL);
    }
    assert!(eof, "group SIGTERM should kill the child and EOF the pty");
  }
}
