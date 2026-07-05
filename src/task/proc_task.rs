use std::future::pending;

use tokio::sync::mpsc::UnboundedReceiver;

use crate::error::ResultLogger;
use crate::kernel::kernel_message::{KernelCommand, SharedVt, TaskContext};
use crate::kernel::task::{
  ExitInfo, ReadyMode, RestartMode, TaskCmd, TaskDef, TaskId,
};
use crate::kernel::task_path::TaskPath;
use crate::kernel::task_screen::{TaskScreen, TaskScreenCmd, TaskScreenEffect};
use crate::process::NativeProcess;
use crate::process::process::Process as _;
use crate::process::process_spec::ProcessSpec;
use crate::task::logger::{LogResolver, spawn_logger};
use crate::term::encode::{KeyCodeEncodeModes, encode_key};
use crate::term::key::Key;
use crate::term::{Parser, Winsize};

struct ProcExited(ExitInfo);

pub struct ProcInput(pub Key);

pub struct DuplicateProc(pub Option<String>);

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

pub struct ProcTaskConfig {
  pub spec: ProcessSpec,
  pub label: Option<String>,
  pub stop: StopSignal,
  pub log: Option<LogResolver>,
  pub restart: RestartMode,
  /// Readiness probe: the task reports ready once an output line contains
  /// this string. Without it the task is ready as soon as it starts.
  pub ready_log: Option<String>,
  pub scrollback_len: usize,
  pub mouse_scroll_speed: usize,
  pub deps: Vec<TaskId>,
  pub tags: Vec<String>,
  /// Pin to init at registration, so a registered task is already
  /// started with no separate `Start` command.
  pub pinned: bool,
}

impl ProcTaskConfig {
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

pub fn spawn_proc_task(
  parent: &TaskContext,
  task_path: Option<TaskPath>,
  config: ProcTaskConfig,
) -> (TaskId, tokio::sync::oneshot::Receiver<bool>) {
  let task_id = parent.alloc_id();
  let ack = spawn_proc_task_with_id(parent, task_id, task_path, config);
  (task_id, ack)
}

pub fn spawn_proc_task_with_id(
  parent: &TaskContext,
  task_id: TaskId,
  task_path: Option<TaskPath>,
  config: ProcTaskConfig,
) -> tokio::sync::oneshot::Receiver<bool> {
  let ProcTaskConfig {
    spec,
    stop,
    log,
    restart,
    ready_log,
    scrollback_len,
    mouse_scroll_speed,
    deps,
    label,
    tags,
    pinned,
  } = config;
  let vt = SharedVt::new(Parser::new(24, 80, scrollback_len));
  let task_vt = vt.clone();
  let ready = match ready_log {
    Some(_) => ReadyMode::Reported,
    None => ReadyMode::Immediate,
  };
  parent.spawn_async_with_id(
    task_id,
    TaskDef {
      ready,
      restart,
      deps,
      path: task_path,
      label,
      vt: Some(vt),
      tags,
      pinned,
      ..Default::default()
    },
    move |ctx, receiver| async move {
      proc_main(
        ctx,
        receiver,
        spec,
        task_vt,
        log,
        stop,
        scrollback_len,
        mouse_scroll_speed,
        restart,
        ready_log,
      )
      .await;
    },
  )
}

async fn proc_main(
  ctx: TaskContext,
  mut receiver: UnboundedReceiver<TaskCmd>,
  spec: ProcessSpec,
  vt: SharedVt,
  mut log: Option<LogResolver>,
  stop: StopSignal,
  scrollback_len: usize,
  mouse_scroll_speed: usize,
  restart: RestartMode,
  ready_log: Option<String>,
) {
  let mut task_screen = TaskScreen::new(ctx.task_id, vt, mouse_scroll_speed);
  let mut screen_effects: Vec<TaskScreenEffect> = Vec::new();

  let mut process: Option<NativeProcess> = None;
  // The log path is resolved per spawn (it may contain the pid).
  let mut current_log: Option<(std::path::PathBuf, u64)> = None;
  let mut read_buf = [0u8; 8 * 1024];
  let mut stdout_eof = false;
  let mut exit_info: Option<ExitInfo> = None;
  let mut ready_line_buf: Vec<u8> = Vec::new();
  let mut ready_sent = false;

  loop {
    if stdout_eof
      && let Some(info) = exit_info
      && process.take().is_some()
    {
      ctx.send(KernelCommand::TaskStopped(info));
    }

    enum Next {
      Cmd(Option<TaskCmd>),
      Read(std::io::Result<usize>),
    }
    let read_fut = async {
      match process.as_mut() {
        Some(p) if !stdout_eof => p.read(&mut read_buf).await,
        _ => pending().await,
      }
    };
    let next = tokio::select! {
      cmd = receiver.recv() => Next::Cmd(cmd),
      n = read_fut => Next::Read(n),
    };

    match next {
      Next::Cmd(None) => break,
      Next::Cmd(Some(cmd)) => match cmd {
        TaskCmd::Start => {
          if process.is_none() {
            process = start_instance(&ctx, &spec, task_screen.vt());
            if let Some(p) = &process {
              exit_info = None;
              stdout_eof = false;
              ready_line_buf.clear();
              ready_sent = false;
              update_log_observer(
                &mut task_screen,
                &mut log,
                &mut current_log,
                p.pid(),
              );
            }
          }
        }
        TaskCmd::Stop => {
          if let Some(p) = process.as_mut() {
            stop_process(p, &stop, task_screen.vt(), &spec).await;
          }
        }
        TaskCmd::Kill => {
          if let Some(p) = process.as_mut() {
            p.kill(stop.kill_group()).await.log_ignore();
          }
        }
        TaskCmd::Msg(msg) => {
          let msg = match msg.downcast::<ProcExited>() {
            Ok(exited) => {
              exit_info = Some(exited.0);
              if let Some(p) = process.as_mut() {
                p.on_exited();
              }
              continue;
            }
            Err(msg) => msg,
          };
          let msg = match msg.downcast::<TaskScreenCmd>() {
            Ok(cmd) => {
              task_screen.handle_cmd(*cmd, &mut screen_effects);
              apply_effects(
                &mut screen_effects,
                &mut process,
                task_screen.vt(),
              )
              .await;
              continue;
            }
            Err(msg) => msg,
          };
          let msg = match msg.downcast::<ProcInput>() {
            Ok(input) => {
              if let Some(p) = process.as_mut() {
                send_key(p, task_screen.vt(), input.0).await;
              }
              continue;
            }
            Err(msg) => msg,
          };
          let msg = match msg.downcast::<DuplicateProc>() {
            Ok(dup) => {
              let new_id = ctx.alloc_id();
              let path = TaskPath::new(format!("/{}", new_id.0)).ok();
              let _ = spawn_proc_task_with_id(
                &ctx,
                new_id,
                path,
                ProcTaskConfig {
                  spec: spec.clone(),
                  stop: stop.clone(),
                  log: None,
                  restart,
                  ready_log: ready_log.clone(),
                  scrollback_len,
                  mouse_scroll_speed,
                  deps: Vec::new(),
                  label: dup.0,
                  tags: Vec::new(),
                  pinned: true,
                },
              );
              continue;
            }
            Err(msg) => msg,
          };
          let _ = msg;
          log::error!("ProcTask received unknown Msg");
        }
      },

      Next::Read(Ok(0)) => stdout_eof = true,
      Next::Read(Ok(n)) => {
        if let Some(pattern) = &ready_log
          && !ready_sent
        {
          ready_sent =
            scan_ready(&ctx, pattern, &mut ready_line_buf, &read_buf[..n]);
        }
        task_screen
          .process(&read_buf[..n], &mut screen_effects)
          .await;
        apply_effects(&mut screen_effects, &mut process, task_screen.vt())
          .await;
      }
      Next::Read(Err(e)) => {
        log::warn!("Process read error: {}", e);
        stdout_eof = true;
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
  log: &mut Option<LogResolver>,
  current: &mut Option<(std::path::PathBuf, u64)>,
  pid: u32,
) {
  let Some(resolve) = log.as_mut() else {
    return;
  };
  let Some(sink) = resolve(pid) else {
    return;
  };
  if let Some((path, _)) = current {
    if *path == sink.path {
      return;
    }
  }
  if let Some((_, id)) = current.take() {
    task_screen.remove_direct_observer(id);
  }
  let path = sink.path.clone();
  let id = task_screen.add_direct_observer(spawn_logger(sink));
  *current = Some((path, id));
}

fn start_instance(
  ctx: &TaskContext,
  spec: &ProcessSpec,
  vt: &SharedVt,
) -> Option<NativeProcess> {
  let size = match vt.read() {
    Ok(parser) => {
      let s = parser.screen().size();
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
  if let Ok(mut parser) = vt.write() {
    parser.reset();
    parser.set_size(size.y, size.x);
  }
  match spawn_native(ctx, spec, size) {
    Ok(process) => {
      ctx.send(KernelCommand::TaskStarted);
      Some(process)
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
) {
  for effect in effects.drain(..) {
    match effect {
      TaskScreenEffect::Write(s) => {
        if let Some(p) = process.as_mut() {
          p.write_all(s.as_bytes()).await.log_ignore();
        }
      }
      TaskScreenEffect::Resize(size) => {
        if let Ok(mut parser) = vt.write() {
          parser.set_size(size.y, size.x);
        }
        if let Some(p) = process.as_mut() {
          p.resize(size).log_ignore();
        }
      }
    }
  }
}

async fn send_key(process: &mut NativeProcess, vt: &SharedVt, key: Key) {
  let application_cursor_keys = vt
    .read()
    .map(|parser| parser.screen().application_cursor())
    .unwrap_or(false);
  let modes = KeyCodeEncodeModes {
    enable_csi_u_key_encoding: true,
    application_cursor_keys,
    newline_mode: false,
  };
  match encode_key(&key, modes) {
    Ok(encoded) => process.write_all(encoded.as_bytes()).await.log_ignore(),
    Err(_) => log::warn!("Failed to encode key: {}", key.spec()),
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
      for key in keys {
        send_key(process, vt, key.clone()).await;
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
      for key in keys {
        send_key(process, vt, key.clone()).await;
      }
    }
    StopSignal::Cmd(shell) => run_stop_cmd(spec, shell.clone()),
  }
}

fn run_stop_cmd(spec: &ProcessSpec, shell: String) {
  let cwd = spec.cwd.clone();
  let env = spec.env.clone();
  tokio::spawn(async move {
    #[cfg(windows)]
    let mut cmd = {
      let mut c = tokio::process::Command::new("pwsh.exe");
      c.arg("-Command").arg(&shell);
      c
    };
    #[cfg(not(windows))]
    let mut cmd = {
      let mut c = tokio::process::Command::new("/bin/sh");
      c.arg("-c").arg(&shell);
      c
    };
    if let Some(cwd) = &cwd {
      cmd.current_dir(cwd);
    }
    for (k, v) in &env {
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
    if let Err(e) = cmd.status().await {
      log::warn!("Stop command failed: {}", e);
    }
  });
}

#[cfg(not(windows))]
#[cfg(test)]
mod tests {
  use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

  use crate::kernel::kernel::Kernel;
  use crate::kernel::kernel_message::{
    KernelCommand, KernelQuery, KernelQueryResponse, TaskContext,
  };
  use crate::kernel::task::TaskId;
  use crate::task::logger::LogSink;

  use super::*;

  async fn resolve(pc: &TaskContext, path: &str) -> TaskId {
    let (tx, rx) = tokio::sync::oneshot::channel();
    pc.send(KernelCommand::Query(
      KernelQuery::ResolvePath(TaskPath::new(path).unwrap()),
      tx,
    ));
    let resp = tokio::time::timeout(Duration::from_secs(1), rx)
      .await
      .expect("timed out resolving path")
      .expect("kernel query channel closed");
    match resp {
      KernelQueryResponse::ResolvedPath(Some(id)) => id,
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

    let path = TaskPath::new("/logged").unwrap();
    let spec = ProcessSpec::from_argv(vec![
      "sh".to_string(),
      "-c".to_string(),
      "printf hello-log".to_string(),
    ]);
    let sink_path = log_path.clone();
    let (id, _) = spawn_proc_task(
      &pc,
      Some(path),
      ProcTaskConfig {
        log: Some(Box::new(move |_pid| {
          Some(LogSink {
            path: sink_path.clone(),
            append: false,
          })
        })),
        ..ProcTaskConfig::new(spec)
      },
    );
    pc.send(KernelCommand::Start(id));

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

    // The SIGCHLD waiter isn't running in unit tests, so the proc never
    // transitions to Exited on its own; remove it explicitly to unblock quit.
    let id = resolve(&pc, "/logged").await;
    pc.send(KernelCommand::RemoveTask(id));
    pc.send(KernelCommand::Quit);
    tokio::time::timeout(Duration::from_secs(2), kernel_task)
      .await
      .expect("timed out waiting for kernel to quit")
      .unwrap();

    let _ = std::fs::remove_file(&log_path);
  }

  #[tokio::test]
  async fn log_path_is_resolved_with_real_pid() {
    use std::sync::{Arc, Mutex};

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
    let seen_pid = Arc::new(Mutex::new(None::<u32>));
    let cap = seen_pid.clone();
    let log_dir = dir.clone();
    let (id, _) = spawn_proc_task(
      &pc,
      Some(TaskPath::new("/pidlog").unwrap()),
      ProcTaskConfig {
        log: Some(Box::new(move |pid| {
          *cap.lock().unwrap() = Some(pid);
          Some(LogSink {
            path: log_dir.join(format!("{pid}.log")),
            append: false,
          })
        })),
        ..ProcTaskConfig::new(spec)
      },
    );
    pc.send(KernelCommand::Start(id));

    let kernel_task = tokio::spawn(kernel.run());

    let deadline = Instant::now() + Duration::from_secs(2);
    let pid = loop {
      if let Some(pid) = *seen_pid.lock().unwrap() {
        let log = dir.join(format!("{pid}.log"));
        if std::fs::read_to_string(&log).is_ok_and(|c| c.contains("hi")) {
          break pid;
        }
      }
      assert!(Instant::now() < deadline, "pid-named log never got output");
      tokio::time::sleep(Duration::from_millis(10)).await;
    };
    assert_ne!(pid, 0, "resolver should receive a real pid");

    let id = resolve(&pc, "/pidlog").await;
    pc.send(KernelCommand::RemoveTask(id));
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

    let path = TaskPath::new("/sleeper").unwrap();
    let spec = ProcessSpec::from_argv(vec![
      "sh".to_string(),
      "-c".to_string(),
      "sleep 100".to_string(),
    ]);
    let (id, _) = spawn_proc_task(
      &pc,
      Some(path),
      ProcTaskConfig {
        stop: StopSignal::Cmd(format!("printf done > {}", marker.display())),
        ..ProcTaskConfig::new(spec)
      },
    );
    pc.send(KernelCommand::Start(id));

    let kernel_task = tokio::spawn(kernel.run());

    let id = resolve(&pc, "/sleeper").await;
    pc.send(KernelCommand::Stop(id));

    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
      if marker.exists() {
        break;
      }
      assert!(Instant::now() < deadline, "stop command never ran");
      tokio::time::sleep(Duration::from_millis(10)).await;
    }

    pc.send(KernelCommand::Kill(id));
    pc.send(KernelCommand::RemoveTask(id));
    pc.send(KernelCommand::Quit);
    tokio::time::timeout(Duration::from_secs(2), kernel_task)
      .await
      .expect("timed out waiting for kernel to quit")
      .unwrap();

    let _ = std::fs::remove_file(&marker);
  }
}

fn spawn_native(
  ctx: &TaskContext,
  spec: &ProcessSpec,
  size: Winsize,
) -> anyhow::Result<NativeProcess> {
  let exit_ctx = ctx.clone();

  #[cfg(unix)]
  {
    Ok(crate::process::unix_process::UnixProcess::spawn(
      ctx.task_id,
      spec,
      size,
      Box::new(move |wait_status| {
        let info = ExitInfo {
          code: wait_status.exit_status().map(|code| code as i32),
          signal: wait_status.terminating_signal().map(|sig| sig as i32),
        };
        exit_ctx.send_self_custom(ProcExited(info));
      }),
    )?)
  }

  #[cfg(windows)]
  {
    use anyhow::Context as _;
    crate::process::win_process::WinProcess::spawn(
      ctx.task_id,
      spec,
      size,
      Box::new(move |exit_code| {
        let info = match exit_code {
          Some(code) => ExitInfo::code(code as i32),
          None => ExitInfo::error(),
        };
        exit_ctx.send_self_custom(ProcExited(info));
      }),
    )
    .context("WinProcess::spawn")
  }
}
