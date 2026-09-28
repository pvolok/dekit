use std::any::Any;
use std::fmt;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::UnboundedSender;

use super::kernel_message::{SharedVt, TaskSelector};
use super::task_key::TaskSpaceId;
use super::task_path::TaskPath;

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
  Start,
  Stop,
  Kill,
  /// Register a copy of this task, if the task kind supports it.
  Duplicate(Option<String>),
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

impl fmt::Debug for TaskCmd {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      TaskCmd::Start => write!(f, "Start"),
      TaskCmd::Stop => write!(f, "Stop"),
      TaskCmd::Kill => write!(f, "Kill"),
      TaskCmd::Duplicate(label) => write!(f, "Duplicate({:?})", label),
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

pub struct TaskNotification {
  pub from: TaskId,
  pub notify: TaskNotify,
}

#[derive(Clone)]
pub enum TaskNotify {
  Added {
    path: Option<TaskPath>,
    label: Option<String>,
    kind: TaskKind,
    state: TaskState,
    vt: Option<SharedVt>,
  },
  StateChanged(TaskState),
  Removed,
  LabelChanged(Option<String>),
}

impl fmt::Debug for TaskNotify {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      TaskNotify::Added {
        path, label, state, ..
      } => {
        write!(f, "Added({:?}, {:?}, {:?})", path, label, state)
      }
      TaskNotify::StateChanged(state) => write!(f, "StateChanged({:?})", state),
      TaskNotify::Removed => write!(f, "Removed"),
      TaskNotify::LabelChanged(label) => {
        write!(f, "LabelChanged({:?})", label)
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
      TaskCmd::Start => fx.started(),
      TaskCmd::Stop | TaskCmd::Kill => fx.stopped(ExitInfo::code(0)),
      TaskCmd::Duplicate(_)
      | TaskCmd::Msg(_)
      | TaskCmd::Freeze(_)
      | TaskCmd::Thaw => (),
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

  pub kind: TaskKind,
  pub ready: ReadyMode,
  pub restart: RestartMode,
  pub stop_timeout: Duration,

  pub space: TaskSpaceId,
  pub path: Option<TaskPath>,
  pub label: Option<String>,
  pub vt: Option<SharedVt>,
  pub tags: Vec<String>,
}

impl TaskHandle {
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
  pub vt: Option<SharedVt>,
  pub tags: Vec<String>,
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
      vt: None,
      tags: Vec::new(),
    }
  }
}
