use std::{collections::HashMap, sync::Mutex};

use anyhow::{anyhow, bail};
use rustix::{
  process::{WaitOptions, WaitStatus},
  termios::Pid,
};
use tokio::signal::unix::SignalKind;

use crate::kernel::task::ExitInfo;

/// The one reaper: `reap_now` collects every exited child of this process
/// with `waitpid(-1)`, one call per exit however many children there are.
/// So every child must be registered here (`wait_for`, `wait_for_child`);
/// waiting for one any other way races the reaper and fails with ECHILD.
pub struct UnixProcessesWaiter {
  thread: tokio::task::JoinHandle<anyhow::Result<()>>,

  listeners: HashMap<Pid, Box<dyn Fn(ExitInfo) + Send + Sync>>,
  unclaimed: HashMap<Pid, ExitInfo>,
  /// While paused nothing is reaped: exits stay zombies for whichever
  /// image ends up running.
  paused: bool,
}

static GLOBAL: Mutex<Option<UnixProcessesWaiter>> = Mutex::new(None);

fn exit_info(status: WaitStatus) -> ExitInfo {
  ExitInfo {
    code: status.exit_status(),
    signal: status.terminating_signal(),
  }
}

impl UnixProcessesWaiter {
  /// Hands `f` the exit of `pid`, at once if the reaper collected it
  /// before this registration.
  pub fn wait_for(pid: Pid, f: Box<dyn Fn(ExitInfo) + Send + Sync>) {
    match GLOBAL.lock() {
      Ok(mut guard) => {
        if let Some(pw) = guard.as_mut() {
          match pw.unclaimed.remove(&pid) {
            Some(info) => {
              f(info);
            }
            None => {
              pw.listeners.insert(pid, f);
            }
          }
        }
      }
      Err(_) => (),
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
    let mut holder =
      GLOBAL.lock().map_err(|_e| anyhow!("Mutex is poisoned."))?;
    if holder.is_some() {
      bail!("UnixProcessWaiter is already initialized.");
    }
    let mut signals = tokio::signal::unix::signal(SignalKind::child())?;
    let thread: tokio::task::JoinHandle<anyhow::Result<()>> =
      tokio::spawn(async move {
        while let Some(()) = signals.recv().await {
          Self::reap_now();
        }
        Ok(())
      });
    *holder = Some(UnixProcessesWaiter {
      thread,

      listeners: Default::default(),
      unclaimed: Default::default(),
      paused,
    });

    Ok(())
  }

  /// Collects every exited child now, unless paused. Safe to call at any
  /// time: a signal that arrives later just reaps nothing.
  pub fn reap_now() {
    let mut guard = match GLOBAL.lock() {
      Ok(guard) => guard,
      Err(e) => {
        log::error!("SIGCHLD waiter lock error: {}", e);
        return;
      }
    };
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
          match pw.listeners.remove(&pid) {
            Some(listener) => listener(info),
            None => {
              pw.unclaimed.insert(pid, info);
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
    if let Ok(mut guard) = GLOBAL.lock()
      && let Some(pw) = guard.as_mut()
    {
      pw.paused = true;
    }
  }

  pub fn resume() {
    if let Ok(mut guard) = GLOBAL.lock()
      && let Some(pw) = guard.as_mut()
    {
      pw.paused = false;
    }
    Self::reap_now();
  }

  /// A child spawned with `std::process`, which must never be waited for
  /// directly (see above).
  pub fn wait_for_child(
    child: std::process::Child,
    f: Box<dyn Fn(ExitInfo) + Send + Sync>,
  ) {
    if let Some(pid) = Pid::from_raw(child.id() as i32) {
      Self::wait_for(pid, f);
    }
  }

  pub fn uninit() -> anyhow::Result<()> {
    let mut holder =
      GLOBAL.lock().map_err(|_e| anyhow!("Mutex is poisoned."))?;
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
