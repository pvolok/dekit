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

    let mut empty_set: libc::sigset_t = unsafe { std::mem::zeroed() };
    unsafe { libc::sigemptyset(&mut empty_set) };

    let mut master_fd = -1;
    let pid = UnixProcessesWaiter::fork(
      || unsafe {
        // Some args are *mut on some BSD variants.
        #[allow(clippy::unnecessary_mut_passed)]
        let pid = libc::forkpty(
          &mut master_fd,
          null_mut(),
          null_mut(),
          &mut size.into(),
        );

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
        pid
      },
      on_wait_returned,
    )?;

    unsafe {
      let master = OwnedFd::from_raw_fd(master_fd);

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

      Ok(UnixProcess {
        pid,
        master: AsyncFd::new(master)?,
      })
    }
  }
}

/// Runs `argv` on a task's behalf (its ready or stop command): in the
/// task's cwd and env, stdio on /dev/null, in a process group of its own.
/// Only the reaper waits for it; `on_exit` gets its exit, 127 when it
/// could not be run.
pub fn spawn_command(
  argv: &[String],
  spec: &ProcessSpec,
  on_exit: Box<dyn Fn(ExitInfo) + Send + Sync>,
) -> std::io::Result<Pid> {
  use std::ffi::{OsStr, OsString};
  use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};

  // Everything the child uses is made here: between fork and exec it
  // must not allocate or take locks another thread may hold.
  let argv = argv
    .iter()
    .map(|arg| CString::new(arg.as_str()))
    .collect::<Result<Vec<_>, _>>()?;
  let Some(prog) = argv.first() else {
    return Err(std::io::Error::new(
      std::io::ErrorKind::InvalidInput,
      "empty command",
    ));
  };
  let argv_ptrs: Vec<*const libc::c_char> = argv
    .iter()
    .map(|arg| arg.as_ptr())
    .chain(std::iter::once(null()))
    .collect();
  let cwd = spec.cwd.as_deref().map(CString::new).transpose()?;
  let mut env: std::collections::BTreeMap<OsString, OsString> =
    std::env::vars_os().collect();
  for (key, value) in &spec.env {
    match value {
      Some(value) => {
        env.insert(key.into(), value.into());
      }
      None => {
        env.remove(OsStr::new(key));
      }
    }
  }
  let env = env
    .into_iter()
    .map(|(key, value)| {
      let mut entry = key.into_vec();
      entry.push(b'=');
      entry.extend_from_slice(value.as_bytes());
      CString::new(entry)
    })
    .collect::<Result<Vec<_>, _>>()?;
  let env_ptrs: Vec<*const libc::c_char> = env
    .iter()
    .map(|entry| entry.as_ptr())
    .chain(std::iter::once(null()))
    .collect();
  // CLOEXEC from the start: a fork on another thread must not inherit it.
  let null_fd = unsafe {
    libc::open(c"/dev/null".as_ptr(), libc::O_RDWR | libc::O_CLOEXEC)
  };
  if null_fd < 0 {
    return Err(std::io::Error::last_os_error());
  }
  let _null = unsafe { OwnedFd::from_raw_fd(null_fd) };
  let mut empty_set: libc::sigset_t = unsafe { std::mem::zeroed() };
  unsafe { libc::sigemptyset(&mut empty_set) };

  UnixProcessesWaiter::fork(
    || unsafe {
      let pid = libc::fork();
      if pid == 0 {
        for signo in [
          libc::SIGCHLD,
          libc::SIGHUP,
          libc::SIGINT,
          libc::SIGQUIT,
          libc::SIGTERM,
          libc::SIGALRM,
          libc::SIGPIPE,
        ] {
          libc::signal(signo, libc::SIG_DFL);
        }
        libc::pthread_sigmask(libc::SIG_SETMASK, &empty_set, null_mut());
        libc::setpgid(0, 0);
        for fd in 0..3 {
          if fd == null_fd {
            libc::fcntl(fd, libc::F_SETFD, 0);
          } else {
            libc::dup2(null_fd, fd);
          }
        }
        if let Some(cwd) = &cwd
          && libc::chdir(cwd.as_ptr()) != 0
        {
          libc::_exit(127);
        }
        // What std does: execvp searches the new env's PATH.
        #[cfg(target_vendor = "apple")]
        {
          *libc::_NSGetEnviron() = env_ptrs.as_ptr() as *mut *mut libc::c_char;
        }
        #[cfg(not(target_vendor = "apple"))]
        {
          unsafe extern "C" {
            static mut environ: *const *const libc::c_char;
          }
          environ = env_ptrs.as_ptr();
        }
        libc::execvp(prog.as_ptr(), argv_ptrs.as_ptr());
        libc::_exit(127);
      }
      pid
    },
    on_exit,
  )
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

  #[test]
  fn command_runs_in_the_task_context() {
    let dir = std::env::temp_dir()
      .join(format!("dekit_spawn_command_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("marker"), "").unwrap();
    let mut spec = ProcessSpec::from_argv(Vec::new());
    spec.cwd(dir.to_string_lossy());
    spec.env("DEKIT_SET", "yes");
    spec.env_remove("HOME");
    // No reaper runs in unit tests: the test waits for its own children.
    let status = |argv: &[&str]| {
      let argv: Vec<String> = argv.iter().map(|arg| arg.to_string()).collect();
      let pid = spawn_command(&argv, &spec, Box::new(|_| {})).unwrap();
      let mut status = 0;
      unsafe { libc::waitpid(pid.as_raw_nonzero().get(), &mut status, 0) };
      libc::WEXITSTATUS(status)
    };
    // In the task's cwd and env, leading its own process group. No `--`
    // before the group: dash's kill takes it as the pid.
    let check = "[ -f marker ] && [ \"$DEKIT_SET\" = yes ] && \
                 [ -z \"${HOME+set}\" ] && kill -0 -$$";
    assert_eq!(status(&["sh", "-c", check]), 0);
    // Could not be run: 127, as in a shell.
    assert_eq!(status(&["./missing"]), 127);
    assert_eq!(status(&["dekit-no-such-program"]), 127);
    let _ = std::fs::remove_dir_all(&dir);
  }
}
