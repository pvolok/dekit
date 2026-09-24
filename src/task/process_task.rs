use std::future::pending;

use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

use crate::error::ResultLogger;
use crate::kernel::kernel_message::{
  KernelCommand, SharedVt, TaskContext, TaskRegistration, TaskSelector,
};
use crate::kernel::task::{
  ExitInfo, ReadyMode, RestartMode, TaskCmd, TaskDef, TaskId,
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
  /// Graceful stop of the whole process tree. Unix: SIGTERM to the group.
  /// Windows: Ctrl-C (TODO).
  Shutdown,
  /// Force-kill the whole process tree. Unix: SIGKILL to the group. Windows:
  /// terminate (the process today; TODO: Job Object).
  Kill,
  Signal {
    sig: Sig,
    group: bool,
  },
  SendKeys(Vec<Key>),
  /// Run a shell command as the stop action. Useful for tools like
  /// `podman compose` that don't reliably respond to signals but do have
  /// an explicit teardown command (e.g. `podman compose down`). The main
  /// process is expected to exit on its own once the stop command
  /// completes (e.g. `compose up` exits when containers go away).
  Cmd(String),
}

impl Default for StopSignal {
  fn default() -> Self {
    StopSignal::Shutdown
  }
}

impl StopSignal {
  /// Target for a force-kill (the grace-period timeout or an explicit `Kill`):
  /// honor a `Signal` stop's own choice, otherwise force-kill the whole group
  /// so orphaned children don't leak.
  fn kill_group(&self) -> bool {
    match self {
      StopSignal::Signal { group, .. } => *group,
      StopSignal::Shutdown
      | StopSignal::Kill
      | StopSignal::SendKeys(_)
      | StopSignal::Cmd(_) => true,
    }
  }
}

pub struct ProcessTaskConfig {
  pub spec: ProcessSpec,
  pub label: Option<String>,
  pub stop: StopSignal,
  pub log: Option<LogSpec>,
  pub restart: RestartMode,
  /// Readiness probe: the task reports ready once an output line contains
  /// this string. Without it the task is ready as soon as it starts.
  pub ready_log: Option<String>,
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
      stop: StopSignal::default(),
      log: None,
      restart: RestartMode::Never,
      ready_log: None,
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
    process.instance.clone(),
  )
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
    snap::StopSignal::Shutdown {} => StopSignal::Shutdown,
    snap::StopSignal::Kill {} => StopSignal::Kill,
    snap::StopSignal::Signal { sig, group } => StopSignal::Signal {
      sig: Sig::from_name(sig)
        .ok_or_else(|| anyhow::anyhow!("unknown stop signal {sig}"))?,
      group: *group,
    },
    snap::StopSignal::SendKeys { keys } => StopSignal::SendKeys(keys.clone()),
    snap::StopSignal::Cmd { cmd } => StopSignal::Cmd(cmd.clone()),
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
  Ok(ProcessTaskConfig {
    spec,
    label: saved.label.clone(),
    stop,
    log,
    restart: saved.restart.into(),
    ready_log: process.ready_log.clone(),
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
      ready: match config.ready_log {
        Some(_) => ReadyMode::Reported,
        None => ReadyMode::Immediate,
      },
      restart: config.restart,
      deps: std::mem::take(&mut config.deps),
      space,
      path,
      label: config.label.take(),
      vt: Some(vt),
      tags: std::mem::take(&mut config.tags),
      pinned: config.pinned,
      ..Default::default()
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
  ready_line_buf: Vec<u8>,
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
}

fn snapshot(
  config: &ProcessTaskConfig,
  process: Option<&NativeProcess>,
  task_screen: &TaskScreen,
  instance: &Instance,
) -> snap::ProcessTask {
  let stop = match &config.stop {
    StopSignal::Shutdown => snap::StopSignal::Shutdown {},
    StopSignal::Kill => snap::StopSignal::Kill {},
    StopSignal::Signal { sig, group } => snap::StopSignal::Signal {
      sig: sig.name().to_string(),
      group: *group,
    },
    StopSignal::SendKeys(keys) => {
      snap::StopSignal::SendKeys { keys: keys.clone() }
    }
    StopSignal::Cmd(cmd) => snap::StopSignal::Cmd { cmd: cmd.clone() },
  };
  #[cfg(unix)]
  let instance = process.map(|p| snap::Instance {
    pid: p.pid(),
    master_fd: p.master_fd(),
    exit: instance.exit_info.map(Into::into),
    stdout_eof: instance.stdout_eof,
    ready_sent: instance.ready_sent,
    ready_line: snap::to_base64(&instance.ready_line_buf),
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
    ready_log: config.ready_log.clone(),
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

  let mut process: Option<NativeProcess> = None;
  let mut instance = Instance::default();
  let mut read_buf = [0u8; 8 * 1024];
  let mut key_buf: Vec<u8> = Vec::new();
  // Frozen for an upgrade: no reads until thawed.
  let mut frozen = false;

  #[cfg(unix)]
  if let Some(saved) = saved {
    match adopt_native(&saved) {
      Ok((adopted, receiver)) => {
        process = Some(adopted);
        instance.exits = receiver;
        instance.exit_info = saved.exit.map(Into::into);
        instance.stdout_eof = saved.stdout_eof;
        instance.ready_sent = saved.ready_sent;
        instance.ready_line_buf =
          snap::from_base64(&saved.ready_line).unwrap_or_default();
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
    let next = tokio::select! {
      cmd = receiver.recv() => Next::Cmd(cmd),
      n = read_fut => Next::Read(n),
      info = exit_fut => Next::Exited(info),
    };

    match next {
      Next::Cmd(None) => break,
      Next::Cmd(Some(cmd)) => match cmd {
        TaskCmd::Start => {
          if process.is_none()
            && let Some((p, receiver)) =
              start_instance(&ctx, &config.spec, task_screen.vt())
          {
            instance.exit_info = None;
            instance.stdout_eof = false;
            instance.ready_line_buf.clear();
            instance.ready_sent = false;
            update_log_observer(
              &mut task_screen,
              &config.log,
              &mut instance.current_log,
              ctx.task_id,
              p.pid(),
            );
            process = Some(p);
            instance.exits = Some(receiver);
          }
        }
        TaskCmd::Stop => {
          if let Some(p) = process.as_mut() {
            stop_process(p, &config.stop, task_screen.vt(), &config.spec).await;
          }
        }
        TaskCmd::Kill => {
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
              stop: config.stop.clone(),
              log: None,
              restart: config.restart,
              ready_log: config.ready_log.clone(),
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
          let saved =
            snapshot(&config, process.as_ref(), &task_screen, &instance);
          ctx.send(KernelCommand::TaskFrozen(
            number,
            snap::TaskKind::Process(saved),
          ));
        }
        TaskCmd::Thaw => frozen = false,
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
      Next::Exited(Some(info)) => instance.exited(info, process.as_mut()),
      Next::Exited(None) => instance.exits = None,
      Next::Read(Ok(0)) => instance.stdout_eof = true,
      Next::Read(Ok(n)) => {
        if let Some(pattern) = &config.ready_log
          && !instance.ready_sent
        {
          instance.ready_sent = scan_ready(
            &ctx,
            pattern,
            &mut instance.ready_line_buf,
            &read_buf[..n],
          );
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

/// Match completed output lines against the readiness pattern; reports
/// `TaskReady` and returns true on the first match.
fn scan_ready(
  ctx: &TaskContext,
  pattern: &str,
  line_buf: &mut Vec<u8>,
  bytes: &[u8],
) -> bool {
  for b in bytes {
    if *b == b'\n' {
      if String::from_utf8_lossy(line_buf).contains(pattern) {
        ctx.send(KernelCommand::TaskReady);
        return true;
      }
      line_buf.clear();
    } else if line_buf.len() < 4096 {
      line_buf.push(*b);
    }
  }
  false
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
) -> Option<(NativeProcess, UnboundedReceiver<ExitInfo>)> {
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
      Some(spawned)
    }
    Err(err) => {
      log::warn!("Process spawn error: {}", err);
      ctx.send(KernelCommand::TaskStopped(ExitInfo::error()));
      None
    }
  }
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
    StopSignal::Shutdown => {
      process.send_signal(libc::SIGTERM, true).log_ignore()
    }
    StopSignal::Kill => process.send_signal(libc::SIGKILL, true).log_ignore(),
    StopSignal::Signal { sig, group } => {
      process.send_signal(sig.to_libc(), *group).log_ignore();
    }
    StopSignal::SendKeys(keys) => {
      let mut buf = Vec::new();
      for key in keys {
        send_key(process, vt, key.clone(), &mut buf).await;
      }
    }
    StopSignal::Cmd(shell) => run_stop_cmd(spec, shell.clone()),
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
    // TODO: deliver Ctrl-C through the ConPTY for a graceful shutdown; for now
    // fall back to terminating the process.
    StopSignal::Shutdown => process.kill(true).await.log_ignore(),
    // TODO: terminate the whole tree via a Job Object; for now terminate the
    // process.
    StopSignal::Kill => process.kill(true).await.log_ignore(),
    // Windows has no real signals: INT/TERM/KILL fall back to terminating the
    // process; everything else has no equivalent and is ignored.
    StopSignal::Signal { sig, .. } => match sig {
      Sig::Int | Sig::Term | Sig::Kill => process.kill(true).await.log_ignore(),
      _ => log::debug!("{sig:?} has no Windows equivalent; ignoring"),
    },
    StopSignal::SendKeys(keys) => {
      let mut buf = Vec::new();
      for key in keys {
        send_key(process, vt, key.clone(), &mut buf).await;
      }
    }
    StopSignal::Cmd(shell) => run_stop_cmd(spec, shell.clone()),
  }
}

fn run_stop_cmd(spec: &ProcessSpec, shell: String) {
  #[cfg(windows)]
  let mut cmd = {
    let mut c = std::process::Command::new("pwsh.exe");
    c.arg("-Command").arg(&shell);
    c
  };
  #[cfg(not(windows))]
  let mut cmd = {
    let mut c = std::process::Command::new("/bin/sh");
    c.arg("-c").arg(&shell);
    c
  };
  if let Some(cwd) = &spec.cwd {
    cmd.current_dir(cwd);
  }
  for (k, v) in &spec.env {
    match v {
      Some(v) => {
        cmd.env(k, v);
      }
      None => {
        cmd.env_remove(k);
      }
    }
  }
  cmd.stdout(std::process::Stdio::null());
  cmd.stderr(std::process::Stdio::null());

  #[cfg(unix)]
  match cmd.spawn() {
    Ok(child) => {
      crate::process::unix_processes_waiter::UnixProcessesWaiter::wait_for_child(
        child,
        Box::new(|info| log::debug!("Stop command exited: {info}")),
      )
    }
    Err(err) => log::warn!("Stop command failed: {err}"),
  }
  #[cfg(windows)]
  tokio::spawn(async move {
    if let Err(err) = tokio::process::Command::from(cmd).status().await {
      log::warn!("Stop command failed: {err}");
    }
  });
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
  use crate::kernel::task::TaskId;

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
  async fn stop_signal_cmd_runs_shell_command() {
    let nanos = SystemTime::now()
      .duration_since(UNIX_EPOCH)
      .unwrap()
      .as_nanos();
    let mut marker = std::env::temp_dir();
    marker.push(format!("dekit_stopcmd_{}_{}", std::process::id(), nanos));

    let kernel = Kernel::new();
    let pc = kernel.context();

    let path = TaskKey::default_space(TaskPath::new("sleeper").unwrap());
    let spec = ProcessSpec::from_argv(vec![
      "sh".to_string(),
      "-c".to_string(),
      "sleep 100".to_string(),
    ]);
    let (id, _) = spawn_process_task(
      &pc,
      Some(path),
      ProcessTaskConfig {
        stop: StopSignal::Cmd(format!("printf done > {}", marker.display())),
        ..ProcessTaskConfig::new(spec)
      },
    );
    pc.send(KernelCommand::Start(TaskSelector::Id(id), None));

    let kernel_task = tokio::spawn(kernel.run());

    let id = resolve(&pc, "sleeper").await;
    pc.send(KernelCommand::Stop(TaskSelector::Id(id), None));

    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
      if marker.exists() {
        break;
      }
      assert!(Instant::now() < deadline, "stop command never ran");
      tokio::time::sleep(Duration::from_millis(10)).await;
    }

    pc.send(KernelCommand::Kill(TaskSelector::Id(id), None));
    pc.send(KernelCommand::Remove(TaskSelector::Id(id), None));
    pc.send(KernelCommand::Quit);
    tokio::time::timeout(Duration::from_secs(2), kernel_task)
      .await
      .expect("timed out waiting for kernel to quit")
      .unwrap();

    let _ = std::fs::remove_file(&marker);
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
      state: snap::TaskState::Running {},
      vetoed: false,
      killed: false,
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
    Box::new(move |info| {
      let _ = exit_sender.send(info);
    }),
  );
  Ok((process, Some(exits)))
}
