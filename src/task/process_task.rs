use std::future::{Future, pending};
use std::pin::Pin;
use std::time::Duration;

use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

use crate::error::ResultLogger;
use crate::kernel::kernel_message::{
  KernelCommand, SharedVt, TaskContext, TaskRegistration, TaskSelector,
};
use crate::kernel::task::{
  ExitInfo, ReadyMode, RestartMode, STOP_TIMEOUT, TaskCmd, TaskDef, TaskId,
  TaskKind,
};
use crate::kernel::task_key::TaskKey;
use crate::kernel::task_path::TaskPath;
use crate::kernel::task_screen::{
  DEFAULT_SIZE, TaskScreen, TaskScreenCmd, TaskScreenEffect,
};
use crate::process::NativeProcess;
use crate::process::process::Process as _;
use crate::process::process_spec::ProcessSpec;
#[cfg(unix)]
use crate::task::logger::LogSink;
use crate::task::logger::{LogSpec, spawn_logger};
#[cfg(windows)]
use crate::task::ready::command;
use crate::task::ready::{
  Probe, ReadyCheck, ReadyConfig, VisibleLine, parse_http_url, wait_ready,
};
use crate::term::key::Key;
use crate::term::vt::emit::{self, KeyEncodeModes};
use crate::term::{Screen, Winsize};
use crate::upgrade::snapshot as snap;

/// An OS signal a `Signal` stop can deliver. The name table and the libc
/// mapping are generated from one list so they can't drift. On Windows only
/// INT/TERM/KILL have a (terminate) fallback; every other signal is ignored.
macro_rules! signals {
  ($($name:literal => $variant:ident => $libc:ident,)+) => {
    #[derive(Clone, Copy, Debug)]
    pub enum Sig {
      $($variant,)+
    }

    impl Sig {
      pub fn from_name(name: &str) -> Option<Sig> {
        match name {
          $($name => Some(Sig::$variant),)+
          _ => None,
        }
      }

      pub fn name(self) -> &'static str {
        match self {
          $(Sig::$variant => $name,)+
        }
      }

      #[cfg(not(windows))]
      fn to_libc(self) -> i32 {
        match self {
          $(Sig::$variant => libc::$libc,)+
        }
      }
    }
  };
}

signals! {
  "SIGHUP" => Hup => SIGHUP,
  "SIGINT" => Int => SIGINT,
  "SIGQUIT" => Quit => SIGQUIT,
  "SIGILL" => Ill => SIGILL,
  "SIGTRAP" => Trap => SIGTRAP,
  "SIGABRT" => Abrt => SIGABRT,
  "SIGBUS" => Bus => SIGBUS,
  "SIGFPE" => Fpe => SIGFPE,
  "SIGKILL" => Kill => SIGKILL,
  "SIGUSR1" => Usr1 => SIGUSR1,
  "SIGSEGV" => Segv => SIGSEGV,
  "SIGUSR2" => Usr2 => SIGUSR2,
  "SIGPIPE" => Pipe => SIGPIPE,
  "SIGALRM" => Alrm => SIGALRM,
  "SIGTERM" => Term => SIGTERM,
  "SIGCHLD" => Chld => SIGCHLD,
  "SIGCONT" => Cont => SIGCONT,
  "SIGSTOP" => Stop => SIGSTOP,
  "SIGTSTP" => Tstp => SIGTSTP,
  "SIGTTIN" => Ttin => SIGTTIN,
  "SIGTTOU" => Ttou => SIGTTOU,
  "SIGURG" => Urg => SIGURG,
  "SIGXCPU" => Xcpu => SIGXCPU,
  "SIGXFSZ" => Xfsz => SIGXFSZ,
  "SIGVTALRM" => Vtalrm => SIGVTALRM,
  "SIGPROF" => Prof => SIGPROF,
  "SIGWINCH" => Winch => SIGWINCH,
  "SIGSYS" => Sys => SIGSYS,
}

#[derive(Clone, Debug)]
pub enum StopSignal {
  Signal {
    sig: Sig,
    group: bool,
  },
  /// Typed into the task's terminal.
  Keys(Vec<Key>),
  /// A program to run, for tools that stop on a command rather than a
  /// signal (e.g. `podman compose down`). The task is expected to exit on
  /// its own once it has run.
  Cmd(Vec<String>),
}

impl Default for StopSignal {
  fn default() -> Self {
    StopSignal::Signal {
      sig: Sig::Term,
      group: true,
    }
  }
}

impl StopSignal {
  /// Target for a force-kill (the grace-period timeout or an explicit `Kill`):
  /// honor a `Signal` stop's own choice, otherwise force-kill the whole group
  /// so orphaned children don't leak.
  fn kill_group(&self) -> bool {
    match self {
      StopSignal::Signal { group, .. } => *group,
      StopSignal::Keys(_) | StopSignal::Cmd(_) => true,
    }
  }
}

pub struct ProcessTaskConfig {
  pub spec: ProcessSpec,
  pub label: Option<String>,
  pub kind: TaskKind,
  pub stop: StopSignal,
  pub stop_timeout: Duration,
  pub log: Option<LogSpec>,
  pub restart: RestartMode,
  /// Without it the task is ready as soon as it starts.
  pub ready: Option<ReadyConfig>,
  pub scrollback_len: usize,
  pub mouse_scroll_speed: usize,
  pub deps: Vec<TaskSelector>,
  pub tags: Vec<String>,
  /// Pin to init at registration, so a registered task is already
  /// started with no separate `Start` command.
  pub pinned: bool,
}

#[cfg(test)]
impl ProcessTaskConfig {
  pub fn new(spec: ProcessSpec) -> Self {
    Self {
      spec,
      label: None,
      kind: TaskKind::Service,
      stop: StopSignal::default(),
      stop_timeout: STOP_TIMEOUT,
      log: None,
      restart: RestartMode::Never,
      ready: None,
      scrollback_len: 1000,
      mouse_scroll_speed: 5,
      deps: Vec::new(),
      tags: Vec::new(),
      pinned: false,
    }
  }
}

pub fn process_task_registration(
  task_id: TaskId,
  key: Option<TaskKey>,
  config: ProcessTaskConfig,
) -> TaskRegistration {
  let vt = SharedVt::new(Screen::new(DEFAULT_SIZE, config.scrollback_len));
  registration(task_id, key, config, vt, None)
}

/// A process task re-created around its inherited child and PTY.
#[cfg(unix)]
pub fn process_task_from_snapshot(
  task_id: TaskId,
  key: Option<TaskKey>,
  saved: &snap::Task,
  process: &snap::ProcessTask,
) -> anyhow::Result<TaskRegistration> {
  let deps = saved
    .deps
    .iter()
    .map(|id| TaskSelector::Id(TaskId(*id)))
    .collect();
  let config = process_task_config_from_snapshot(saved, process, deps)?;
  process_task_resumed(
    task_id,
    key,
    config,
    &process.screen,
    resumed_instance(saved, process),
  )
}

/// The saved child as its task resumes it. The kernel waits for a ready
/// report only in a restored `Running` state; in any other it keeps what
/// it has (a `Ready` task stays ready when the new config adds a check),
/// so no check runs.
#[cfg(unix)]
pub fn resumed_instance(
  saved: &snap::Task,
  process: &snap::ProcessTask,
) -> Option<snap::Instance> {
  let mut instance = process.instance.clone()?;
  match saved.state {
    snap::TaskState::Running {} => (),
    snap::TaskState::Idle {}
    | snap::TaskState::Starting {}
    | snap::TaskState::Ready {}
    | snap::TaskState::Stopping {}
    | snap::TaskState::Backoff(_)
    | snap::TaskState::Done(_)
    | snap::TaskState::Exited(_) => instance.ready_sent = true,
  }
  Some(instance)
}

pub fn process_task_config_from_snapshot(
  saved: &snap::Task,
  process: &snap::ProcessTask,
  deps: Vec<TaskSelector>,
) -> anyhow::Result<ProcessTaskConfig> {
  let spec = ProcessSpec {
    prog: process.spec.prog.clone(),
    args: process.spec.args.clone(),
    cwd: process.spec.cwd.clone(),
    env: process.spec.env.iter().cloned().collect(),
  };
  let stop = match &process.stop {
    snap::StopSignal::Shutdown {} => StopSignal::Signal {
      sig: Sig::Term,
      group: true,
    },
    snap::StopSignal::Kill {} => StopSignal::Signal {
      sig: Sig::Kill,
      group: true,
    },
    snap::StopSignal::Signal { sig, group } => StopSignal::Signal {
      sig: Sig::from_name(sig)
        .ok_or_else(|| anyhow::anyhow!("unknown stop signal {sig}"))?,
      group: *group,
    },
    snap::StopSignal::SendKeys { keys } => StopSignal::Keys(keys.clone()),
    // An older binary's line, meant for the system shell.
    snap::StopSignal::Cmd { cmd } => {
      StopSignal::Cmd(crate::parse_shell::system_argv(cmd))
    }
    snap::StopSignal::Program { argv } => StopSignal::Cmd(argv.clone()),
  };
  let log = process.log.as_ref().map(|log| LogSpec {
    config: crate::config::task_log::TaskLogConfig {
      enabled: log.enabled,
      dir: log.dir.as_ref().map(std::path::PathBuf::from),
      file: log.file.as_ref().map(std::path::PathBuf::from),
      mode: Some(if log.truncate {
        crate::config::task_log::LogMode::Truncate
      } else {
        crate::config::task_log::LogMode::Append
      }),
    },
    name: log.name.clone(),
  });
  let check = match (&process.ready_log, &process.ready_probe) {
    (None, None) => None,
    (Some(text), None) => Some(ReadyCheck::Log(text.clone())),
    (None, Some(probe)) => Some(ReadyCheck::Probe {
      probe: match &probe.check {
        snap::ReadyCheck::Tcp { host, port } => Probe::Tcp {
          host: host.clone(),
          port: *port,
        },
        snap::ReadyCheck::Http { url } => {
          Probe::Http(parse_http_url(url).map_err(anyhow::Error::msg)?)
        }
        snap::ReadyCheck::Cmd { argv } => Probe::Cmd { argv: argv.clone() },
        snap::ReadyCheck::File { path } => Probe::File {
          path: std::path::PathBuf::from(path),
        },
      },
      interval: Duration::from_millis(probe.interval_ms),
    }),
    (Some(_), Some(_)) => anyhow::bail!("a task has two ready checks"),
  };
  Ok(ProcessTaskConfig {
    spec,
    label: saved.label.clone(),
    kind: if saved.job {
      TaskKind::Job
    } else {
      TaskKind::Service
    },
    stop,
    stop_timeout: saved
      .stop_timeout_ms
      .map_or(STOP_TIMEOUT, Duration::from_millis),
    log,
    restart: saved.restart.into(),
    ready: check.map(|check| ReadyConfig {
      check,
      timeout: saved.ready_timeout_ms.map(Duration::from_millis),
    }),
    scrollback_len: process.scrollback_len,
    mouse_scroll_speed: process.mouse_scroll_speed,
    deps,
    tags: saved.tags.clone(),
    pinned: saved.pinned,
  })
}

/// A process task around a saved screen and, if the child is still this
/// runner's, its inherited PTY; the config may be newer than the child.
pub fn process_task_resumed(
  task_id: TaskId,
  key: Option<TaskKey>,
  config: ProcessTaskConfig,
  screen: &snap::Screen,
  instance: Option<snap::Instance>,
) -> anyhow::Result<TaskRegistration> {
  let vt = SharedVt::new(Screen::from_snapshot(screen)?);
  Ok(registration(task_id, key, config, vt, instance))
}

fn registration(
  task_id: TaskId,
  key: Option<TaskKey>,
  config: ProcessTaskConfig,
  vt: SharedVt,
  instance: Option<snap::Instance>,
) -> TaskRegistration {
  let task_vt = vt.clone();
  let (space, path) = match key.clone() {
    Some(key) => (key.space, Some(key.path)),
    None => (Default::default(), None),
  };
  let mut config = config;
  TaskRegistration::async_task(
    task_id,
    TaskDef {
      kind: config.kind,
      ready: match &config.ready {
        Some(ready) => ReadyMode::Reported {
          timeout: ready.timeout,
        },
        None => ReadyMode::Immediate,
      },
      restart: config.restart,
      stop_timeout: config.stop_timeout,
      deps: std::mem::take(&mut config.deps),
      space,
      path,
      label: config.label.take(),
      vt: Some(vt),
      tags: std::mem::take(&mut config.tags),
      pinned: config.pinned,
      saved_pin: false,
    },
    move |ctx, receiver| async move {
      process_main(ctx, receiver, key, task_vt, config, instance).await;
    },
  )
}

/// What the task tracks about its current child besides the process
/// itself; carried across an upgrade with it.
#[derive(Default)]
struct Instance {
  /// Where the reaper hands the exit status; gone once it has.
  exits: Option<UnboundedReceiver<ExitInfo>>,
  exit_info: Option<ExitInfo>,
  stdout_eof: bool,
  ready_sent: bool,
  /// A stop was sent: no ready check runs.
  stop_sent: bool,
  ready_line: VisibleLine,
  /// The log path is resolved per spawn (it may contain the pid).
  current_log: Option<(std::path::PathBuf, u64)>,
}

impl Instance {
  fn exited(&mut self, info: ExitInfo, process: Option<&mut NativeProcess>) {
    self.exit_info = Some(info);
    self.exits = None;
    if let Some(p) = process {
      p.on_exited();
    }
  }

  /// Feeds output to the log check, if there is one and the instance is
  /// not ready; true when this makes it ready.
  fn log_ready(&mut self, ready: &Option<ReadyConfig>, bytes: &[u8]) -> bool {
    if let Some(ReadyConfig {
      check: ReadyCheck::Log(text),
      ..
    }) = ready
      && !self.ready_sent
      && self.ready_line.feed(text.as_bytes(), bytes)
    {
      self.ready_sent = true;
      self.ready_line = VisibleLine::default();
      return true;
    }
    false
  }
}

fn snapshot(
  config: &ProcessTaskConfig,
  process: Option<&NativeProcess>,
  task_screen: &TaskScreen,
  instance: &Instance,
) -> snap::ProcessTask {
  let stop = match &config.stop {
    StopSignal::Signal { sig, group } => snap::StopSignal::Signal {
      sig: sig.name().to_string(),
      group: *group,
    },
    StopSignal::Keys(keys) => snap::StopSignal::SendKeys { keys: keys.clone() },
    StopSignal::Cmd(argv) => snap::StopSignal::Program { argv: argv.clone() },
  };
  let (ready_log, ready_probe) = match config.ready.as_ref().map(|r| &r.check) {
    Some(ReadyCheck::Log(text)) => (Some(text.clone()), None),
    Some(ReadyCheck::Probe { probe, interval }) => (
      None,
      Some(snap::ReadyProbe {
        check: match probe {
          Probe::Tcp { host, port } => snap::ReadyCheck::Tcp {
            host: host.clone(),
            port: *port,
          },
          Probe::Http(url) => snap::ReadyCheck::Http {
            url: url.url.clone(),
          },
          Probe::Cmd { argv } => snap::ReadyCheck::Cmd { argv: argv.clone() },
          Probe::File { path } => snap::ReadyCheck::File {
            path: path.to_string_lossy().into_owned(),
          },
        },
        interval_ms: interval.as_millis() as u64,
      }),
    ),
    None => (None, None),
  };
  #[cfg(unix)]
  let instance = process.map(|p| snap::Instance {
    pid: p.pid(),
    master_fd: p.master_fd(),
    exit: instance.exit_info.map(Into::into),
    stdout_eof: instance.stdout_eof,
    ready_sent: instance.ready_sent,
    // Only a polled check still waiting reads it; written only then, so
    // an older binary can take back any other snapshot.
    stop_sent: instance.stop_sent
      && ready_probe.is_some()
      && !instance.ready_sent
      && instance.exit_info.is_none(),
    ready_line: snap::to_base64(&instance.ready_line.saved()),
    log_path: instance
      .current_log
      .as_ref()
      .map(|(path, _)| path.to_string_lossy().into_owned()),
  });
  #[cfg(not(unix))]
  let instance = {
    let _ = (process, instance);
    None
  };
  snap::ProcessTask {
    spec: snap::ProcessSpec {
      prog: config.spec.prog.clone(),
      args: config.spec.args.clone(),
      cwd: config.spec.cwd.clone(),
      env: config
        .spec
        .env
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect(),
    },
    stop,
    log: config.log.as_ref().map(|log| snap::LogSpec {
      name: log.name.clone(),
      enabled: log.config.enabled,
      dir: log
        .config
        .dir
        .as_ref()
        .map(|p| p.to_string_lossy().into_owned()),
      file: log
        .config
        .file
        .as_ref()
        .map(|p| p.to_string_lossy().into_owned()),
      truncate: log.config.mode() == crate::config::task_log::LogMode::Truncate,
    }),
    ready_log,
    ready_probe,
    scrollback_len: config.scrollback_len,
    mouse_scroll_speed: config.mouse_scroll_speed,
    instance,
    screen: task_screen
      .vt()
      .read()
      .map(|screen| screen.snapshot())
      .unwrap_or_else(|_| Screen::new(DEFAULT_SIZE, 0).snapshot()),
  }
}

async fn process_main(
  ctx: TaskContext,
  mut receiver: UnboundedReceiver<TaskCmd>,
  key: Option<TaskKey>,
  vt: SharedVt,
  config: ProcessTaskConfig,
  saved: Option<snap::Instance>,
) {
  let mut task_screen =
    TaskScreen::new(ctx.task_id, vt, config.mouse_scroll_speed);
  let mut screen_effects: Vec<TaskScreenEffect> = Vec::new();

  // The child must not outlive this future (it panicked, or the kernel
  // went away): one that ignores SIGHUP survives the PTY's hangup.
  #[cfg(unix)]
  let kill_group = config.stop.kill_group();
  let mut process =
    scopeguard::guard(None, move |process: Option<NativeProcess>| {
      #[cfg(unix)]
      if let Some(p) = process {
        crate::process::unix_processes_waiter::UnixProcessesWaiter::kill(
          p.pid,
          libc::SIGKILL,
          kill_group,
        );
      }
      #[cfg(windows)]
      drop(process);
    });
  let mut instance = Instance::default();
  let mut read_buf = [0u8; 8 * 1024];
  let mut key_buf: Vec<u8> = Vec::new();
  // Frozen for an upgrade: no reads until thawed.
  let mut frozen = false;
  // The ready check that polls, while the task is starting.
  let mut probe: Option<ProbeFuture> = None;

  #[cfg(unix)]
  if let Some(saved) = saved {
    match adopt_native(&saved) {
      Ok((adopted, receiver)) => {
        *process = Some(adopted);
        instance.exits = receiver;
        instance.exit_info = saved.exit.map(Into::into);
        instance.stdout_eof = saved.stdout_eof;
        instance.ready_sent = saved.ready_sent;
        instance.stop_sent = saved.stop_sent;
        // Checked again like new output: an older binary saved raw output
        // it had not searched yet, since it checked only at a `\n`.
        let line = snap::from_base64(&saved.ready_line).unwrap_or_default();
        if instance.log_ready(&config.ready, &line) {
          ctx.send(KernelCommand::TaskReady);
        }
        probe = start_probe(&config, &instance);
        if let Some(path) = saved.log_path {
          let path = std::path::PathBuf::from(path);
          let id = task_screen.add_logger(spawn_logger(LogSink {
            path: path.clone(),
            append: true,
          }));
          instance.current_log = Some((path, id));
        }
      }
      Err(err) => {
        log::error!("Failed to adopt child {}: {err}", saved.pid);
        // No process to wait for: without this report the task would
        // stay active forever.
        ctx.send(KernelCommand::TaskStopped(ExitInfo::error()));
      }
    }
  }
  #[cfg(not(unix))]
  let _ = saved;

  loop {
    if instance.stdout_eof
      && let Some(info) = instance.exit_info
      && process.take().is_some()
    {
      ctx.send(KernelCommand::TaskStopped(info));
    }

    enum Next {
      Cmd(Option<TaskCmd>),
      Read(std::io::Result<usize>),
      Exited(Option<ExitInfo>),
      Ready,
    }
    let read_fut = async {
      match process.as_mut() {
        Some(p) if !instance.stdout_eof && !frozen => {
          p.read(&mut read_buf).await
        }
        _ => pending().await,
      }
    };
    let exit_fut = async {
      match instance.exits.as_mut() {
        Some(exits) => exits.recv().await,
        None => pending().await,
      }
    };
    let probe_fut = async {
      match probe.as_mut() {
        Some(probe) => probe.await,
        None => pending().await,
      }
    };
    let next = tokio::select! {
      cmd = receiver.recv() => Next::Cmd(cmd),
      n = read_fut => Next::Read(n),
      info = exit_fut => Next::Exited(info),
      () = probe_fut => Next::Ready,
    };

    match next {
      Next::Cmd(None) => break,
      Next::Cmd(Some(cmd)) => match cmd {
        TaskCmd::Start if process.is_none() => {
          match start_instance(&ctx, &config.spec, task_screen.vt()) {
            Ok((p, receiver)) => {
              instance.exit_info = None;
              instance.stdout_eof = false;
              instance.ready_line = VisibleLine::default();
              instance.ready_sent = false;
              instance.stop_sent = false;
              probe = start_probe(&config, &instance);
              update_log_observer(
                &mut task_screen,
                &config.log,
                &mut instance.current_log,
                ctx.task_id,
                p.pid(),
              );
              *process = Some(p);
              instance.exits = Some(receiver);
            }
            Err(message) => {
              task_screen
                .process(message.as_bytes(), &mut screen_effects)
                .await;
              screen_effects.clear();
            }
          }
        }
        TaskCmd::Start => {}
        TaskCmd::Stop => {
          probe = None;
          instance.stop_sent = true;
          if let Some(p) = process.as_mut() {
            stop_process(p, &config.stop, task_screen.vt(), &config.spec).await;
          }
        }
        TaskCmd::Kill => {
          probe = None;
          instance.stop_sent = true;
          if let Some(p) = process.as_mut() {
            p.kill(config.stop.kill_group()).await.log_ignore();
          }
        }
        TaskCmd::Duplicate(label) => {
          let new_id = ctx.alloc_id();
          let key = match &key {
            Some(k) => TaskPath::new(format!("{}_{}", k.path, new_id.0))
              .ok()
              .map(|path| TaskKey::new(k.space.clone(), path)),
            None => TaskPath::new(new_id.0.to_string())
              .ok()
              .map(TaskKey::default_space),
          };
          let ack = ctx.register_task(process_task_registration(
            new_id,
            key,
            ProcessTaskConfig {
              spec: config.spec.clone(),
              kind: config.kind,
              stop: config.stop.clone(),
              stop_timeout: config.stop_timeout,
              log: None,
              restart: config.restart,
              ready: config.ready.clone(),
              scrollback_len: config.scrollback_len,
              mouse_scroll_speed: config.mouse_scroll_speed,
              deps: Vec::new(),
              label,
              tags: Vec::new(),
              pinned: true,
            },
          ));
          tokio::spawn(async move {
            if let Ok(Err(err)) = ack.await {
              log::warn!("Duplicate failed: {err}");
            }
          });
        }
        TaskCmd::Freeze(number) => {
          // An exit the reaper collected before reaping paused is already
          // queued; the snapshot must carry it.
          if let Some(info) = instance
            .exits
            .as_mut()
            .and_then(|receiver| receiver.try_recv().ok())
          {
            instance.exited(info, process.as_mut());
          }
          task_screen.flush_loggers().await;
          frozen = true;
          probe = None;
          let saved =
            snapshot(&config, process.as_ref(), &task_screen, &instance);
          ctx.send(KernelCommand::TaskFrozen(
            number,
            snap::TaskKind::Process(saved),
          ));
        }
        TaskCmd::Thaw => {
          frozen = false;
          if process.is_some() {
            probe = start_probe(&config, &instance);
          }
        }
        TaskCmd::Msg(msg) => match msg.downcast::<TaskScreenCmd>() {
          Ok(cmd) => {
            task_screen.handle_cmd(*cmd, &mut screen_effects);
            apply_effects(
              &mut screen_effects,
              &mut process,
              task_screen.vt(),
              &mut key_buf,
            )
            .await;
          }
          Err(_) => log::error!("ProcessTask received unknown Msg"),
        },
      },

      // Each instance exits once; `None` means the reaper is gone.
      Next::Exited(Some(info)) => {
        probe = None;
        instance.exited(info, process.as_mut());
      }
      Next::Exited(None) => instance.exits = None,
      Next::Ready => {
        probe = None;
        instance.ready_sent = true;
        ctx.send(KernelCommand::TaskReady);
      }
      Next::Read(Ok(0)) => instance.stdout_eof = true,
      Next::Read(Ok(n)) => {
        if instance.log_ready(&config.ready, &read_buf[..n]) {
          ctx.send(KernelCommand::TaskReady);
        }
        task_screen
          .process(&read_buf[..n], &mut screen_effects)
          .await;
        apply_effects(
          &mut screen_effects,
          &mut process,
          task_screen.vt(),
          &mut key_buf,
        )
        .await;
      }
      Next::Read(Err(e)) => {
        log::warn!("Process read error: {}", e);
        instance.stdout_eof = true;
      }
    }
  }
}

type ProbeFuture = Pin<Box<dyn Future<Output = ()> + Send>>;

/// The ready check that polls, unless the instance is ready, is being
/// stopped, or has exited.
fn start_probe(
  config: &ProcessTaskConfig,
  instance: &Instance,
) -> Option<ProbeFuture> {
  if instance.ready_sent || instance.stop_sent || instance.exit_info.is_some() {
    return None;
  }
  match &config.ready {
    Some(ReadyConfig {
      check: ReadyCheck::Probe { probe, interval },
      ..
    }) => Some(Box::pin(wait_ready(
      probe.clone(),
      *interval,
      config.spec.clone(),
    ))),
    Some(ReadyConfig {
      check: ReadyCheck::Log(_),
      ..
    })
    | None => None,
  }
}

fn update_log_observer(
  task_screen: &mut TaskScreen,
  log: &Option<LogSpec>,
  current: &mut Option<(std::path::PathBuf, u64)>,
  task_id: TaskId,
  pid: u32,
) {
  let Some(log) = log else {
    return;
  };
  let Some(sink) = log.resolve(task_id.0, pid) else {
    return;
  };
  if let Some((path, _)) = current {
    if *path == sink.path {
      return;
    }
  }
  if let Some((_, id)) = current.take() {
    task_screen.remove_logger(id);
  }
  let path = sink.path.clone();
  let id = task_screen.add_logger(spawn_logger(sink));
  *current = Some((path, id));
}

fn start_instance(
  ctx: &TaskContext,
  spec: &ProcessSpec,
  vt: &SharedVt,
) -> Result<(NativeProcess, UnboundedReceiver<ExitInfo>), String> {
  let size = match vt.read() {
    Ok(screen) => {
      let s = screen.size();
      Winsize {
        x: s.width,
        y: s.height,
        x_px: 0,
        y_px: 0,
      }
    }
    Err(_) => Winsize {
      x: 80,
      y: 24,
      x_px: 0,
      y_px: 0,
    },
  };
  if let Ok(mut screen) = vt.write() {
    screen.reset();
    screen.set_size(size.y, size.x);
  }
  match spawn_native(ctx, spec, size) {
    Ok(spawned) => {
      ctx.send(KernelCommand::TaskStarted);
      Ok(spawned)
    }
    Err(err) => {
      log::warn!("Process spawn error: {:#}", err);
      ctx.send(KernelCommand::TaskStopped(ExitInfo::error()));
      Err(spawn_error_message(spec, &err))
    }
  }
}

/// What the task's screen shows when its program could not start.
fn spawn_error_message(spec: &ProcessSpec, err: &anyhow::Error) -> String {
  let mut message = format!(
    "\x1b[31mCannot start `{}`: {}\x1b[0m\r\n",
    spec.prog,
    err.root_cause()
  );
  #[cfg(windows)]
  let not_found = err
    .root_cause()
    .downcast_ref::<std::io::Error>()
    .is_some_and(|err| err.kind() == std::io::ErrorKind::NotFound);
  #[cfg(windows)]
  if not_found && let Some(script) = batch_file_on_path(spec) {
    let line = std::iter::once(&spec.prog)
      .chain(&spec.args)
      .map(String::as_str)
      .collect::<Vec<_>>()
      .join(" ");
    message.push_str(&format!(
      "`{}` is {}, a batch file; Windows runs those only through cmd:\r\n  cmd: [\"cmd\", \"/c\", \"{}\"]\r\n",
      spec.prog,
      script.display(),
      line.replace('\\', "\\\\").replace('"', "\\\""),
    ));
  }
  message
}

/// `npm` and the like are `npm.cmd` on Windows, which CreateProcess does
/// not find or run by the bare name.
#[cfg(windows)]
fn batch_file_on_path(spec: &ProcessSpec) -> Option<std::path::PathBuf> {
  let prog = std::path::Path::new(&spec.prog);
  if prog.extension().is_some() || prog.components().count() != 1 {
    return None;
  }
  let path = spec
    .env
    .iter()
    .find(|(key, _)| key.eq_ignore_ascii_case("PATH"))
    .map_or_else(
      || std::env::var_os("PATH"),
      |(_, value)| value.as_ref().map(Into::into),
    )?;
  std::env::split_paths(&path).find_map(|dir| {
    ["cmd", "bat"]
      .iter()
      .map(|ext| dir.join(prog).with_extension(ext))
      .find(|file| file.is_file())
  })
}

async fn apply_effects(
  effects: &mut Vec<TaskScreenEffect>,
  process: &mut Option<NativeProcess>,
  vt: &SharedVt,
  key_buf: &mut Vec<u8>,
) {
  for effect in effects.drain(..) {
    match effect {
      TaskScreenEffect::Write(s) => {
        if let Some(p) = process.as_mut() {
          p.write_all(&s).await.log_ignore();
        }
      }
      TaskScreenEffect::Key(key) => {
        if let Some(p) = process.as_mut() {
          send_key(p, vt, key, key_buf).await;
        }
      }
      TaskScreenEffect::Paste(text) => {
        if let Some(p) = process.as_mut() {
          let bracketed = vt
            .read()
            .map(|screen| screen.bracketed_paste())
            .unwrap_or(false);
          key_buf.clear();
          emit::paste(key_buf, &text, bracketed);
          p.write_all(key_buf).await.log_ignore();
        }
      }
      TaskScreenEffect::Resize(size) => {
        if let Some(p) = process.as_mut() {
          p.resize(size).log_ignore();
        }
      }
    }
  }
}

async fn send_key(
  process: &mut NativeProcess,
  vt: &SharedVt,
  key: Key,
  buf: &mut Vec<u8>,
) {
  // CSI-u only once the program asked for the supported kitty
  // "disambiguate escape codes" flag.
  let (csi_u, application_cursor_keys) = vt
    .read()
    .map(|s| (s.kitty_flags() != 0, s.application_cursor()))
    .unwrap_or((false, false));
  let modes = KeyEncodeModes {
    enable_csi_u_key_encoding: csi_u,
    application_cursor_keys,
    newline_mode: false,
  };
  buf.clear();
  emit::key(buf, &key, modes);
  if !buf.is_empty() {
    process.write_all(buf).await.log_ignore();
  }
}

#[cfg(not(windows))]
async fn stop_process(
  process: &mut NativeProcess,
  stop: &StopSignal,
  vt: &SharedVt,
  spec: &ProcessSpec,
) {
  match stop {
    StopSignal::Signal { sig, group } => {
      process.send_signal(sig.to_libc(), *group).log_ignore();
    }
    StopSignal::Keys(keys) => {
      let mut buf = Vec::new();
      for key in keys {
        send_key(process, vt, key.clone(), &mut buf).await;
      }
    }
    StopSignal::Cmd(argv) => run_stop_cmd(spec, argv),
  }
}

#[cfg(windows)]
async fn stop_process(
  process: &mut NativeProcess,
  stop: &StopSignal,
  vt: &SharedVt,
  spec: &ProcessSpec,
) {
  match stop {
    // Windows has no real signals: INT/TERM/KILL fall back to terminating the
    // process; everything else has no equivalent and is ignored. TODO: a
    // Ctrl-C through the ConPTY for INT/TERM, and the whole tree via a Job
    // Object.
    StopSignal::Signal { sig, .. } => match sig {
      Sig::Int | Sig::Term | Sig::Kill => process.kill(true).await.log_ignore(),
      _ => log::debug!("{sig:?} has no Windows equivalent; ignoring"),
    },
    StopSignal::Keys(keys) => {
      let mut buf = Vec::new();
      for key in keys {
        send_key(process, vt, key.clone(), &mut buf).await;
      }
    }
    StopSignal::Cmd(argv) => run_stop_cmd(spec, argv),
  }
}

fn run_stop_cmd(spec: &ProcessSpec, argv: &[String]) {
  #[cfg(unix)]
  {
    let cmd = format!("{argv:?}");
    let spawned = crate::process::unix_process::spawn_command(
      argv,
      spec,
      Box::new(move |info| match info.code {
        Some(127) => log::warn!(
          "Stop command {cmd} exited with 127 (not found or not executable)"
        ),
        _ => log::debug!("Stop command exited: {info}"),
      }),
    );
    if let Err(err) = spawned {
      log::warn!("Stop command {argv:?} cannot run: {err}");
    }
  }
  #[cfg(windows)]
  {
    let Some(cmd) = command(argv, spec) else {
      return;
    };
    tokio::spawn(async move {
      if let Err(err) = tokio::process::Command::from(cmd).status().await {
        log::warn!("Stop command failed: {err}");
      }
    });
  }
}

#[cfg(not(windows))]
#[cfg(test)]
mod tests {
  use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

  use crate::config::task_log::{LogMode, TaskLogConfig};
  use crate::kernel::kernel::Kernel;
  use crate::kernel::kernel_message::{
    KernelCommand, KernelQuery, KernelQueryResponse, SpaceSelector, TaskContext,
  };
  use crate::kernel::task::{TaskId, TaskState};

  use super::*;

  fn spawn_process_task(
    parent: &TaskContext,
    key: Option<TaskKey>,
    config: ProcessTaskConfig,
  ) -> (
    TaskId,
    tokio::sync::oneshot::Receiver<
      Result<(), crate::kernel::kernel_message::RegisterError>,
    >,
  ) {
    let task_id = parent.alloc_id();
    let ack =
      parent.register_task(process_task_registration(task_id, key, config));
    (task_id, ack)
  }

  async fn resolve(pc: &TaskContext, path: &str) -> TaskId {
    let (tx, rx) = tokio::sync::oneshot::channel();
    pc.send(KernelCommand::Query(
      KernelQuery::ListTasks(TaskSelector::Glob(
        SpaceSelector::default_space(),
        path.to_string(),
      )),
      tx,
    ));
    let resp = tokio::time::timeout(Duration::from_secs(1), rx)
      .await
      .expect("timed out resolving path")
      .expect("kernel query channel closed");
    match resp {
      KernelQueryResponse::TaskList(tasks) if tasks.len() == 1 => tasks[0].id,
      _ => panic!("path did not resolve: {path}"),
    }
  }

  #[tokio::test]
  async fn proc_output_is_logged_via_direct_observer() {
    let nanos = SystemTime::now()
      .duration_since(UNIX_EPOCH)
      .unwrap()
      .as_nanos();
    let mut log_path = std::env::temp_dir();
    log_path.push(format!("dekit_log_{}_{}.log", std::process::id(), nanos));

    let kernel = Kernel::new();
    let pc = kernel.context();

    let path = TaskKey::default_space(TaskPath::new("logged").unwrap());
    let spec = ProcessSpec::from_argv(vec![
      "sh".to_string(),
      "-c".to_string(),
      "printf hello-log".to_string(),
    ]);
    let sink_path = log_path.clone();
    let (id, _) = spawn_process_task(
      &pc,
      Some(path),
      ProcessTaskConfig {
        log: Some(LogSpec {
          config: TaskLogConfig {
            enabled: Some(true),
            dir: None,
            file: Some(sink_path),
            mode: Some(LogMode::Truncate),
          },
          name: "logged".to_string(),
        }),
        ..ProcessTaskConfig::new(spec)
      },
    );
    pc.send(KernelCommand::Start(TaskSelector::Id(id), None));

    let kernel_task = tokio::spawn(kernel.run());

    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
      if let Ok(contents) = std::fs::read_to_string(&log_path) {
        if contents.contains("hello-log") {
          break;
        }
      }
      assert!(Instant::now() < deadline, "log file never got output");
      tokio::time::sleep(Duration::from_millis(10)).await;
    }

    // The SIGCHLD waiter isn't running in unit tests, so the task never
    // transitions to Exited on its own; remove it explicitly to unblock quit.
    let id = resolve(&pc, "logged").await;
    pc.send(KernelCommand::Remove(TaskSelector::Id(id), None));
    pc.send(KernelCommand::Quit);
    tokio::time::timeout(Duration::from_secs(2), kernel_task)
      .await
      .expect("timed out waiting for kernel to quit")
      .unwrap();

    let _ = std::fs::remove_file(&log_path);
  }

  #[tokio::test]
  async fn log_path_is_resolved_with_real_pid() {
    let nanos = SystemTime::now()
      .duration_since(UNIX_EPOCH)
      .unwrap()
      .as_nanos();
    let mut dir = std::env::temp_dir();
    dir.push(format!("dekit_pidlog_{}_{}", std::process::id(), nanos));
    std::fs::create_dir_all(&dir).unwrap();

    let kernel = Kernel::new();
    let pc = kernel.context();

    let spec = ProcessSpec::from_argv(vec![
      "sh".to_string(),
      "-c".to_string(),
      "printf hi".to_string(),
    ]);
    let (id, _) = spawn_process_task(
      &pc,
      Some(TaskKey::default_space(TaskPath::new("pidlog").unwrap())),
      ProcessTaskConfig {
        log: Some(LogSpec {
          config: TaskLogConfig {
            enabled: Some(true),
            dir: Some(dir.clone()),
            file: Some(std::path::PathBuf::from("{pid}.log")),
            mode: Some(LogMode::Truncate),
          },
          name: "pidlog".to_string(),
        }),
        ..ProcessTaskConfig::new(spec)
      },
    );
    pc.send(KernelCommand::Start(TaskSelector::Id(id), None));

    let kernel_task = tokio::spawn(kernel.run());

    let deadline = Instant::now() + Duration::from_secs(2);
    let pid = loop {
      let found = std::fs::read_dir(&dir)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.ok())
        .find(|entry| {
          std::fs::read_to_string(entry.path()).is_ok_and(|c| c.contains("hi"))
        })
        .and_then(|entry| {
          entry
            .path()
            .file_stem()
            .and_then(|stem| stem.to_str()?.parse::<u32>().ok())
        });
      if let Some(pid) = found {
        break pid;
      }
      assert!(Instant::now() < deadline, "pid-named log never got output");
      tokio::time::sleep(Duration::from_millis(10)).await;
    };
    assert_ne!(pid, 0, "log file should be named after a real pid");

    let id = resolve(&pc, "pidlog").await;
    pc.send(KernelCommand::Remove(TaskSelector::Id(id), None));
    pc.send(KernelCommand::Quit);
    tokio::time::timeout(Duration::from_secs(2), kernel_task)
      .await
      .expect("timed out waiting for kernel to quit")
      .unwrap();

    let _ = std::fs::remove_dir_all(&dir);
  }

  #[tokio::test]
  async fn stop_cmd_runs_in_the_task_dir() {
    let nanos = SystemTime::now()
      .duration_since(UNIX_EPOCH)
      .unwrap()
      .as_nanos();
    let mut dir = std::env::temp_dir();
    dir.push(format!("dekit_stopcmd_{}_{}", std::process::id(), nanos));
    std::fs::create_dir_all(&dir).unwrap();

    let kernel = Kernel::new();
    let pc = kernel.context();

    let stops = [
      (
        "by_cmd",
        StopSignal::Cmd(vec!["touch".to_string(), "by_cmd".to_string()]),
      ),
      (
        "by_sh",
        StopSignal::Cmd(vec![
          "sh".to_string(),
          "-c".to_string(),
          "printf done > by_sh".to_string(),
        ]),
      ),
    ];
    for (name, stop) in stops {
      let mut spec =
        ProcessSpec::from_argv(vec!["sleep".to_string(), "100".to_string()]);
      spec.cwd(dir.to_string_lossy());
      let (id, _) = spawn_process_task(
        &pc,
        Some(TaskKey::default_space(TaskPath::new(name).unwrap())),
        ProcessTaskConfig {
          stop,
          ..ProcessTaskConfig::new(spec)
        },
      );
      pc.send(KernelCommand::Start(TaskSelector::Id(id), None));
    }

    let kernel_task = tokio::spawn(kernel.run());

    let mut ids = Vec::new();
    for name in ["by_cmd", "by_sh"] {
      let id = resolve(&pc, name).await;
      pc.send(KernelCommand::Stop(TaskSelector::Id(id), None));
      ids.push(id);
    }

    let deadline = Instant::now() + Duration::from_secs(2);
    while !(dir.join("by_cmd").exists() && dir.join("by_sh").exists()) {
      assert!(Instant::now() < deadline, "a stop command never ran");
      tokio::time::sleep(Duration::from_millis(10)).await;
    }

    for id in ids {
      pc.send(KernelCommand::Kill(TaskSelector::Id(id), None));
      pc.send(KernelCommand::Remove(TaskSelector::Id(id), None));
    }
    pc.send(KernelCommand::Quit);
    tokio::time::timeout(Duration::from_secs(2), kernel_task)
      .await
      .expect("timed out waiting for kernel to quit")
      .unwrap();

    let _ = std::fs::remove_dir_all(&dir);
  }

  #[test]
  fn snapshot_config_round_trips_ready_and_timeouts() {
    let config = ProcessTaskConfig {
      kind: TaskKind::Service,
      stop_timeout: Duration::from_secs(3),
      ready: Some(ReadyConfig {
        check: ReadyCheck::Probe {
          probe: Probe::Tcp {
            host: None,
            port: 5432,
          },
          interval: Duration::from_millis(250),
        },
        timeout: Some(Duration::from_secs(30)),
      }),
      ..ProcessTaskConfig::new(ProcessSpec::from_argv(vec!["true".into()]))
    };
    let screen = TaskScreen::new(
      TaskId(1),
      SharedVt::new(Screen::new(DEFAULT_SIZE, 0)),
      config.mouse_scroll_speed,
    );
    let process = snapshot(&config, None, &screen, &Instance::default());
    assert_eq!(process.ready_log, None);
    let saved = snap::Task {
      id: 1,
      space: String::new(),
      path: Some("db".to_string()),
      label: None,
      tags: Vec::new(),
      pinned: true,
      deps: Vec::new(),
      restart: snap::Restart::Never,
      job: false,
      ready_timeout_ms: Some(30_000),
      stop_timeout_ms: Some(3_000),
      state: snap::TaskState::Idle {},
      vetoed: false,
      killed: false,
      start_failed: false,
      saved_pin: false,
      attempts: 0,
      last_start_secs_ago: None,
      timer_ms: None,
      kind: snap::TaskKind::Process(process.clone()),
    };
    let back =
      process_task_config_from_snapshot(&saved, &process, Vec::new()).unwrap();
    assert_eq!(back.stop_timeout, Duration::from_secs(3));
    let ready = back.ready.unwrap();
    assert_eq!(ready.timeout, Some(Duration::from_secs(30)));
    match ready.check {
      ReadyCheck::Probe {
        probe: Probe::Tcp { host: None, port },
        interval,
      } => {
        assert_eq!(port, 5432);
        assert_eq!(interval, Duration::from_millis(250));
      }
      other => panic!("{other:?}"),
    }

    // An older snapshot's `ready_log` is the log check.
    let mut old = process.clone();
    old.ready_probe = None;
    old.ready_log = Some("listening".to_string());
    let back =
      process_task_config_from_snapshot(&saved, &old, Vec::new()).unwrap();
    match back.ready.unwrap().check {
      ReadyCheck::Log(text) => assert_eq!(text, "listening"),
      other => panic!("{other:?}"),
    }

    for stop in [
      StopSignal::Signal {
        sig: Sig::Int,
        group: false,
      },
      StopSignal::Keys(vec![Key::parse("<C-c>").unwrap()]),
      StopSignal::Cmd(vec!["podman".to_string(), "stop".to_string()]),
    ] {
      let config = ProcessTaskConfig {
        stop: stop.clone(),
        ..ProcessTaskConfig::new(ProcessSpec::from_argv(vec!["true".into()]))
      };
      let process = snapshot(&config, None, &screen, &Instance::default());
      let back =
        process_task_config_from_snapshot(&saved, &process, Vec::new())
          .unwrap();
      assert_eq!(format!("{:?}", back.stop), format!("{stop:?}"));
    }

    // An older binary's `cmd` line was meant for the system shell and
    // keeps running there; its `shutdown` and `kill` are SIGTERM and
    // SIGKILL to the group.
    let config = ProcessTaskConfig {
      stop: StopSignal::Cmd(vec!["podman".to_string(), "stop".to_string()]),
      ..ProcessTaskConfig::new(ProcessSpec::from_argv(vec!["true".into()]))
    };
    let mut old = snapshot(&config, None, &screen, &Instance::default());
    old.stop = snap::StopSignal::Cmd {
      cmd: "kill $(cat pid)".to_string(),
    };
    let back =
      process_task_config_from_snapshot(&saved, &old, Vec::new()).unwrap();
    match back.stop {
      StopSignal::Cmd(argv) => {
        assert_eq!(argv, crate::parse_shell::system_argv("kill $(cat pid)"))
      }
      other => panic!("{other:?}"),
    }
    old.stop = snap::StopSignal::Shutdown {};
    let back =
      process_task_config_from_snapshot(&saved, &old, Vec::new()).unwrap();
    match back.stop {
      StopSignal::Signal {
        sig: Sig::Term,
        group: true,
      } => (),
      other => panic!("{other:?}"),
    }
    old.stop = snap::StopSignal::Kill {};
    let back =
      process_task_config_from_snapshot(&saved, &old, Vec::new()).unwrap();
    match back.stop {
      StopSignal::Signal {
        sig: Sig::Kill,
        group: true,
      } => (),
      other => panic!("{other:?}"),
    }
  }

  #[cfg(unix)]
  #[tokio::test]
  async fn failed_adopt_reports_the_task_stopped() {
    let config =
      ProcessTaskConfig::new(ProcessSpec::from_argv(vec!["true".to_string()]));
    let screen = TaskScreen::new(
      TaskId(1),
      SharedVt::new(Screen::new(DEFAULT_SIZE, 0)),
      config.mouse_scroll_speed,
    );
    let mut process = snapshot(&config, None, &screen, &Instance::default());
    // Pid 0 cannot be adopted.
    process.instance = Some(snap::Instance {
      pid: 0,
      master_fd: -1,
      exit: None,
      stdout_eof: false,
      ready_sent: false,
      stop_sent: false,
      ready_line: String::new(),
      log_path: None,
    });
    let saved = snap::Task {
      id: 1,
      space: String::new(),
      path: Some("adopted".to_string()),
      label: None,
      tags: Vec::new(),
      pinned: true,
      deps: Vec::new(),
      restart: snap::Restart::Never,
      job: false,
      ready_timeout_ms: None,
      stop_timeout_ms: None,
      state: snap::TaskState::Running {},
      vetoed: false,
      killed: false,
      start_failed: false,
      saved_pin: false,
      attempts: 1,
      last_start_secs_ago: None,
      timer_ms: None,
      kind: snap::TaskKind::Process(process.clone()),
    };
    let key = TaskKey::default_space(TaskPath::new("adopted").unwrap());
    let registration =
      process_task_from_snapshot(TaskId(1), Some(key), &saved, &process)
        .unwrap();

    let mut kernel = Kernel::new();
    let pc = kernel.context();
    kernel
      .restore(2, vec![(Some(&saved), registration)])
      .unwrap();
    let kernel_task = tokio::spawn(kernel.run());

    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
      let (tx, rx) = tokio::sync::oneshot::channel();
      pc.send(KernelCommand::Query(
        KernelQuery::ListTasks(TaskSelector::Id(TaskId(1))),
        tx,
      ));
      let active = match rx.await.unwrap() {
        KernelQueryResponse::TaskList(tasks) => tasks[0].state.is_active(),
        KernelQueryResponse::Explain(_) => unreachable!(),
      };
      if !active {
        break;
      }
      assert!(Instant::now() < deadline, "task stayed active");
      tokio::time::sleep(Duration::from_millis(10)).await;
    }

    pc.send(KernelCommand::Quit);
    tokio::time::timeout(Duration::from_secs(2), kernel_task)
      .await
      .expect("timed out waiting for kernel to quit")
      .unwrap();
  }

  async fn state_of(pc: &TaskContext, id: TaskId) -> TaskState {
    match pc.query(KernelQuery::ListTasks(TaskSelector::Id(id))).await {
      Ok(KernelQueryResponse::TaskList(tasks)) => tasks[0].state,
      other => panic!("{:?}", other.is_ok()),
    }
  }

  #[tokio::test]
  async fn a_panicking_task_is_reported_stopped() {
    let kernel = Kernel::new();
    let pc = kernel.context();
    let id = pc.alloc_id();
    let _ack = pc.register_task(TaskRegistration::async_task(
      id,
      TaskDef::default(),
      |_ctx, mut receiver| async move {
        let _ = receiver.recv().await;
        panic!("a bug in the task");
      },
    ));
    pc.send(KernelCommand::Start(TaskSelector::Id(id), None));
    let kernel_task = tokio::spawn(kernel.run());

    let deadline = Instant::now() + Duration::from_secs(2);
    while state_of(&pc, id).await.is_active() {
      assert!(Instant::now() < deadline, "task stayed active");
      tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
      state_of(&pc, id).await,
      TaskState::Exited(ExitInfo::error())
    );
    kernel_task.abort();
  }

  /// A restored task runs its ready check only in `Running`, where the
  /// kernel waits for it; a `Ready` one stays ready without it. An older
  /// binary's saved line is checked when the task resumes.
  #[tokio::test]
  async fn a_resumed_task_checks_ready_only_while_running() {
    let screen = TaskScreen::new(
      TaskId(1),
      SharedVt::new(Screen::new(DEFAULT_SIZE, 0)),
      5,
    );
    let listeners: Vec<std::net::TcpListener> = (0..2)
      .map(|_| std::net::TcpListener::bind("127.0.0.1:0").unwrap())
      .collect();
    let probe = |i: usize| ReadyCheck::Probe {
      probe: Probe::Tcp {
        host: Some("127.0.0.1".to_string()),
        port: listeners[i].local_addr().unwrap().port(),
      },
      interval: Duration::from_millis(10),
    };

    let cases = [
      ("was_ready", snap::TaskState::Ready {}, probe(0)),
      ("running", snap::TaskState::Running {}, probe(1)),
      // Old binaries ended lines only at `\n`.
      (
        "logged",
        snap::TaskState::Running {},
        ReadyCheck::Log("listening".to_string()),
      ),
    ];
    let mut children = Vec::new();
    let mut tasks = Vec::new();
    for (i, (name, state, check)) in cases.into_iter().enumerate() {
      let config = ProcessTaskConfig {
        ready: Some(ReadyConfig {
          check,
          timeout: None,
        }),
        ..ProcessTaskConfig::new(ProcessSpec::from_argv(vec!["true".into()]))
      };
      // A live child, and a pipe standing in for its PTY.
      let child = std::process::Command::new("sleep")
        .arg("60")
        .spawn()
        .unwrap();
      let mut fds = [0; 2];
      unsafe {
        assert_eq!(libc::pipe(fds.as_mut_ptr()), 0);
        libc::fcntl(fds[0], libc::F_SETFL, libc::O_NONBLOCK);
      }
      let mut process = snapshot(&config, None, &screen, &Instance::default());
      process.instance = Some(snap::Instance {
        pid: child.id(),
        master_fd: fds[0],
        exit: None,
        stdout_eof: false,
        ready_sent: false,
        stop_sent: false,
        ready_line: snap::to_base64(b"listening\r"),
        log_path: None,
      });
      children.push((child, fds[1]));
      tasks.push(snap::Task {
        id: i + 1,
        space: String::new(),
        path: Some(name.to_string()),
        label: None,
        tags: Vec::new(),
        pinned: true,
        deps: Vec::new(),
        restart: snap::Restart::Never,
        job: false,
        ready_timeout_ms: None,
        stop_timeout_ms: None,
        state,
        vetoed: false,
        killed: false,
        start_failed: false,
        saved_pin: false,
        attempts: 1,
        last_start_secs_ago: None,
        timer_ms: None,
        kind: snap::TaskKind::Process(process),
      });
    }
    let mut restored = Vec::new();
    for saved in &tasks {
      let snap::TaskKind::Process(process) = &saved.kind else {
        unreachable!()
      };
      let path = TaskPath::new(saved.path.clone().unwrap()).unwrap();
      let registration = process_task_from_snapshot(
        TaskId(saved.id),
        Some(TaskKey::default_space(path)),
        saved,
        process,
      )
      .unwrap();
      restored.push((Some(saved), registration));
    }
    let mut kernel = Kernel::new();
    let pc = kernel.context();
    kernel.restore(tasks.len() + 1, restored).unwrap();
    let kernel_task = tokio::spawn(kernel.run());

    let deadline = Instant::now() + Duration::from_secs(2);
    for id in [TaskId(2), TaskId(3)] {
      while state_of(&pc, id).await != TaskState::Ready {
        assert!(Instant::now() < deadline, "task {id:?} never got ready");
        tokio::time::sleep(Duration::from_millis(10)).await;
      }
    }
    // Many intervals, and still no check for the task that was ready.
    tokio::time::sleep(Duration::from_millis(200)).await;
    listeners[0].set_nonblocking(true).unwrap();
    assert!(listeners[0].accept().is_err(), "the ready task was checked");
    assert_eq!(state_of(&pc, TaskId(1)).await, TaskState::Ready);

    kernel_task.abort();
    for (mut child, fd) in children {
      let _ = child.kill();
      let _ = child.wait();
      unsafe { libc::close(fd) };
    }
  }
}

fn spawn_native(
  ctx: &TaskContext,
  spec: &ProcessSpec,
  size: Winsize,
) -> anyhow::Result<(NativeProcess, UnboundedReceiver<ExitInfo>)> {
  let (exit_sender, exits) = unbounded_channel();

  #[cfg(unix)]
  {
    let process = crate::process::unix_process::UnixProcess::spawn(
      ctx.task_id,
      spec,
      size,
      Box::new(move |info| {
        let _ = exit_sender.send(info);
      }),
    )?;
    Ok((process, exits))
  }

  #[cfg(windows)]
  {
    use anyhow::Context as _;
    let process = crate::process::win_process::WinProcess::spawn(
      ctx.task_id,
      spec,
      size,
      Box::new(move |exit_code| {
        let info = match exit_code {
          Some(code) => ExitInfo::code(code as i32),
          None => ExitInfo::error(),
        };
        let _ = exit_sender.send(info);
      }),
    )
    .context("WinProcess::spawn")?;
    Ok((process, exits))
  }
}

#[cfg(unix)]
fn adopt_native(
  saved: &snap::Instance,
) -> std::io::Result<(NativeProcess, Option<UnboundedReceiver<ExitInfo>>)> {
  let process = crate::process::unix_process::UnixProcess::adopt(
    saved.pid,
    saved.master_fd,
  )?;
  // The previous image already collected this exit, freeing the pid for
  // reuse: waiting on it could catch an unrelated child.
  if saved.exit.is_some() {
    return Ok((process, None));
  }
  let (exit_sender, exits) = unbounded_channel();
  crate::process::unix_processes_waiter::UnixProcessesWaiter::wait_for(
    process.pid,
    0,
    Box::new(move |info| {
      let _ = exit_sender.send(info);
    }),
  );
  Ok((process, Some(exits)))
}

#[cfg(windows)]
#[cfg(test)]
mod win_tests {
  use super::*;

  #[test]
  fn spawn_error_names_a_batch_file_and_the_cmd_to_use() {
    let dir = std::env::temp_dir()
      .join(format!("dekit_batch_hint_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("fake-npm.cmd"), "@echo off\r\n").unwrap();
    let mut spec = ProcessSpec::from_argv(vec![
      "fake-npm".into(),
      "run".into(),
      "a \"b\"".into(),
    ]);
    spec
      .env
      .insert("Path".into(), Some(dir.to_string_lossy().into_owned()));
    let not_found =
      anyhow::Error::from(std::io::Error::from(std::io::ErrorKind::NotFound));

    let message = spawn_error_message(&spec, &not_found);
    assert!(message.contains("Cannot start `fake-npm`"), "{message}");
    assert!(message.contains("fake-npm.cmd, a batch file"), "{message}");
    assert!(
      message.contains(r#"cmd: ["cmd", "/c", "fake-npm run a \"b\""]"#),
      "{message}"
    );

    // Another failure is not about the batch file.
    let denied = anyhow::Error::from(std::io::Error::from(
      std::io::ErrorKind::PermissionDenied,
    ));
    assert!(!spawn_error_message(&spec, &denied).contains("batch file"));

    std::fs::remove_dir_all(&dir).unwrap();
  }
}
