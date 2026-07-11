use std::any::Any;
use std::fmt;
use std::time::Instant;

use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::UnboundedSender;

use super::kernel_message::SharedVt;
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
  Msg(Box<dyn Any + Send>),
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
      TaskCmd::Msg(_) => write!(f, "Msg(...)"),
    }
  }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExitInfo {
  pub code: Option<i32>,
  pub signal: Option<i32>,
}

impl ExitInfo {
  pub fn code(code: i32) -> Self {
    Self {
      code: Some(code),
      signal: None,
    }
  }

  pub fn signal(signal: i32) -> Self {
    Self {
      code: None,
      signal: Some(signal),
    }
  }

  /// The task could not run at all (e.g. spawn failure).
  pub fn error() -> Self {
    Self {
      code: None,
      signal: None,
    }
  }

  pub fn success(&self) -> bool {
    self.code == Some(0)
  }
}

impl fmt::Display for ExitInfo {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
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
  /// Crashed; waiting out the restart delay.
  Backoff,
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
      | TaskState::Backoff
      | TaskState::Done(_)
      | TaskState::Exited(_) => false,
    }
  }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TaskKind {
  /// Long-running; satisfies dependents while `Ready`.
  Service,
  /// Run-to-completion; satisfies dependents once `Done`.
  Job,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReadyMode {
  /// Ready as soon as the task reports started.
  Immediate,
  /// Ready only when the task reports it (readiness probe).
  Reported,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RestartMode {
  Never,
  OnFailure,
  Always,
}

pub struct TaskNotification {
  pub from: TaskId,
  pub from_path: Option<TaskPath>,
  pub notify: TaskNotify,
}

#[derive(Clone)]
pub enum TaskNotify {
  Added {
    path: Option<TaskPath>,
    label: Option<String>,
    state: TaskState,
    vt: Option<SharedVt>,
  },
  StateChanged(TaskState),
  Removed,
  PathChanged(Option<TaskPath>, Option<TaskPath>),
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
      TaskNotify::PathChanged(old, new) => {
        write!(f, "PathChanged({:?}, {:?})", old, new)
      }
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
pub struct TargetTask;

impl Task for TargetTask {
  fn handle_cmd(&mut self, cmd: TaskCmd, fx: &mut Effects) {
    match cmd {
      TaskCmd::Start => fx.started(),
      TaskCmd::Stop | TaskCmd::Kill => fx.stopped(ExitInfo::code(0)),
      TaskCmd::Msg(_) => (),
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

  pub kind: TaskKind,
  pub ready: ReadyMode,
  pub restart: RestartMode,

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
        | TaskState::Backoff
        | TaskState::Exited(_) => false,
      },
    }
  }
}

pub struct TaskDef {
  pub kind: TaskKind,
  pub ready: ReadyMode,
  pub restart: RestartMode,
  pub deps: Vec<TaskId>,
  /// aka autostart
  pub pinned: bool,
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
      deps: Vec::new(),
      pinned: false,
      path: None,
      label: None,
      vt: None,
      tags: Vec::new(),
    }
  }
}
