use std::{
  any::Any,
  fmt::{self, Debug},
  ops::Deref,
  sync::{Arc, RwLock, atomic::AtomicUsize},
};

use tokio::sync::mpsc::UnboundedSender;

use crate::term::Screen;

use super::sub_trie::SubMode;
use super::task::{ExitInfo, Task, TaskCmd, TaskDef, TaskId, TaskState};
use super::task_key::{TaskKey, TaskSpaceId};
use super::task_path::TaskPath;
use crate::upgrade::snapshot::TaskKind as TaskKindSnapshot;

pub struct KernelMessage {
  pub from: TaskId,
  pub command: KernelCommand,
}

pub struct TaskRegistration {
  pub task_id: TaskId,
  pub def: TaskDef,
  pub factory: Box<dyn FnOnce(TaskContext) -> Box<dyn Task> + Send>,
}

impl TaskRegistration {
  pub fn async_task<F, Fut>(task_id: TaskId, def: TaskDef, f: F) -> Self
  where
    F: FnOnce(TaskContext, tokio::sync::mpsc::UnboundedReceiver<TaskCmd>) -> Fut
      + Send
      + 'static,
    Fut: std::future::Future<Output = ()> + Send + 'static,
  {
    use super::task::ChannelTask;
    Self {
      task_id,
      def,
      factory: Box::new(|ctx| {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        tokio::spawn(f(ctx, rx));
        Box::new(ChannelTask::new(tx))
      }),
    }
  }
}

pub type Ack = Option<tokio::sync::oneshot::Sender<usize>>;

pub enum KernelCommand {
  Quit,

  /// Registration is atomic: deps are resolved, the path claimed, and the
  /// task inserted in one dispatch, or nothing happens.
  RegisterTask(
    TaskRegistration,
    tokio::sync::oneshot::Sender<Result<(), RegisterError>>,
  ),

  /// Selector commands resolve the selector and act on the matches in the
  /// same dispatch, so no other message can interleave between the two.
  /// The ack is answered in that dispatch with the matched-task count.
  Start(TaskSelector, Ack),
  Stop(TaskSelector, Ack),
  Kill(TaskSelector, Ack),
  Restart(TaskSelector, Ack),
  ForceRestart(TaskSelector, Ack),
  Down(TaskSelector, Ack),
  Veto(TaskSelector, Ack),
  /// Total: removes matching tasks in any state, killing running ones.
  Remove(TaskSelector, Ack),
  SetLabel(TaskSelector, Option<String>, Ack),
  /// Asks each matching task to register a copy of itself.
  Duplicate(TaskSelector, Option<String>, Ack),

  TaskMsg(TaskId, Box<dyn Any + Send>),

  Query(
    KernelQuery,
    tokio::sync::oneshot::Sender<KernelQueryResponse>,
  ),

  SubscribePath(TaskKey, SubMode),
  UnsubscribePath(TaskKey, SubMode),
  /// Sends `true`/`false` whenever the selected set gains its first active
  /// task or loses its last one.
  WatchActive(TaskSelector, UnboundedSender<bool>),

  // Task reporting
  TaskStarted,
  TaskReady,
  TaskStopped(ExitInfo),

  /// A time limit set on the task's current state ran out (stop grace,
  /// backoff delay). The epoch says which state it was set for, so a
  /// timeout from an earlier state is ignored.
  StateTimeout(TaskId, u64),

  /// Freeze every task and answer with the graph once all have reported
  /// `TaskFrozen`, or with why the runner cannot be frozen. While frozen
  /// the kernel defers intent and drives nothing; task reports and queries
  /// still go through. Frozen until `Thaw`, even after an error.
  Freeze(tokio::sync::oneshot::Sender<Result<KernelSnapshot, String>>),
  /// A task's answer to `TaskCmd::Freeze`, with that freeze's number.
  TaskFrozen(u64, TaskKindSnapshot),
  /// Resumes driving; what the freeze deferred is then handled one message
  /// at a time, ahead of anything newer.
  Thaw,
}

/// The kernel's part of an upgrade snapshot.
#[derive(Debug)]
pub struct KernelSnapshot {
  pub next_task_id: usize,
  pub tasks: Vec<crate::upgrade::snapshot::Task>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum SpaceSelector {
  One(TaskSpaceId),
  Any,
}

impl SpaceSelector {
  pub fn default_space() -> Self {
    SpaceSelector::One(TaskSpaceId::default_space())
  }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TaskSelector {
  Id(TaskId),
  /// Tasks whose path matches the glob; `**` is every task with a path.
  Glob(SpaceSelector, String),
  /// Tasks carrying the tag.
  Tag(SpaceSelector, String),
}

impl TaskSelector {
  pub fn all() -> Self {
    TaskSelector::Glob(SpaceSelector::default_space(), "**".to_string())
  }
}

impl fmt::Display for TaskSelector {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    let space = |f: &mut fmt::Formatter<'_>, space: &SpaceSelector| match space
    {
      SpaceSelector::One(space) if space.is_default() => Ok(()),
      SpaceSelector::One(space) => write!(f, "@{}/", space),
      SpaceSelector::Any => f.write_str("@*/"),
    };
    match self {
      TaskSelector::Id(id) => write!(f, "{{id: {}}}", id.0),
      TaskSelector::Glob(s, pattern) => {
        space(f, s)?;
        f.write_str(pattern)
      }
      TaskSelector::Tag(s, tag) => {
        space(f, s)?;
        write!(f, "+{}", tag)
      }
    }
  }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RegisterError {
  IdTaken,
  ReservedSpace(TaskSpaceId),
  PathTaken(TaskKey),
  /// A dep selector matched no task.
  MissingDep(TaskSelector),
}

impl fmt::Display for RegisterError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      RegisterError::IdTaken => f.write_str("task id is already registered"),
      RegisterError::ReservedSpace(space) => {
        write!(f, "space '@{}' is reserved", space)
      }
      RegisterError::PathTaken(key) => {
        write!(f, "a task already exists at '{}'", key)
      }
      RegisterError::MissingDep(selector) => {
        write!(f, "dep '{}' matches no task", selector)
      }
    }
  }
}

impl std::error::Error for RegisterError {}

pub enum KernelQuery {
  ListTasks(TaskSelector),
  /// Explain why matching tasks are (not) running.
  Explain(TaskSelector),
}

pub enum KernelQueryResponse {
  TaskList(Vec<TaskInfo>),
  Explain(Vec<TaskExplain>),
}

#[derive(Clone, Debug)]
pub struct TaskInfo {
  pub id: TaskId,
  pub space: TaskSpaceId,
  pub path: Option<TaskPath>,
  pub label: Option<String>,
  pub state: TaskState,
  pub vt: Option<SharedVt>,
}

impl TaskInfo {
  /// `@space/path`, or `<task:id>` for a task without a path.
  pub fn name(&self) -> String {
    task_name(self.id, &self.space, self.path.as_ref())
  }
}

pub fn task_name(
  id: TaskId,
  space: &TaskSpaceId,
  path: Option<&TaskPath>,
) -> String {
  match path {
    Some(path) => TaskKey::new(space.clone(), path.clone()).to_string(),
    None => format!("<task:{}>", id.0),
  }
}

#[derive(Clone, Debug)]
pub struct TaskExplain {
  pub id: TaskId,
  pub name: String,
  pub state: TaskState,
  pub wanted: bool,
  /// Wanted and every dependency transitively supported and satisfied;
  /// false on a wanted task means it is blocked by a dep below.
  pub supported: bool,
  pub vetoed: bool,
  pub pinned: bool,
  pub required_by: Vec<String>,
  pub deps: Vec<DepExplain>,
  pub attempts: u32,
}

#[derive(Clone, Debug)]
pub struct DepExplain {
  pub name: String,
  pub state: TaskState,
  pub wanted: bool,
  pub satisfied: bool,
}

#[derive(Clone)]
pub struct SharedVt(Arc<RwLock<Screen>>);

impl SharedVt {
  pub fn new(screen: Screen) -> Self {
    SharedVt(Arc::new(RwLock::new(screen)))
  }
}

impl Debug for SharedVt {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_tuple("SharedVt").finish()
  }
}

impl Deref for SharedVt {
  type Target = Arc<RwLock<Screen>>;

  fn deref(&self) -> &Self::Target {
    &self.0
  }
}

#[derive(Clone)]
pub struct TaskContext {
  next_task_id: Arc<AtomicUsize>,
  sender: UnboundedSender<KernelMessage>,
  pub task_id: TaskId,
}

impl TaskContext {
  pub fn new(
    next_task_id: Arc<AtomicUsize>,
    task_id: TaskId,
    sender: UnboundedSender<KernelMessage>,
  ) -> Self {
    Self {
      next_task_id,
      sender,
      task_id,
    }
  }

  #[cfg(test)]
  pub fn sender_for_tests(&self) -> UnboundedSender<KernelMessage> {
    self.sender.clone()
  }

  pub fn send(&self, command: KernelCommand) {
    if let Err(_err) = self.sender.send(KernelMessage {
      from: self.task_id,
      command,
    }) {
      log::debug!(
        "Failed to send kernel message (task_id: {}). Channel is closed.",
        self.task_id.0,
      );
    }
  }

  pub fn send_msg<T: Any + Send + 'static>(&self, to: TaskId, msg: T) {
    self.send(KernelCommand::TaskMsg(to, Box::new(msg)));
  }

  pub fn alloc_id(&self) -> TaskId {
    TaskId(
      self
        .next_task_id
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed),
    )
  }

  pub fn register(
    &self,
    def: TaskDef,
    factory: Box<dyn FnOnce(TaskContext) -> Box<dyn Task> + Send>,
  ) -> TaskId {
    let task_id = self.alloc_id();
    self.register_with_id(task_id, def, factory)
  }

  pub fn register_with_id(
    &self,
    task_id: TaskId,
    def: TaskDef,
    factory: Box<dyn FnOnce(TaskContext) -> Box<dyn Task> + Send>,
  ) -> TaskId {
    let _ = self.register_task(TaskRegistration {
      task_id,
      def,
      factory,
    });
    task_id
  }

  pub fn spawn_async<F, Fut>(&self, def: TaskDef, f: F) -> TaskId
  where
    F: FnOnce(TaskContext, tokio::sync::mpsc::UnboundedReceiver<TaskCmd>) -> Fut
      + Send
      + 'static,
    Fut: std::future::Future<Output = ()> + Send + 'static,
  {
    let task_id = self.alloc_id();
    let _ = self.spawn_async_with_id(task_id, def, f);
    task_id
  }

  pub fn spawn_async_with_id<F, Fut>(
    &self,
    task_id: TaskId,
    def: TaskDef,
    f: F,
  ) -> tokio::sync::oneshot::Receiver<Result<(), RegisterError>>
  where
    F: FnOnce(TaskContext, tokio::sync::mpsc::UnboundedReceiver<TaskCmd>) -> Fut
      + Send
      + 'static,
    Fut: std::future::Future<Output = ()> + Send + 'static,
  {
    let registration = TaskRegistration::async_task(task_id, def, f);
    self.register_task(registration)
  }

  pub fn register_task(
    &self,
    registration: TaskRegistration,
  ) -> tokio::sync::oneshot::Receiver<Result<(), RegisterError>> {
    let (ack_tx, ack_rx) = tokio::sync::oneshot::channel();
    self.send(KernelCommand::RegisterTask(registration, ack_tx));
    ack_rx
  }

  pub fn subscribe_path(&self, key: TaskKey, mode: SubMode) {
    self.send(KernelCommand::SubscribePath(key, mode));
  }

  pub fn watch_active(
    &self,
    selector: TaskSelector,
  ) -> tokio::sync::mpsc::UnboundedReceiver<bool> {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    self.send(KernelCommand::WatchActive(selector, tx));
    rx
  }

  pub fn unsubscribe_path(&self, key: TaskKey, mode: SubMode) {
    self.send(KernelCommand::UnsubscribePath(key, mode));
  }

  pub fn query(
    &self,
    query: KernelQuery,
  ) -> tokio::sync::oneshot::Receiver<KernelQueryResponse> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    self.send(KernelCommand::Query(query, tx));
    rx
  }

  pub fn get_task_sender(&self, target_id: TaskId) -> TaskSender {
    TaskSender {
      task_id: target_id,
      from_id: self.task_id,
      sender: self.sender.clone(),
    }
  }
}

#[derive(Clone)]
pub struct TaskSender {
  pub task_id: TaskId,
  pub from_id: TaskId,
  sender: UnboundedSender<KernelMessage>,
}

impl TaskSender {
  pub fn send<T: Any + Send + 'static>(&self, msg: T) {
    let r = self.sender.send(KernelMessage {
      from: self.from_id,
      command: KernelCommand::TaskMsg(self.task_id, Box::new(msg)),
    });
    if let Err(_err) = r {
      log::debug!(
        "TaskSender.send() to closed channel. from_id:{} task_id:{}",
        self.from_id.0,
        self.task_id.0
      );
    }
  }
}
