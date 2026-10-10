use std::{
  collections::HashMap,
  panic::AssertUnwindSafe,
  ptr::null_mut,
  sync::{Mutex, MutexGuard},
};

use anyhow::bail;
use rustix::{
  process::{WaitOptions, WaitStatus},
  termios::Pid,
};
use tokio::signal::unix::SignalKind;

use crate::kernel::task::ExitInfo;

/// The one reaper: `reap_now` collects every exited child of this process
/// with `waitpid(-1)`, one call per exit however many children there are.
/// So every child must be registered here (`fork`, `wait_for`); waiting
/// for one any other way races the reaper and fails with ECHILD.
///
/// An exit reaped before its child is registered waits in `unclaimed`
/// with its place in the reap order. A spawner takes `mark()` before it
/// forks (`fork` does): an exit reaped before the mark belongs to an
/// earlier process that had the same pid (one nobody waited for, such as
/// a check command an upgrade cut off), never to the new child.
///
/// A listener that panics is logged and skipped. A panic in the reaper's
/// own bookkeeping ends the runner: exits could no longer be reported.
pub struct UnixProcessesWaiter {
  thread: tokio::task::JoinHandle<()>,

  listeners: HashMap<Pid, Box<dyn Fn(ExitInfo) + Send + Sync>>,
  unclaimed: HashMap<Pid, (u64, ExitInfo)>,
  /// Exits reaped so far: the next one's place in the reap order.
  reaped: u64,
  /// While paused nothing is reaped: exits stay zombies for whichever
  /// image ends up running.
  paused: bool,
}

static GLOBAL: Mutex<Option<UnixProcessesWaiter>> = Mutex::new(None);

fn global() -> MutexGuard<'static, Option<UnixProcessesWaiter>> {
  match GLOBAL.lock() {
    Ok(guard) => guard,
    Err(_) => fatal(),
  }
}

fn fatal() -> ! {
  log::error!("The child reaper failed; exits can no longer be reported");
  std::process::abort()
}

fn exit_info(status: WaitStatus) -> ExitInfo {
  ExitInfo {
    code: status.exit_status(),
    signal: status.terminating_signal(),
    ready_timeout: false,
  }
}

fn call(f: &(dyn Fn(ExitInfo) + Send + Sync), info: ExitInfo) {
  if std::panic::catch_unwind(AssertUnwindSafe(|| f(info))).is_err() {
    log::error!("An exit listener panicked");
  }
}

impl UnixProcessesWaiter {
  /// Whether the reaper is running in this process: then it alone may
  /// wait for a child.
  pub fn installed() -> bool {
    global().is_some()
  }

  /// Where the reap order is now; taken before a fork. Only a child not
  /// spawned by `fork` (the clipboard helper) needs it directly.
  pub fn mark() -> u64 {
    global().as_ref().map_or(0, |pw| pw.reaped)
  }

  /// Forks with `fork`, which returns the pid in the parent and in the
  /// child must exec or `_exit`, and hands `f` the child's exit. Signals
  /// are blocked across the fork so none runs this process's handlers in
  /// the child; the child resets them and unblocks.
  pub fn fork(
    fork: impl FnOnce() -> libc::pid_t,
    f: Box<dyn Fn(ExitInfo) + Send + Sync>,
  ) -> std::io::Result<Pid> {
    let mark = Self::mark();
    let pid = unsafe {
      let mut block_set: libc::sigset_t = std::mem::zeroed();
      let mut old_set: libc::sigset_t = std::mem::zeroed();
      libc::sigfillset(&mut block_set);
      libc::pthread_sigmask(libc::SIG_SETMASK, &block_set, &mut old_set);
      let pid = fork();
      let err = std::io::Error::last_os_error();
      libc::pthread_sigmask(libc::SIG_SETMASK, &old_set, null_mut());
      if pid < 0 {
        return Err(err);
      }
      Pid::from_raw_unchecked(pid)
    };
    Self::wait_for(pid, mark, f);
    Ok(pid)
  }

  /// Hands `f` the exit of `pid`, at once if the reaper collected it
  /// since `mark` (0 for a child adopted after an upgrade).
  pub fn wait_for(pid: Pid, mark: u64, f: Box<dyn Fn(ExitInfo) + Send + Sync>) {
    let mut guard = global();
    if let Some(pw) = guard.as_mut() {
      match pw.unclaimed.remove(&pid) {
        Some((place, info)) if place >= mark => call(&*f, info),
        // Before the mark: an earlier process with this pid.
        Some(_) | None => {
          pw.listeners.insert(pid, f);
        }
      }
    }
  }

  /// Sends `sig` to `pid`, or to the process group it leads, unless the
  /// reaper has collected it: its number may belong to another process
  /// by then. Under the lock a registered pid is not reaped yet, since the
  /// reaper reaps and takes the listener under the same lock.
  pub fn kill(pid: Pid, sig: i32, group: bool) {
    let guard = global();
    if let Some(pw) = guard.as_ref()
      && pw.listeners.contains_key(&pid)
    {
      let pid: i32 = pid.as_raw_nonzero().into();
      // No such group (the child has not made it yet, or has left it):
      // the child alone.
      if !(group && unsafe { libc::kill(-pid, sig) } == 0) {
        unsafe { libc::kill(pid, sig) };
      }
    }
  }

  pub fn init() -> anyhow::Result<()> {
    Self::start(false)
  }

  /// Like `init`, with nothing reaped until `resume`: after an upgrade's
  /// exec, every pid the snapshot names stays this process's unreaped
  /// child until the runner is taken over.
  pub fn init_paused() -> anyhow::Result<()> {
    Self::start(true)
  }

  fn start(paused: bool) -> anyhow::Result<()> {
    let mut holder = global();
    if holder.is_some() {
      bail!("UnixProcessWaiter is already initialized.");
    }
    let mut signals = tokio::signal::unix::signal(SignalKind::child())?;
    let thread = tokio::spawn(async move {
      while let Some(()) = signals.recv().await {
        if std::panic::catch_unwind(Self::reap_now).is_err() {
          fatal();
        }
      }
    });
    *holder = Some(UnixProcessesWaiter {
      thread,

      listeners: Default::default(),
      unclaimed: Default::default(),
      reaped: 0,
      paused,
    });

    Ok(())
  }

  /// Collects every exited child now, unless paused. Safe to call at any
  /// time: a signal that arrives later just reaps nothing.
  pub fn reap_now() {
    let mut guard = global();
    let Some(pw) = guard.as_mut() else {
      return;
    };
    if pw.paused {
      return;
    }
    loop {
      match rustix::process::wait(WaitOptions::NOHANG) {
        Ok(Some((pid, wait_status))) => {
          let info = exit_info(wait_status);
          let place = pw.reaped;
          pw.reaped += 1;
          match pw.listeners.remove(&pid) {
            Some(listener) => call(&*listener, info),
            None => {
              pw.unclaimed.insert(pid, (place, info));
            }
          }
        }
        Ok(None) => break,
        Err(e) => {
          // ECHILD - No spawned processes.
          if e.raw_os_error() != libc::ECHILD {
            log::error!(
              "ProcessesWaiter wait() error: {} ({})",
              e.kind(),
              e.raw_os_error()
            );
          }
          break;
        }
      }
    }
  }

  pub fn pause() {
    if let Some(pw) = global().as_mut() {
      pw.paused = true;
    }
  }

  pub fn resume() {
    if let Some(pw) = global().as_mut() {
      pw.paused = false;
    }
    Self::reap_now();
  }

  pub fn uninit() -> anyhow::Result<()> {
    let mut holder = global();
    match holder.as_mut() {
      Some(pw) => {
        pw.thread.abort();
      }
      None => bail!("Cannot uninit None UnixProcessWaiter."),
    }
    *holder = None;

    Ok(())
  }
}
