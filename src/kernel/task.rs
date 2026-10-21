use std::any::Any;
use std::fmt;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::UnboundedSender;

use super::kernel_message::{MakeCopy, Place, ScreenHandle, TaskSelector};
use super::task_key::TaskSpaceId;
use super::task_path::TaskPath;
use crate::spec::{Source, TaskSpec};

#[derive(
  Clone,
  Copy,
  Debug,
  Deserialize,
  Eq,
  Hash,
  Ord,
  PartialEq,
  PartialOrd,
  Serialize,
)]
pub struct TaskId(pub usize);

pub const INIT_TASK_ID: TaskId = TaskId(0);

/// How long a stopping task may take before it is hard-killed, unless the
/// task sets its own.
pub const STOP_TIMEOUT: Duration = Duration::from_secs(10);

pub trait Task: Send + 'static {
  fn handle_cmd(&mut self, cmd: TaskCmd, fx: &mut Effects);
}

pub enum TaskEffect {
  Started,
  Ready,
  Stopped(ExitInfo),
}

pub struct Effects(Vec<TaskEffect>);

impl Effects {
  pub fn new() -> Self {
    Self(Vec::new())
  }

  pub fn started(&mut self) {
    self.0.push(TaskEffect::Started);
  }

  pub fn ready(&mut self) {
    self.0.push(TaskEffect::Ready);
  }

  pub fn stopped(&mut self, info: ExitInfo) {
    self.0.push(TaskEffect::Stopped(info));
  }

  pub fn drain(&mut self) -> std::vec::Drain<'_, TaskEffect> {
    self.0.drain(..)
  }
}

pub enum TaskCmd {
  /// Start a run; for a task made from a definition, of that definition.
  Start(Option<Box<Run>>),
  Stop,
  Kill,
  Msg(Box<dyn Any + Send>),
  /// Stop all I/O and answer with `KernelCommand::TaskFrozen`, echoing
  /// this freeze's number, with the task's state. No other command
  /// arrives until `Thaw`, or the process is replaced.
  Freeze(u64),
  Thaw,
}

impl TaskCmd {
  pub fn msg(m: impl Any + Send + 'static) -> Self {
    TaskCmd::Msg(Box::new(m))
  }
}

/// What a run of a task made from a definition starts from: the
/// definition as it is now, and where the task is listed, which names its
/// log.
#[derive(Clone, Debug)]
pub struct Run {
  pub spec: TaskSpec,
  pub path: Option<TaskPath>,
}

/// What a task's next run starts from, when its definition changed while
/// a run was under way: a run goes on as it started.
#[derive(Clone, Debug)]
pub struct NextRun {
  pub kind: TaskKind,
  pub ready: ReadyMode,
  pub stop_timeout: Duration,
  pub spec: Option<TaskSpec>,
}

impl fmt::Debug for TaskCmd {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      TaskCmd::Start(_) => write!(f, "Start"),
      TaskCmd::Stop => write!(f, "Stop"),
      TaskCmd::Kill => write!(f, "Kill"),
      TaskCmd::Msg(_) => write!(f, "Msg(...)"),
      TaskCmd::Freeze(number) => write!(f, "Freeze({number})"),
      TaskCmd::Thaw => write!(f, "Thaw"),
    }
  }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExitInfo {
  pub code: Option<i32>,
  pub signal: Option<i32>,
  /// Stopped because it was not ready within its ready timeout.
  pub ready_timeout: bool,
}

impl ExitInfo {
  pub fn code(code: i32) -> Self {
    Self {
      code: Some(code),
      signal: None,
      ready_timeout: false,
    }
  }

  pub fn signal(signal: i32) -> Self {
    Self {
      code: None,
      signal: Some(signal),
      ready_timeout: false,
    }
  }

  /// The task could not run at all (e.g. spawn failure).
  pub fn error() -> Self {
    Self {
      code: None,
      signal: None,
      ready_timeout: false,
    }
  }

  pub fn success(&self) -> bool {
    self.code == Some(0) && !self.ready_timeout
  }
}

impl fmt::Display for ExitInfo {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    if self.ready_timeout {
      return write!(f, "ready-timeout");
    }
    match (self.code, self.signal) {
      (Some(code), _) => write!(f, "exited:{}", code),
      (None, Some(signal)) => write!(f, "signal:{}", signal),
      (None, None) => write!(f, "error"),
    }
  }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TaskState {
  Idle,
  Starting,
  Running,
  Ready,
  Stopping,
  /// Ended with this exit; waiting out the restart delay.
  Backoff(ExitInfo),
  /// Ran to successful completion (jobs). Satisfies dependents.
  Done(ExitInfo),
  /// Exited and will not be brought back automatically.
  Exited(ExitInfo),
}

impl TaskState {
  /// The task occupies its slot: it must wind down before its deps may stop.
  pub fn is_active(&self) -> bool {
    match self {
      TaskState::Starting
      | TaskState::Running
      | TaskState::Ready
      | TaskState::Stopping => true,
      TaskState::Idle
      | TaskState::Backoff(_)
      | TaskState::Done(_)
      | TaskState::Exited(_) => false,
    }
  }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum TaskKind {
  /// Long-running; satisfies dependents while `Ready`.
  #[default]
  Service,
  /// Run-to-completion; satisfies dependents once `Done`.
  Job,
}

impl TaskKind {
  /// A job is ready when it is done: it takes no ready check.
  pub fn takes_ready(self) -> bool {
    self == TaskKind::Service
  }

  /// A job always restarted would never be done.
  pub fn takes_restart(self, restart: RestartMode) -> bool {
    self == TaskKind::Service || restart != RestartMode::Always
  }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReadyMode {
  /// Ready as soon as the task reports started.
  Immediate,
  /// Ready only when the task reports it (readiness probe); not ready
  /// `timeout` after starting fails the start.
  Reported { timeout: Option<Duration> },
}

impl ReadyMode {
  pub fn timeout(self) -> Option<Duration> {
    match self {
      ReadyMode::Immediate => None,
      ReadyMode::Reported { timeout } => timeout,
    }
  }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RestartMode {
  Never,
  OnFailure,
  Always,
}

impl RestartMode {
  pub const ALL: [RestartMode; 3] = [
    RestartMode::Never,
    RestartMode::OnFailure,
    RestartMode::Always,
  ];

  /// As the config writes it.
  pub fn name(self) -> &'static str {
    match self {
      RestartMode::Never => "never",
      RestartMode::OnFailure => "on-failure",
      RestartMode::Always => "always",
    }
  }

  pub fn from_name(name: &str) -> Option<RestartMode> {
    RestartMode::ALL
      .into_iter()
      .find(|mode| mode.name() == name)
  }
}

pub struct TaskNotification {
  pub from: TaskId,
  pub notify: TaskNotify,
}

#[derive(Clone)]
pub enum TaskNotify {
  Added {
    path: Option<TaskPath>,
    label: Option<String>,
    /// The task listed just before this one.
    after: Option<TaskId>,
    kind: TaskKind,
    state: TaskState,
    vt: Option<ScreenHandle>,
  },
  /// A folder, listed right after `after`; `from` is its id.
  FolderAdded {
    path: TaskPath,
    after: Option<TaskId>,
    collapsed: bool,
  },
  StateChanged(TaskState),
  /// The task or folder is gone.
  Removed,
  /// The label or the kind changed: renamed, or redefined.
  Changed {
    label: Option<String>,
    kind: TaskKind,
  },
  /// Listed elsewhere, at this path, right after `after`. A move of
  /// several tasks and folders notifies them in their new list order.
  Moved {
    path: Option<TaskPath>,
    after: Option<TaskId>,
  },
}

impl fmt::Debug for TaskNotify {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      TaskNotify::Added {
        path, label, state, ..
      } => {
        write!(f, "Added({:?}, {:?}, {:?})", path, label, state)
      }
      TaskNotify::FolderAdded { path, .. } => {
        write!(f, "FolderAdded({:?})", path)
      }
      TaskNotify::StateChanged(state) => write!(f, "StateChanged({:?})", state),
      TaskNotify::Removed => write!(f, "Removed"),
      TaskNotify::Changed { label, kind } => {
        write!(f, "Changed({:?}, {:?})", label, kind)
      }
      TaskNotify::Moved { path, after } => {
        write!(f, "Moved({:?}, {:?})", path, after)
      }
    }
  }
}

pub struct ChannelTask {
  sender: UnboundedSender<TaskCmd>,
}

impl ChannelTask {
  pub fn new(sender: UnboundedSender<TaskCmd>) -> Self {
    Self { sender }
  }
}

impl Task for ChannelTask {
  fn handle_cmd(&mut self, cmd: TaskCmd, fx: &mut Effects) {
    // A closed channel means the driving future is gone; report the task
    // dead so it cannot wedge in an active state.
    if self.sender.send(cmd).is_err() {
      fx.stopped(ExitInfo::error());
    }
  }
}

/// A task with no process of its own; it exists to hold edges.
#[cfg(test)]
pub struct TargetTask;

#[cfg(test)]
impl Task for TargetTask {
  fn handle_cmd(&mut self, cmd: TaskCmd, fx: &mut Effects) {
    match cmd {
      TaskCmd::Start(_) => fx.started(),
      TaskCmd::Stop | TaskCmd::Kill => fx.stopped(ExitInfo::code(0)),
      TaskCmd::Msg(_) | TaskCmd::Freeze(_) | TaskCmd::Thaw => (),
    }
  }
}

pub struct TaskHandle {
  pub task: Box<dyn Task>,

  pub state: TaskState,
  /// Bumped on every state change (and hard kill); state timeouts from an
  /// earlier epoch are ignored.
  pub epoch: u64,
  /// Vetoed: excluded from want-propagation until demanded again
  /// (a direct start, or a start of a dependent pulling this task).
  pub vetoed: bool,
  /// A hard kill was sent; if the task is still stopping when its timeout
  /// runs out again, the kernel gives up waiting.
  pub killed: bool,
  pub attempts: u32,
  pub last_start: Option<Instant>,
  /// When the timer on the current state (stop grace, kill wait, backoff
  /// delay, ready timeout) runs out; cleared when the state changes or
  /// the timer runs out.
  pub deadline: Option<Instant>,

  /// Cached reconciler state, maintained incrementally.
  /// `wanted`: reachable from a pin through non-vetoed nodes.
  pub wanted: bool,
  /// `supported`: wanted, and every dependency is itself supported and
  /// currently satisfied.
  pub supported: bool,
  /// Count of requirers that are wanted; a pin counts as one.
  pub wanted_parents: u32,
  /// Count of active tasks that require this one (the shutdown gate).
  pub active_dependents: u32,

  /// Not ready within the ready timeout: the stop under way lands as a
  /// failed exit. Only set in `Stopping`.
  pub start_failed: bool,
  /// Started when the tasks were last saved: `up` starts it again. Only
  /// on an unpinned task; any verb that pins or unpins it clears it.
  pub saved_pin: bool,

  /// What its current or last run is, how it gets ready, and how long
  /// its stop may take.
  pub kind: TaskKind,
  pub ready: ReadyMode,
  pub stop_timeout: Duration,
  /// What the next run starts from, when it is not what the last one did.
  pub next: Option<Box<NextRun>>,
  pub restart: RestartMode,

  pub space: TaskSpaceId,
  pub path: Option<TaskPath>,
  pub label: Option<String>,
  pub vt: Option<ScreenHandle>,
  pub tags: Vec<String>,
  /// The definition as written that its current or last run started
  /// from, without its deps: they are the task's edges.
  pub spec: Option<TaskSpec>,
  /// The config entry it is written in; none for the runner's own.
  pub origin: Option<Source>,
  /// Makes a copy of it from its definition.
  pub make_copy: Option<MakeCopy>,
}

impl TaskHandle {
  /// Runs start from `next` from now on. Returns whether its kind
  /// changed.
  pub fn adopt(&mut self, next: NextRun) -> bool {
    let changed = self.kind != next.kind;
    self.kind = next.kind;
    self.ready = next.ready;
    self.stop_timeout = next.stop_timeout;
    self.spec = next.spec;
    changed
  }

  /// The definition as written that its next run starts from.
  pub fn def_spec(&self) -> Option<&TaskSpec> {
    match &self.next {
      Some(next) => next.spec.as_ref(),
      None => self.spec.as_ref(),
    }
  }

  /// Whether this task currently satisfies its dependents.
  pub fn is_satisfied(&self) -> bool {
    match self.kind {
      TaskKind::Service => self.state == TaskState::Ready,
      TaskKind::Job => match self.state {
        TaskState::Done(_) => true,
        TaskState::Idle
        | TaskState::Starting
        | TaskState::Running
        | TaskState::Ready
        | TaskState::Stopping
        | TaskState::Backoff(_)
        | TaskState::Exited(_) => false,
      },
    }
  }

  /// Where an exit lands, as the task is configured: a job's success is
  /// done; otherwise it backs off if the restart mode retries it.
  pub fn exit_state(&self, info: ExitInfo) -> TaskState {
    if self.kind == TaskKind::Job && info.success() {
      return TaskState::Done(info);
    }
    let retry = match self.restart {
      RestartMode::Never => false,
      RestartMode::OnFailure => !info.success(),
      RestartMode::Always => true,
    };
    if retry {
      TaskState::Backoff(info)
    } else {
      TaskState::Exited(info)
    }
  }
}

pub struct TaskDef {
  pub kind: TaskKind,
  pub ready: ReadyMode,
  pub restart: RestartMode,
  /// How long a stop may take before the hard kill.
  pub stop_timeout: Duration,
  /// Resolved at registration; each selector must match at least one
  /// registered task, so the graph stays acyclic by construction.
  pub deps: Vec<TaskSelector>,
  /// Pinned at registration, so it starts at once.
  pub pinned: bool,
  /// Registered unpinned, and `up` pins it (see `TaskHandle::saved_pin`).
  /// Never together with `pinned`.
  pub saved_pin: bool,
  pub space: TaskSpaceId,
  pub path: Option<TaskPath>,
  pub label: Option<String>,
  /// Where it is listed beside the tasks at its path's parent: an
  /// `After` path elsewhere, or `Keep`, is last.
  pub place: Place,
  pub vt: Option<ScreenHandle>,
  pub tags: Vec<String>,
  /// The definition as written: the kernel keeps it but for its deps.
  pub spec: Option<TaskSpec>,
  pub origin: Option<Source>,
  /// What the next run starts from, for a task restored with a run that
  /// started from an older definition (`kind`, `ready`, `stop_timeout`
  /// and `spec` are that run's then).
  pub next: Option<Box<NextRun>>,
  pub make_copy: Option<MakeCopy>,
}

impl Default for TaskDef {
  fn default() -> Self {
    Self {
      kind: TaskKind::Service,
      ready: ReadyMode::Immediate,
      restart: RestartMode::Never,
      stop_timeout: STOP_TIMEOUT,
      deps: Vec::new(),
      pinned: false,
      saved_pin: false,
      space: TaskSpaceId::default_space(),
      path: None,
      label: None,
      place: Place::Last,
      vt: None,
      tags: Vec::new(),
      spec: None,
      origin: None,
      next: None,
      make_copy: None,
    }
  }
}
