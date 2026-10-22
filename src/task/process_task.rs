use std::borrow::Cow;
use std::future::{Future, pending};
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

use crate::config::config::Config;
use crate::config::task::{AUTOSTART_TAG, CmdConfig, TaskConfig};
use crate::error::ResultLogger;
use crate::kernel::kernel_message::{
  KernelCommand, TaskContext, TaskRegistration, TaskSelector, TaskUpdate,
};
use crate::kernel::task::{
  ExitInfo, NextRun, ReadyMode, RestartMode, Run, STOP_TIMEOUT, TaskCmd,
  TaskDef, TaskId,
};
use crate::kernel::task_key::TaskKey;
use crate::kernel::task_path::TaskPath;
use crate::kernel::task_screen::{
  DEFAULT_SIZE, TaskScreen, TaskScreenCmd, TaskScreenEffect,
};
use crate::process::NativeProcess;
use crate::process::process::Process as _;
use crate::process::process_spec::ProcessSpec;
use crate::runner::RunnerSpec;
#[cfg(unix)]
use crate::runner::datagram::DatagramSocket;
use crate::spec::TaskSpec;
use crate::task::config_tasks::project_dir;
#[cfg(unix)]
use crate::task::logger::LogSink;
use crate::task::logger::{LogSpec, spawn_logger};
#[cfg(windows)]
use crate::task::ready::command;
#[cfg(unix)]
use crate::task::ready::wait_sd_notify;
use crate::task::ready::{
  Probe, ReadyCheck, ReadyConfig, line_ready, wait_ready,
};
use crate::term::key::Key;
use crate::term::vt::emit::{self, KeyEncodeModes};
use crate::term::{Screen, ScreenHandle, Winsize};
use crate::upgrade::snapshot as snap;

/// Windows has no Unix datagram sockets: the config refuses the check
/// that would need one.
#[cfg(windows)]
#[allow(dead_code)]
struct DatagramSocket;

/// An OS signal a `Signal` stop can deliver. The name table and the libc
/// mapping are generated from one list so they can't drift. On Windows only
/// INT/TERM/KILL have a (terminate) fallback; every other signal is ignored.
macro_rules! signals {
  ($($name:literal => $variant:ident => $libc:ident,)+) => {
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
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

#[derive(Clone, Debug, Eq, PartialEq)]
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

/// What a definition is read with: the project's directory, which
/// relative paths are from, and the runner, which script tasks talk to.
#[derive(Clone, Debug)]
pub struct RunCx {
  pub dir: PathBuf,
  pub runner: Option<RunnerSpec>,
}

impl RunCx {
  pub fn of(config: &Config) -> Arc<RunCx> {
    Arc::new(RunCx {
      dir: project_dir(config),
      runner: config.runner.clone(),
    })
  }

  /// Reads a definition as the task at `path` runs it, and as the kernel
  /// keeps it: in one form, so equal definitions compare equal.
  fn read(
    &self,
    path: &str,
    spec: &TaskSpec,
  ) -> anyhow::Result<(TaskConfig, TaskSpec)> {
    let cfg = spec.parse(path, &self.dir)?;
    match &cfg.cmd {
      Some(CmdConfig::Script { .. }) if self.runner.is_none() => {
        anyhow::bail!("script command has no runner identity")
      }
      Some(CmdConfig::Cmd { .. } | CmdConfig::Script { .. }) => {
        let spec = TaskSpec::from_config(&cfg);
        Ok((cfg, spec))
      }
      None => anyhow::bail!("expected 'cmd' or 'script'"),
    }
  }
}

/// What one run of a process task is: read from its definition when it
/// starts, and kept until the next start.
#[derive(Clone)]
pub struct ProcessRun {
  pub spec: ProcessSpec,
  pub stop: StopSignal,
  pub log: Option<LogSpec>,
  /// Without it the run is ready as soon as it starts.
  pub ready: Option<ReadyConfig>,
  pub scrollback_len: usize,
  pub mouse_scroll_speed: usize,
}

impl ProcessRun {
  fn of(cx: &RunCx, cfg: &TaskConfig) -> ProcessRun {
    ProcessRun {
      spec: crate::config::task::process_spec(cfg, cx.runner.as_ref()),
      stop: cfg
        .stop
        .as_ref()
        .and_then(|stop| stop.signal.clone())
        .unwrap_or_default(),
      log: cfg.log.clone().map(|log| LogSpec {
        config: log,
        name: cfg.path.clone(),
      }),
      ready: cfg.ready.clone(),
      scrollback_len: cfg.scrollback_len(),
      mouse_scroll_speed: cfg.mouse_scroll_speed(),
    }
  }

  #[cfg(test)]
  pub fn new(spec: ProcessSpec) -> Self {
    Self {
      spec,
      stop: StopSignal::default(),
      log: None,
      ready: None,
      scrollback_len: 1000,
      mouse_scroll_speed: 5,
    }
  }
}

fn ready_mode(cfg: &TaskConfig) -> ReadyMode {
  match &cfg.ready {
    Some(ready) => ReadyMode::Reported {
      timeout: ready.timeout,
    },
    None => ReadyMode::Immediate,
  }
}

fn stop_timeout(cfg: &TaskConfig) -> Duration {
  cfg
    .stop
    .as_ref()
    .and_then(|stop| stop.timeout)
    .unwrap_or(STOP_TIMEOUT)
}

/// Its own tags, and `autostart` for one that autostarts.
fn tags(cfg: &TaskConfig) -> Vec<String> {
  let mut tags = cfg.tags.clone();
  if cfg.autostart() {
    tags.push(AUTOSTART_TAG.to_string());
  }
  tags
}

/// A process task made from a definition as written (`defaults`
/// already in it): what the kernel keeps of it, around a fresh screen.
/// Its deps, pin, origin and place are the caller's to set.
pub fn process_task(
  cx: &Arc<RunCx>,
  task_id: TaskId,
  key: Option<TaskKey>,
  spec: TaskSpec,
) -> anyhow::Result<TaskRegistration> {
  let path = key
    .as_ref()
    .map_or(String::new(), |key| key.path.to_string());
  let (cfg, spec) = cx.read(&path, &spec)?;
  let screen = Screen::new(DEFAULT_SIZE, cfg.scrollback_len());
  Ok(registration(
    cx, task_id, key, &cfg, spec, None, screen, None, None,
  ))
}

/// A process task around a saved screen, its sd_notify socket if it has one,
/// and, if the child is still this runner's, its inherited PTY.
/// `run_spec` is what the run under way started from, when the task was
/// redefined since.
#[allow(clippy::too_many_arguments)]
pub fn process_task_resumed(
  cx: &Arc<RunCx>,
  task_id: TaskId,
  key: Option<TaskKey>,
  spec: TaskSpec,
  run_spec: Option<TaskSpec>,
  screen: &snap::Screen,
  instance: Option<snap::Instance>,
  sd_notify: Option<snap::SdNotifySocket>,
) -> anyhow::Result<TaskRegistration> {
  let path = key
    .as_ref()
    .map_or(String::new(), |key| key.path.to_string());
  let (cfg, spec) = cx.read(&path, &spec)?;
  let run = run_spec
    .map(|run_spec| cx.read(&path, &run_spec))
    .transpose()?;
  let screen = Screen::from_snapshot(screen)?;
  Ok(registration(
    cx, task_id, key, &cfg, spec, run, screen, instance, sd_notify,
  ))
}

/// What runs of `cfg`, written as `spec`, start from.
fn next_run(cfg: &TaskConfig, spec: TaskSpec) -> NextRun {
  NextRun {
    kind: cfg.kind,
    ready: ready_mode(cfg),
    stop_timeout: stop_timeout(cfg),
    spec: Some(spec),
  }
}

/// A task's new definition as `KernelCommand::Update` takes it, unless
/// it is no longer `was`; restarted now if `restart_running`.
pub fn process_task_update(
  cx: &RunCx,
  path: &TaskPath,
  spec: TaskSpec,
  deps: Vec<TaskSelector>,
  was: Option<TaskSpec>,
  restart_running: bool,
) -> anyhow::Result<TaskUpdate> {
  let (cfg, spec) = cx.read(path.as_str(), &spec)?;
  // A `was` in another form is the same definition still.
  let was =
    was.map(|was| cx.read(path.as_str(), &was).map_or(was, |(_, was)| was));
  Ok(TaskUpdate {
    restart: cfg.autorestart.unwrap_or(RestartMode::Never),
    deps,
    tags: tags(&cfg),
    label: cfg.label.clone(),
    next: next_run(&cfg, spec),
    was,
    restart_running,
  })
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

/// `cfg` and `spec` are the task's definition; `run`, what the run
/// under way started from when that is older.
#[allow(clippy::too_many_arguments)]
fn registration(
  cx: &Arc<RunCx>,
  task_id: TaskId,
  key: Option<TaskKey>,
  cfg: &TaskConfig,
  spec: TaskSpec,
  run: Option<(TaskConfig, TaskSpec)>,
  screen: Screen,
  instance: Option<snap::Instance>,
  sd_notify: Option<snap::SdNotifySocket>,
) -> TaskRegistration {
  let handle = ScreenHandle::new(screen.frame());
  let task_handle = handle.clone();
  let (space, path) = match key.clone() {
    Some(key) => (key.space, Some(key.path)),
    None => (Default::default(), None),
  };
  // The kernel's run fields are the run's; the next run is the task's.
  let (run_cfg, run_spec, next) = match run {
    Some((run_cfg, run_spec)) => {
      (run_cfg, run_spec, Some(Box::new(next_run(cfg, spec))))
    }
    None => (cfg.clone(), spec, None),
  };
  let process_run = ProcessRun::of(cx, &run_cfg);
  let copier = cx.clone();
  TaskRegistration::async_task(
    task_id,
    TaskDef {
      kind: run_cfg.kind,
      ready: ready_mode(&run_cfg),
      restart: cfg.autorestart.unwrap_or(RestartMode::Never),
      stop_timeout: stop_timeout(&run_cfg),
      deps: Vec::new(),
      space,
      path,
      label: cfg.label.clone(),
      place: Default::default(),
      vt: Some(handle),
      tags: tags(cfg),
      pinned: false,
      saved_pin: false,
      spec: Some(run_spec),
      origin: None,
      next,
      make_copy: Some(Arc::new(move |task_id, key, spec| {
        let path = key.as_ref().map_or(String::new(), |k| k.path.to_string());
        let spec = spec.for_copy(&path, &copier.dir);
        process_task(&copier, task_id, key, spec)
      })),
    },
    {
      let cx = cx.clone();
      move |ctx, receiver| async move {
        process_main(
          ctx,
          receiver,
          screen,
          task_handle,
          cx,
          process_run,
          instance,
          sd_notify,
        )
        .await;
      }
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

  /// Checks the lines the screen finished since the last output, and the
  /// line so far, against the log ready check; true when this makes the
  /// instance ready.
  fn log_ready(
    &mut self,
    ready: &Option<ReadyConfig>,
    task_screen: &mut TaskScreen,
    lines: &mut Vec<String>,
    also: Option<&str>,
  ) -> bool {
    let Some(ReadyConfig {
      check: ReadyCheck::Log(needle),
      ..
    }) = ready
    else {
      return false;
    };
    if self.ready_sent {
      // Nothing left to look for in the lines.
      task_screen.set_line_tap(false);
      return false;
    }
    lines.clear();
    task_screen.take_finished_lines(lines);
    let found = also.is_some_and(|line| line_ready(needle, line))
      || lines.iter().any(|line| line_ready(needle, line))
      || line_ready(needle, &task_screen.screen().cursor_line_text());
    if found {
      self.ready_sent = true;
      task_screen.set_line_tap(false);
    }
    found
  }
}

#[cfg(unix)]
fn sd_notify_snapshot(
  sd_notify: Option<&DatagramSocket>,
) -> Option<snap::SdNotifySocket> {
  sd_notify.map(|socket| snap::SdNotifySocket {
    path: socket.path().to_string_lossy().into_owned(),
    fd: socket.raw_fd(),
  })
}

#[cfg(not(unix))]
fn sd_notify_snapshot(
  _sd_notify: Option<&DatagramSocket>,
) -> Option<snap::SdNotifySocket> {
  None
}

fn has_sd_notify_check(config: &ProcessRun) -> bool {
  matches!(
    config.ready,
    Some(ReadyConfig {
      check: ReadyCheck::SdNotify,
      ..
    })
  )
}

/// The spec a run starts with: the task's, plus `NOTIFY_SOCKET` for an
/// sd_notify check. Its socket is bound at the first such run and kept
/// for the task's life; a run without the check lets it go.
#[cfg(unix)]
fn sd_notify_spec<'a>(
  ctx: &TaskContext,
  cx: &RunCx,
  config: &'a ProcessRun,
  sd_notify: &mut Option<DatagramSocket>,
) -> Result<Cow<'a, ProcessSpec>, String> {
  if !has_sd_notify_check(config) {
    *sd_notify = None;
    return Ok(Cow::Borrowed(&config.spec));
  }
  if sd_notify.is_none() {
    let bound = crate::runner::lockfile::sd_notify_socket_path(
      cx.runner.as_ref(),
      ctx.task_id.0,
    )
    .and_then(|path| Ok(DatagramSocket::bind(path)?));
    match bound {
      Ok(socket) => *sd_notify = Some(socket),
      Err(err) => {
        return Err(format!(
          "\x1b[31mCannot bind the sd_notify socket: {err:#}\x1b[0m\r\n"
        ));
      }
    }
  }
  let socket = sd_notify.as_ref().expect("bound above");
  // What the previous instance sent must not count for this one.
  socket.drain();
  let mut spec = config.spec.clone();
  spec.env("NOTIFY_SOCKET", socket.path().to_string_lossy());
  Ok(Cow::Owned(spec))
}

#[cfg(not(unix))]
fn sd_notify_spec<'a>(
  _ctx: &TaskContext,
  _cx: &RunCx,
  config: &'a ProcessRun,
  _sd_notify: &mut Option<DatagramSocket>,
) -> Result<Cow<'a, ProcessSpec>, String> {
  Ok(Cow::Borrowed(&config.spec))
}

/// Whether the ready check reads the lines the screen finishes.
fn watches_lines(config: &ProcessRun) -> bool {
  matches!(
    config.ready,
    Some(ReadyConfig {
      check: ReadyCheck::Log(_),
      ..
    })
  )
}

fn snapshot(
  config: &ProcessRun,
  process: Option<&NativeProcess>,
  task_screen: &TaskScreen,
  instance: &Instance,
  sd_notify: Option<&DatagramSocket>,
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
    Some(ReadyCheck::SdNotify) | None => (None, None),
  };
  #[cfg(unix)]
  let instance = process.map(|p| snap::Instance {
    pid: p.pid(),
    master_fd: p.master_fd(),
    exit: instance.exit_info.map(Into::into),
    stdout_eof: instance.stdout_eof,
    ready_sent: instance.ready_sent,
    // Only a polled or sd_notify check still waiting reads it; written only
    // then, so an older binary can take back any other snapshot.
    stop_sent: instance.stop_sent
      && (ready_probe.is_some() || has_sd_notify_check(config))
      && !instance.ready_sent
      && instance.exit_info.is_none(),
    ready_line: String::new(),
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
    sd_notify: sd_notify_snapshot(sd_notify),
    scrollback_len: config.scrollback_len,
    mouse_scroll_speed: config.mouse_scroll_speed,
    instance,
    screen: task_screen.screen().snapshot(),
  }
}

#[allow(clippy::too_many_arguments)]
async fn process_main(
  ctx: TaskContext,
  mut receiver: UnboundedReceiver<TaskCmd>,
  screen: Screen,
  handle: ScreenHandle,
  cx: Arc<RunCx>,
  // The run under way, or the last one: what the task stops, checks and
  // logs by.
  mut config: ProcessRun,
  saved: Option<snap::Instance>,
  saved_sd_notify: Option<snap::SdNotifySocket>,
) {
  let mut task_screen =
    TaskScreen::new(ctx.task_id, screen, handle, config.mouse_scroll_speed);
  task_screen.set_line_tap(watches_lines(&config));
  let mut screen_effects: Vec<TaskScreenEffect> = Vec::new();
  let mut finished_lines: Vec<String> = Vec::new();

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
  // The socket of an sd_notify check: bound at the first start of a run
  // with the check, kept for the task's life, inherited across an upgrade.
  #[cfg(unix)]
  let mut sd_notify: Option<DatagramSocket> =
    saved_sd_notify.and_then(|saved| {
      DatagramSocket::adopt(saved.fd, PathBuf::from(&saved.path))
        .map_err(|err| {
          log::error!("Failed to adopt sd_notify socket {}: {err}", saved.path)
        })
        .ok()
    });
  #[cfg(not(unix))]
  let mut sd_notify: Option<DatagramSocket> = {
    let _ = saved_sd_notify;
    None
  };

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
        // An older binary saved the line it had not searched yet; the
        // screen itself carries the rest.
        let line = snap::from_base64(&saved.ready_line).unwrap_or_default();
        let line = String::from_utf8_lossy(&line);
        if instance.log_ready(
          &config.ready,
          &mut task_screen,
          &mut finished_lines,
          Some(&line),
        ) {
          ctx.send(KernelCommand::TaskReady);
        }
        probe = start_probe(&config, &instance, sd_notify.as_ref());
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
      DragScroll,
      SyncTimeout,
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
    // A selection dragged to the top or bottom row keeps scrolling while
    // the pointer rests there.
    let drag_scroll_at = task_screen.drag_scroll_deadline();
    let drag_scroll_fut = async {
      match drag_scroll_at {
        Some(at) => tokio::time::sleep_until(at).await,
        None => pending().await,
      }
    };
    // A synchronized update the program never ends is painted anyway.
    let sync_at = task_screen.sync_deadline();
    let sync_fut = async {
      match sync_at {
        Some(at) => tokio::time::sleep_until(at).await,
        None => pending().await,
      }
    };
    let next = tokio::select! {
      cmd = receiver.recv() => Next::Cmd(cmd),
      n = read_fut => Next::Read(n),
      info = exit_fut => Next::Exited(info),
      () = probe_fut => Next::Ready,
      () = drag_scroll_fut => Next::DragScroll,
      () = sync_fut => Next::SyncTimeout,
    };

    match next {
      Next::Cmd(None) => break,
      Next::Cmd(Some(cmd)) => match cmd {
        TaskCmd::Start(run) if process.is_none() => {
          if let Some(Run { spec, path }) = run.map(|run| *run) {
            let path = path.as_ref().map_or("", TaskPath::as_str);
            match cx.read(path, &spec) {
              Ok((cfg, _)) => config = ProcessRun::of(&cx, &cfg),
              Err(err) => {
                let message = format!("\r\n[dekit: can't run it: {err:#}]\r\n");
                task_screen
                  .process(message.as_bytes(), &mut screen_effects)
                  .await;
                screen_effects.clear();
                ctx.send(KernelCommand::TaskStopped(ExitInfo::error()));
                continue;
              }
            }
          }
          let spec = match sd_notify_spec(&ctx, &cx, &config, &mut sd_notify) {
            Ok(spec) => spec,
            Err(message) => {
              task_screen
                .process(message.as_bytes(), &mut screen_effects)
                .await;
              screen_effects.clear();
              ctx.send(KernelCommand::TaskStopped(ExitInfo::error()));
              continue;
            }
          };
          match start_instance(&ctx, &config, &spec, &mut task_screen) {
            Ok((p, receiver)) => {
              instance.exit_info = None;
              instance.stdout_eof = false;
              instance.ready_sent = false;
              instance.stop_sent = false;
              // The ready check looks at the new instance's lines.
              task_screen.set_line_tap(watches_lines(&config));
              probe = start_probe(&config, &instance, sd_notify.as_ref());
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
        TaskCmd::Start(_) => {}
        TaskCmd::Stop => {
          probe = None;
          instance.stop_sent = true;
          if let Some(p) = process.as_mut() {
            stop_process(p, &config.stop, task_screen.screen(), &config.spec)
              .await;
          }
        }
        TaskCmd::Kill => {
          probe = None;
          instance.stop_sent = true;
          if let Some(p) = process.as_mut() {
            p.kill(config.stop.kill_group()).await.log_ignore();
          }
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
          let saved = snapshot(
            &config,
            process.as_ref(),
            &task_screen,
            &instance,
            sd_notify.as_ref(),
          );
          ctx.send(KernelCommand::TaskFrozen(
            number,
            snap::TaskKind::Process(saved),
          ));
        }
        TaskCmd::Thaw => {
          frozen = false;
          if process.is_some() {
            probe = start_probe(&config, &instance, sd_notify.as_ref());
          }
        }
        TaskCmd::Msg(msg) => match msg.downcast::<TaskScreenCmd>() {
          Ok(cmd) => {
            task_screen.handle_cmd(*cmd, &mut screen_effects);
            apply_effects(
              &mut screen_effects,
              &mut process,
              task_screen.screen(),
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
      Next::DragScroll => task_screen.drag_scroll_tick(),
      Next::SyncTimeout => task_screen.sync_timeout(),
      Next::Read(Ok(0)) => {
        instance.stdout_eof = true;
        // Nothing more will end an open synchronized update.
        task_screen.sync_timeout();
      }
      Next::Read(Ok(n)) => {
        task_screen
          .process(&read_buf[..n], &mut screen_effects)
          .await;
        if instance.log_ready(
          &config.ready,
          &mut task_screen,
          &mut finished_lines,
          None,
        ) {
          ctx.send(KernelCommand::TaskReady);
        }
        apply_effects(
          &mut screen_effects,
          &mut process,
          task_screen.screen(),
          &mut key_buf,
        )
        .await;
      }
      Next::Read(Err(e)) => {
        log::warn!("Process read error: {}", e);
        instance.stdout_eof = true;
        task_screen.sync_timeout();
      }
    }
  }
}

type ProbeFuture = Pin<Box<dyn Future<Output = ()> + Send>>;

/// The ready check that waits, unless the instance is ready, is being
/// stopped, or has exited: a polled check, or the sd_notify socket's read.
fn start_probe(
  config: &ProcessRun,
  instance: &Instance,
  sd_notify: Option<&DatagramSocket>,
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
      check: ReadyCheck::SdNotify,
      ..
    }) => sd_notify_wait(sd_notify),
    Some(ReadyConfig {
      check: ReadyCheck::Log(_),
      ..
    })
    | None => None,
  }
}

/// The wait for `READY=1` on the task's sd_notify socket.
#[cfg(unix)]
fn sd_notify_wait(sd_notify: Option<&DatagramSocket>) -> Option<ProbeFuture> {
  sd_notify
    .map(|socket| Box::pin(wait_sd_notify(socket.reader())) as ProbeFuture)
}

#[cfg(not(unix))]
fn sd_notify_wait(_sd_notify: Option<&DatagramSocket>) -> Option<ProbeFuture> {
  None
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
  if let Some((path, _)) = current
    && *path == sink.path
  {
    return;
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
  run: &ProcessRun,
  spec: &ProcessSpec,
  task_screen: &mut TaskScreen,
) -> Result<(NativeProcess, UnboundedReceiver<ExitInfo>), String> {
  let size = task_screen.winsize();
  task_screen.reset_screen(run.scrollback_len, run.mouse_scroll_speed);
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
  #[allow(unused_mut)]
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
  vt: &Screen,
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
          let bracketed = vt.bracketed_paste();
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
  vt: &Screen,
  key: Key,
  buf: &mut Vec<u8>,
) {
  // Keys are encoded as the program asked: the kitty flags it pushed,
  // or the legacy encoding.
  let modes = KeyEncodeModes {
    kitty_flags: vt.kitty_flags(),
    application_cursor_keys: vt.application_cursor(),
    newline_mode: vt.modes().newline,
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
  vt: &Screen,
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
  vt: &Screen,
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
  /// A saved task as a runner resumes it, around its child.
  #[cfg(unix)]
  fn resumed(saved: &snap::Task) -> TaskRegistration {
    let snap::TaskKind::Process(process) = &saved.kind else {
      unreachable!()
    };
    let deps = saved
      .deps
      .iter()
      .map(|id| TaskSelector::Id(TaskId(*id)))
      .collect();
    crate::task::config_tasks::saved_task_registration(
      &crate::config::config::Config::make_default(),
      TaskId(saved.id),
      saved,
      process,
      deps,
      resumed_instance(saved, process),
    )
    .unwrap()
  }

  fn test_screen(wheel: usize) -> TaskScreen {
    let screen = Screen::new(DEFAULT_SIZE, 0);
    let handle = ScreenHandle::new(screen.frame());
    TaskScreen::new(TaskId(1), screen, handle, wheel)
  }

  use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

  use crate::config::task_log::TaskLogConfig;
  use crate::kernel::kernel::Kernel;
  use crate::kernel::kernel_message::{
    KernelCommand, SpaceSelector, TaskContext, Verb,
  };
  use crate::kernel::task::{TaskId, TaskKind, TaskState};
  use crate::kernel::task_path::TaskPath;

  use super::*;

  /// Registers a process task at `path` made from the definition `yaml`.
  fn spawn_process_task(
    parent: &TaskContext,
    path: &str,
    yaml: &str,
  ) -> TaskId {
    let cx = Arc::new(RunCx {
      dir: std::env::temp_dir(),
      runner: None,
    });
    let key = TaskKey::default_space(TaskPath::new(path).unwrap());
    let spec: TaskSpec = serde_yaml::from_str(yaml).unwrap();
    let task_id = parent.alloc_id();
    let registration = process_task(&cx, task_id, Some(key), spec).unwrap();
    drop(parent.register_task(registration));
    task_id
  }

  /// What a definition says, as YAML: a path in it quoted.
  fn yaml_path(path: &std::path::Path) -> String {
    format!("'{}'", path.display())
  }

  async fn resolve(pc: &TaskContext, path: &str) -> TaskId {
    let rx = pc.list_tasks(TaskSelector::Glob(
      SpaceSelector::default_space(),
      path.to_string(),
    ));
    let tasks = tokio::time::timeout(Duration::from_secs(1), rx)
      .await
      .expect("timed out resolving path");
    match &tasks[..] {
      [task] => task.id,
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

    let id = spawn_process_task(
      &pc,
      "logged",
      &format!(
        "{{cmd: [sh, -c, printf hello-log], log: {{file: {}, mode: truncate}}}}",
        yaml_path(&log_path)
      ),
    );
    pc.send(KernelCommand::Act(Verb::Start, TaskSelector::Id(id), None));

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

    let id = spawn_process_task(
      &pc,
      "pidlog",
      &format!(
        "{{cmd: [sh, -c, printf hi], log: {{file: {}, mode: truncate}}}}",
        yaml_path(&dir.join("{pid}.log"))
      ),
    );
    pc.send(KernelCommand::Act(Verb::Start, TaskSelector::Id(id), None));

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
      ("by_cmd", "[touch, by_cmd]"),
      ("by_sh", "[sh, -c, 'printf done > by_sh']"),
    ];
    for (name, stop) in stops {
      let id = spawn_process_task(
        &pc,
        name,
        &format!(
          "{{cmd: [sleep, '100'], cwd: {}, stop: {{cmd: {stop}}}}}",
          yaml_path(&dir)
        ),
      );
      pc.send(KernelCommand::Act(Verb::Start, TaskSelector::Id(id), None));
    }

    let kernel_task = tokio::spawn(kernel.run());

    let mut ids = Vec::new();
    for name in ["by_cmd", "by_sh"] {
      let id = resolve(&pc, name).await;
      pc.send(KernelCommand::Act(Verb::Stop, TaskSelector::Id(id), None));
      ids.push(id);
    }

    let deadline = Instant::now() + Duration::from_secs(2);
    while !(dir.join("by_cmd").exists() && dir.join("by_sh").exists()) {
      assert!(Instant::now() < deadline, "a stop command never ran");
      tokio::time::sleep(Duration::from_millis(10)).await;
    }

    for id in ids {
      pc.send(KernelCommand::Act(Verb::Kill, TaskSelector::Id(id), None));
      pc.send(KernelCommand::Remove(TaskSelector::Id(id), None));
    }
    pc.send(KernelCommand::Quit);
    tokio::time::timeout(Duration::from_secs(2), kernel_task)
      .await
      .expect("timed out waiting for kernel to quit")
      .unwrap();

    let _ = std::fs::remove_dir_all(&dir);
  }

  /// A saved task comes back as its spec says, whatever was saved beside
  /// it: an older runner saved a label that was the path, which would
  /// stay behind when the task moves.
  #[test]
  fn a_saved_task_comes_back_as_its_spec_says() {
    let run = ProcessRun::new(ProcessSpec::from_argv(vec!["true".into()]));
    let screen = test_screen(run.mouse_scroll_speed);
    let process = snapshot(&run, None, &screen, &Instance::default(), None);
    let mut saved = snap::Task {
      id: 1,
      space: String::new(),
      path: Some("ember-forge".to_string()),
      label: Some("tui/ember-forge".to_string()),
      tags: Vec::new(),
      pinned: false,
      deps: Vec::new(),
      restart: snap::Restart::Never,
      job: false,
      ready_timeout_ms: None,
      stop_timeout_ms: None,
      state: snap::TaskState::Idle {},
      vetoed: false,
      killed: false,
      start_failed: false,
      stop_timed_out: false,
      saved_pin: false,
      attempts: 0,
      last_start_secs_ago: None,
      timer_ms: None,
      spec: Some(serde_yaml::from_str("{cmd: [forge]}").unwrap()),
      run_spec: None,
      origin: None,
      kind: snap::TaskKind::Process(process.clone()),
    };
    let def = |saved: &snap::Task| {
      crate::task::config_tasks::saved_task_registration(
        &crate::config::config::Config::make_default(),
        TaskId(1),
        saved,
        &process,
        Vec::new(),
        None,
      )
      .unwrap()
      .def
    };
    assert_eq!(def(&saved).label, None);
    saved.spec = Some(
      serde_yaml::from_str(
        "{label: Forge, type: job, cmd: [forge], autorestart: on-failure}",
      )
      .unwrap(),
    );
    let restored = def(&saved);
    assert_eq!(restored.label.as_deref(), Some("Forge"));
    assert_eq!(restored.kind, TaskKind::Job);
    assert_eq!(restored.restart, RestartMode::OnFailure);
    // Saved mid-run since an edit: the run goes on as it started, and
    // the edit is the next run's.
    saved.spec =
      Some(serde_yaml::from_str("{label: v2, cmd: [forge, --v2]}").unwrap());
    saved.run_spec = Some(
      serde_yaml::from_str("{type: job, cmd: [forge], stop: {timeout: 9s}}")
        .unwrap(),
    );
    let restored = def(&saved);
    let argv = |spec: Option<&TaskSpec>| spec.and_then(TaskSpec::argv);
    assert_eq!(argv(restored.spec.as_ref()), Some(vec!["forge".into()]));
    assert_eq!(restored.kind, TaskKind::Job);
    assert_eq!(restored.stop_timeout, Duration::from_secs(9));
    assert_eq!(restored.label.as_deref(), Some("v2"));
    let next = restored.next.expect("the edit is the next run's");
    assert_eq!(next.kind, TaskKind::Service);
    assert_eq!(
      argv(next.spec.as_ref()),
      Some(vec!["forge".into(), "--v2".into()])
    );
    saved.run_spec = None;
    // Without a spec, what was saved is all there is.
    saved.spec = None;
    assert_eq!(def(&saved).label.as_deref(), Some("tui/ember-forge"));
  }

  /// An older runner saved no definition: one is read back from what it
  /// saved beside its child, as that child runs.
  #[test]
  fn a_task_saved_without_a_definition_reads_back_as_it_ran() {
    let run = ProcessRun {
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
      ..ProcessRun::new(ProcessSpec::from_argv(vec!["true".into()]))
    };
    let screen = test_screen(run.mouse_scroll_speed);
    let process = snapshot(&run, None, &screen, &Instance::default(), None);
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
      stop_timed_out: false,
      saved_pin: false,
      attempts: 0,
      last_start_secs_ago: None,
      timer_ms: None,
      spec: None,
      run_spec: None,
      origin: None,
      kind: snap::TaskKind::Process(process.clone()),
    };
    let read = |process: &snap::ProcessTask| {
      crate::task::config_tasks::saved_task_registration(
        &crate::config::config::Config::make_default(),
        TaskId(1),
        &saved,
        process,
        Vec::new(),
        None,
      )
      .unwrap()
      .def
    };
    let def = read(&process);
    assert_eq!(def.stop_timeout, Duration::from_secs(3));
    assert_eq!(
      def.ready,
      ReadyMode::Reported {
        timeout: Some(Duration::from_secs(30))
      }
    );
    let said = |def: &TaskDef, key: &str| {
      crate::spec::show_value(def.spec.as_ref().unwrap().value_at(key))
    };
    assert_eq!(
      said(&def, "ready"),
      "{tcp: 5432, interval: 250ms, timeout: 30s}"
    );

    // An older snapshot's `ready_log` is the log check.
    let mut old = process.clone();
    old.ready_probe = None;
    old.ready_log = Some("listening".to_string());
    assert_eq!(said(&read(&old), "ready"), "{log: listening, timeout: 30s}");

    for (stop, said_stop) in [
      (
        StopSignal::Signal {
          sig: Sig::Int,
          group: false,
        },
        "{signal: SIGINT, group: false, timeout: 3s}",
      ),
      (
        StopSignal::Keys(vec![Key::parse("<C-c>").unwrap()]),
        "{keys: [<C-c>], timeout: 3s}",
      ),
      (
        StopSignal::Cmd(vec!["podman".to_string(), "stop".to_string()]),
        "{cmd: [podman, stop], timeout: 3s}",
      ),
    ] {
      let run = ProcessRun {
        stop,
        ..ProcessRun::new(ProcessSpec::from_argv(vec!["true".into()]))
      };
      let process = snapshot(&run, None, &screen, &Instance::default(), None);
      assert_eq!(said(&read(&process), "stop"), said_stop);
    }

    // An older binary's `cmd` line was meant for the system shell and
    // keeps running there; its `shutdown` and `kill` are SIGTERM and
    // SIGKILL to the group.
    let mut old = process.clone();
    old.stop = snap::StopSignal::Cmd {
      cmd: "kill $(cat pid)".to_string(),
    };
    let argv = crate::parse_shell::system_argv("kill $(cat pid)");
    let stop = said(&read(&old), "stop");
    assert!(stop.contains(&argv[0]) && stop.contains("kill"), "{stop}");
    old.stop = snap::StopSignal::Shutdown {};
    assert_eq!(said(&read(&old), "stop"), "{timeout: 3s}");
    old.stop = snap::StopSignal::Kill {};
    assert_eq!(said(&read(&old), "stop"), "{signal: SIGKILL, timeout: 3s}");

    // How it logs and how much it keeps, so an edit or a write-back keeps
    // them too; what it has anyway is left out.
    let run = ProcessRun {
      log: Some(LogSpec {
        config: TaskLogConfig {
          enabled: Some(true),
          dir: Some("/var/log/forge".into()),
          file: None,
          mode: None,
        },
        name: "forge".into(),
      }),
      scrollback_len: 50,
      ..ProcessRun::new(ProcessSpec::from_argv(vec!["true".into()]))
    };
    let def = read(&snapshot(&run, None, &screen, &Instance::default(), None));
    assert_eq!(
      said(&def, "log"),
      "{enabled: true, dir: /var/log/forge, mode: append}"
    );
    assert_eq!(said(&def, "scrollback_len"), "50");
    assert_eq!(said(&def, "autorestart"), "never");
    let plain = read(&snapshot(
      &ProcessRun::new(ProcessSpec::from_argv(vec!["true".into()])),
      None,
      &screen,
      &Instance::default(),
      None,
    ));
    for key in ["log", "scrollback_len", "mouse_scroll_speed"] {
      assert!(plain.spec.as_ref().unwrap().get(key).is_none(), "{key}");
    }
  }

  #[cfg(unix)]
  #[tokio::test]
  async fn failed_adopt_reports_the_task_stopped() {
    let config =
      ProcessRun::new(ProcessSpec::from_argv(vec!["true".to_string()]));
    let screen = test_screen(config.mouse_scroll_speed);
    let mut process =
      snapshot(&config, None, &screen, &Instance::default(), None);
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
      stop_timed_out: false,
      saved_pin: false,
      attempts: 1,
      last_start_secs_ago: None,
      timer_ms: None,
      spec: None,
      run_spec: None,
      origin: None,
      kind: snap::TaskKind::Process(process.clone()),
    };
    let registration = resumed(&saved);

    let mut kernel = Kernel::new();
    let pc = kernel.context();
    kernel
      .restore(2, vec![(Some(&saved), registration.into())])
      .unwrap();
    let kernel_task = tokio::spawn(kernel.run());

    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
      let tasks = pc.list_tasks(TaskSelector::Id(TaskId(1))).await;
      let active = tasks[0].state.is_active();
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
    pc.list_tasks(TaskSelector::Id(id)).await[0].state
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
    pc.send(KernelCommand::Act(Verb::Start, TaskSelector::Id(id), None));
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
  /// A task that says it is ready the way a systemd service does.
  #[tokio::test]
  async fn an_sd_notify_task_is_ready_when_it_says_so() {
    let python = std::process::Command::new("python3")
      .args(["-c", "import socket"])
      .output();
    if !python.is_ok_and(|output| output.status.success()) {
      eprintln!("no python3: skipped");
      return;
    }
    let dir = std::env::temp_dir()
      .join(format!("dekit_sd_notify_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let script = dir.join("sd_notify.py");
    std::fs::write(
      &script,
      r"import os, socket, time
s = socket.socket(socket.AF_UNIX, socket.SOCK_DGRAM)
to = os.environ['NOTIFY_SOCKET']
s.sendto(b'STATUS=warming up\n', to)
s.sendto(b'READY=1\n', to)
time.sleep(60)
",
    )
    .unwrap();

    let kernel = Kernel::new();
    let pc = kernel.context();
    let id = spawn_process_task(
      &pc,
      "notified",
      &format!(
        "{{cmd: [python3, {}], ready: {{sd_notify: true}}}}",
        yaml_path(&script)
      ),
    );
    pc.send(KernelCommand::Act(Verb::Start, TaskSelector::Id(id), None));
    let kernel_task = tokio::spawn(kernel.run());

    let deadline = Instant::now() + Duration::from_secs(10);
    while state_of(&pc, id).await != TaskState::Ready {
      assert!(Instant::now() < deadline, "the task never got ready");
      tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let socket =
      crate::runner::lockfile::sd_notify_socket_path(None, id.0).unwrap();
    assert!(socket.exists());

    // No reaper runs in unit tests: the task is removed to let quit through.
    pc.send(KernelCommand::Remove(TaskSelector::Id(id), None));
    pc.send(KernelCommand::Quit);
    tokio::time::timeout(Duration::from_secs(2), kernel_task)
      .await
      .expect("timed out waiting for kernel to quit")
      .unwrap();
    // The task took its socket with it.
    let deadline = Instant::now() + Duration::from_secs(2);
    while socket.exists() {
      assert!(
        Instant::now() < deadline,
        "the sd_notify socket was left behind"
      );
      tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let _ = std::fs::remove_dir_all(&dir);
  }

  #[tokio::test]
  async fn a_resumed_task_checks_ready_only_while_running() {
    let screen = test_screen(5);
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
      let config = ProcessRun {
        ready: Some(ReadyConfig {
          check,
          timeout: None,
        }),
        ..ProcessRun::new(ProcessSpec::from_argv(vec!["true".into()]))
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
      let mut process =
        snapshot(&config, None, &screen, &Instance::default(), None);
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
        stop_timed_out: false,
        saved_pin: false,
        attempts: 1,
        last_start_secs_ago: None,
        timer_ms: None,
        spec: None,
        run_spec: None,
        origin: None,
        kind: snap::TaskKind::Process(process),
      });
    }
    let restored = tasks
      .iter()
      .map(|saved| (Some(saved), resumed(saved).into()))
      .collect();
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
