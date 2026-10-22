use std::{
  any::Any,
  fmt::{self, Debug},
  sync::{Arc, atomic::AtomicUsize},
};

use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::UnboundedSender;

pub use crate::term::ScreenHandle;

use super::sub_trie::SubMode;
use super::task::{
  ExitInfo, NextRun, RestartMode, StateReason, Task, TaskCmd, TaskDef, TaskId,
  TaskKind, TaskState,
};
use super::task_key::{TaskKey, TaskSpaceId};
use super::task_path::TaskPath;
use crate::spec::{Source, TaskSpec};
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

/// What a batch registers: a task, or a folder.
pub enum Registration {
  Task(TaskRegistration),
  Folder(FolderDef),
}

impl From<TaskRegistration> for Registration {
  fn from(registration: TaskRegistration) -> Self {
    Registration::Task(registration)
  }
}

/// A folder: a node in the task tree that holds what is under it and
/// stays while it is empty. It runs nothing, has no state, and is no
/// task's dep; its id is from the tasks' ids.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FolderDef {
  pub id: TaskId,
  pub space: TaskSpaceId,
  pub path: TaskPath,
  /// Where it is listed beside the paths at its path's parent, as for a
  /// task; a group there becomes it where it is.
  pub place: Place,
  pub settings: FolderSettings,
}

/// What a folder's entry says.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct FolderSettings {
  /// Collapsed when the console first lists it.
  pub collapsed: bool,
  /// The config entry it is written in; none for the runner's own.
  pub origin: Option<Source>,
}

/// A folder as it is now.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FolderInfo {
  pub id: TaskId,
  pub space: TaskSpaceId,
  pub path: TaskPath,
  /// The task or folder listed just before it.
  pub after: Option<TaskId>,
  pub settings: FolderSettings,
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
    use futures::FutureExt as _;
    Self {
      task_id,
      def,
      factory: Box::new(|ctx| {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let fut = f(ctx.clone(), rx);
        tokio::spawn(async move {
          // A panic ends only this task (the hook logs it); the kernel
          // must not wait for it as if it were still active.
          if std::panic::AssertUnwindSafe(fut)
            .catch_unwind()
            .await
            .is_err()
          {
            ctx.send(KernelCommand::TaskStopped(ExitInfo::error()));
          }
        });
        Box::new(ChannelTask::new(tx))
      }),
    }
  }
}

pub type Ack = Option<tokio::sync::oneshot::Sender<usize>>;

/// Makes a copy of a task from its definition as written, at a new id
/// and key.
pub type MakeCopy = Arc<
  dyn Fn(TaskId, Option<TaskKey>, TaskSpec) -> anyhow::Result<TaskRegistration>
    + Send
    + Sync,
>;

pub enum KernelCommand {
  Quit,
  /// Quits without saving, dropping a save a quit has begun.
  QuitWithoutSave,
  /// `Quit`, or `QuitWithoutSave` when `save` is false, answering in the
  /// dispatch that begins it the longest the runner can take to exit.
  QuitWithin {
    save: bool,
    reply: tokio::sync::oneshot::Sender<std::time::Duration>,
  },

  /// Registration is atomic: deps are resolved, the path claimed, and the
  /// task inserted in one dispatch, or nothing happens.
  RegisterTask(
    TaskRegistration,
    tokio::sync::oneshot::Sender<Result<(), RegisterError>>,
  ),

  /// Registers each task as `RegisterTask` does, and each folder, all in
  /// one dispatch. They are listed in the order given, whatever order
  /// their dependencies make the kernel register them in. Answers with
  /// one result for each, in that order.
  RegisterTasks(
    Vec<Registration>,
    tokio::sync::oneshot::Sender<Vec<Result<(), RegisterError>>>,
  ),

  /// Selector commands resolve the selector and act on the matches in the
  /// same dispatch, so no other message can interleave between the two.
  /// The ack is answered in that dispatch with the matched-task count;
  /// for `up`, the size of its set.
  Act(Verb, TaskSelector, Ack),
  Unpin(TaskSelector, Ack),
  /// Total: removes matching tasks in any state, killing running ones.
  /// A glob that matches a group or folder, or a folder's id, removes it
  /// with every task and folder under it. The ack counts both.
  Remove(TaskSelector, Ack),
  /// Takes a folder away, leaving what is under it; the ack is 1, or 0
  /// for no such folder.
  DropFolder(TaskId, Ack),
  /// Gives a folder what its entry says now.
  SetFolder(TaskId, FolderSettings, Ack),
  SetLabel(TaskSelector, Option<String>, Ack),
  /// Registers a copy of each matching task made from a definition;
  /// replies with how many, or why one could not be.
  Duplicate(
    TaskSelector,
    DuplicateRequest,
    tokio::sync::oneshot::Sender<Result<usize, RegisterError>>,
  ),
  /// Redefines each matching task in place: it keeps its id, screen, and
  /// place in the list. A running task runs the new definition from its
  /// next start. Refused, with nothing changed, when a dep matches no
  /// task or would close a cycle; answers with the count otherwise.
  Update(
    TaskSelector,
    Box<TaskUpdate>,
    tokio::sync::oneshot::Sender<Result<usize, RegisterError>>,
  ),
  /// Sets the config entry each matching task is written in; `None`
  /// makes it the runner's own.
  SetOrigin(TaskSelector, Option<Source>, Ack),
  /// Moves a task, or a group with every task under it, to another path
  /// or place in the list, in one dispatch.
  Move(
    MoveTask,
    tokio::sync::oneshot::Sender<Result<usize, MoveError>>,
  ),

  TaskMsg(TaskId, Box<dyn Any + Send>),

  Query(KernelQuery),

  SubscribePath(TaskKey, SubMode),
  UnsubscribePath(TaskKey, SubMode),
  /// Sends `true`/`false` whenever the selected set gains its first active
  /// task or loses its last one.
  WatchActive(TaskSelector, UnboundedSender<bool>),
  /// Waits until a set of tasks meets a condition (`WaitCond`),
  /// reporting on the request's channel; dropping the receiver ends it.
  Wait(Box<WaitRequest>),

  // Task reporting
  TaskStarted,
  TaskReady,
  TaskStopped(ExitInfo),

  /// A time limit set on the task's current state ran out (stop grace,
  /// kill wait, backoff delay, ready timeout). The epoch says which
  /// state it was set for, so a timeout from an earlier state is ignored.
  StateTimeout(TaskId, u64),

  /// Freeze every task and answer with the graph once all have reported
  /// `TaskFrozen`, or with why the runner cannot be frozen. While frozen
  /// the kernel defers intent and drives nothing; task reports and queries
  /// still go through. Frozen until `Thaw`, even after an error.
  Freeze(tokio::sync::oneshot::Sender<Result<KernelSnapshot, String>>),
  /// A task's answer to `TaskCmd::Freeze`, with that freeze's number.
  TaskFrozen(u64, TaskKindSnapshot),
  /// The freeze a quit began has waited long enough for its tasks; the
  /// runner quits without saving.
  FreezeTimeout(u64),
  /// Resumes driving; what the freeze deferred is then handled one message
  /// at a time, ahead of anything newer.
  Thaw,
}

/// Where a duplicate goes: `path` in the original's space, or next to
/// the original's path, and whether it starts at once.
#[derive(Clone, Debug)]
pub struct DuplicateRequest {
  pub label: Option<String>,
  pub path: Option<TaskPath>,
  pub start: bool,
}

/// What a wait waits for. A task is *live* when the kernel will run it,
/// or keep it running, without anyone acting: wanted, not ended
/// (exited, done, or stopping after a failed start it won't retry), and
/// each dep satisfied or live.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WaitCond {
  /// Satisfied: ready, or done for a job. Fails once a task is neither
  /// satisfied nor live.
  Ready,
  /// No process, and not live; then the last run is judged.
  Exit,
  /// No process, and not live, as `Exit`, but the last run is only
  /// reported. Fails while the task is live: it won't go down by itself.
  /// For the verbs that stop tasks.
  Down,
}

/// A verb that starts or stops the tasks a selector matches, as the
/// kernel carries it out; a wait can carry one out first.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Verb {
  /// Pins and starts the matches (the autostart set) and every task
  /// with a saved pin, clearing the saved pins; a done job is never run
  /// again.
  Up,
  Start,
  Restart,
  ForceRestart,
  Stop,
  Kill,
  Veto,
}

impl Verb {
  /// What the verb's wait is for: the ones that start tasks wait for
  /// them to be ready, the ones that stop them for them to be down.
  pub fn wait_cond(self) -> WaitCond {
    match self {
      Verb::Up | Verb::Start | Verb::Restart | Verb::ForceRestart => {
        WaitCond::Ready
      }
      Verb::Stop | Verb::Kill | Verb::Veto => WaitCond::Down,
    }
  }
}

/// Which tasks a wait is on, fixed when it begins.
pub enum WaitOn {
  /// What the selector matches. For `Ready`, the tasks nobody wants are
  /// left out, unless none is wanted.
  Matches(TaskSelector),
  /// Exactly these, as a wait carried across an upgrade has them.
  Ids(Vec<TaskId>),
  /// Carry out the verb on the selector's matches (for `up`, its set),
  /// then wait on what it acted on, in the same step.
  Act(Verb, TaskSelector),
}

pub struct WaitRequest {
  pub on: WaitOn,
  pub cond: WaitCond,
  pub events: UnboundedSender<WaitEvent>,
}

/// Where a task stands in a wait.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WaitMark {
  Pending,
  Met,
  Failed,
}

/// A task of a wait's set as it is now.
#[derive(Clone, Debug)]
pub struct WaitTask {
  pub info: TaskInfo,
  pub mark: WaitMark,
  /// When it failed for a dep: the task at the root of it.
  pub blocked_by: Option<String>,
  /// Removed while waited on; `info` is how it last was.
  pub removed: bool,
  /// Why it is (not) running, for a failed task still there, in `Done`.
  pub why: Option<TaskExplain>,
}

/// How a wait ended.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WaitVerdict {
  Met,
  Failed,
}

/// What a wait reports, in this order: `Set` once, after the step it
/// began in settles; `Changed` for each task whose state or mark
/// changes; `Done` once, and nothing after it.
#[derive(Clone, Debug)]
pub enum WaitEvent {
  /// The set, in list order.
  Set(Vec<WaitTask>),
  Changed(Box<WaitTask>),
  Done {
    verdict: WaitVerdict,
    tasks: Vec<WaitTask>,
    /// The runner is stopping: nothing is wanted any more.
    quitting: bool,
  },
}

/// A task's new definition, and what the kernel makes of it.
pub struct TaskUpdate {
  pub next: NextRun,
  pub restart: RestartMode,
  pub deps: Vec<TaskSelector>,
  pub tags: Vec<String>,
  pub label: Option<String>,
  /// Refused when the task's definition is no longer this one.
  pub was: Option<TaskSpec>,
  /// Restart it if it is running, so the new definition applies now.
  pub restart_running: bool,
}

/// `from` is a task's path or a group's; `place` is where it is listed
/// among the names under `to`'s parent.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MoveTask {
  pub space: TaskSpaceId,
  pub from: TaskPath,
  pub to: TaskPath,
  pub place: Place,
}

/// Where a moved task or group is listed among the names beside it. On
/// the wire: `keep`, `first`, `last`, or `{after: path}`.
#[derive(
  Clone, Debug, Default, Deserialize, Eq, Hash, PartialEq, Serialize,
)]
#[serde(rename_all = "lowercase")]
pub enum Place {
  /// Where it is now when it stays beside the same names, or the group
  /// it makes is, else last.
  #[default]
  Keep,
  First,
  Last,
  /// Right after this path, which is beside the destination.
  After(TaskPath),
}

impl Place {
  pub fn is_keep(&self) -> bool {
    *self == Place::Keep
  }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MoveError {
  /// No task is at or under the path.
  NotFound(TaskPath),
  /// A task or group is there already.
  Taken(TaskPath),
  /// A task is above the path: tasks are leaves.
  UnderTask(TaskPath),
  /// A group can't go under itself.
  IntoItself,
  /// `after` is not beside the destination.
  BadAfter(TaskPath),
}

impl fmt::Display for MoveError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      MoveError::NotFound(path) => write!(f, "no task at or under '{path}'"),
      MoveError::Taken(path) => write!(f, "'{path}' is taken"),
      MoveError::UnderTask(path) => {
        write!(f, "'{path}' is a task; a task can't have tasks under it")
      }
      MoveError::IntoItself => f.write_str("a group can't go under itself"),
      MoveError::BadAfter(path) => {
        write!(f, "'{path}' is not beside the destination")
      }
    }
  }
}

impl std::error::Error for MoveError {}

/// The kernel's part of an upgrade snapshot.
#[derive(Debug)]
pub struct KernelSnapshot {
  pub next_task_id: usize,
  pub tasks: Vec<crate::upgrade::snapshot::Task>,
  pub folders: Vec<crate::upgrade::snapshot::Folder>,
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
  /// A task is above the path, or under it: the path, then the other task.
  PathNested(TaskKey, TaskKey),
  /// A dep selector matched no task.
  MissingDep(TaskSelector),
  /// The dep needs the task, directly or through others.
  DepCycle(String),
  /// The task is gone, or its definition is not the one the new one was
  /// made from.
  Changed,
  /// The task is not one made from a definition, to replace or copy.
  NoDefinition,
  /// A folder is at the path.
  FolderAt(TaskKey),
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
      RegisterError::PathNested(key, other) => {
        let (above, below) = if other.path.depth() < key.path.depth() {
          (other, key)
        } else {
          (key, other)
        };
        write!(
          f,
          "a task can't have tasks under it: '{}' is under '{}'",
          below, above
        )
      }
      RegisterError::MissingDep(selector) => {
        write!(f, "dep '{}' matches no task", selector)
      }
      RegisterError::DepCycle(dep) => {
        write!(f, "'{}' depends on this task, so it can't be a dep", dep)
      }
      RegisterError::Changed => f.write_str("the task changed meanwhile"),
      RegisterError::NoDefinition => {
        f.write_str("the task is not one made from a definition")
      }
      RegisterError::FolderAt(key) => write!(f, "'{}' is a folder", key),
    }
  }
}

impl std::error::Error for RegisterError {}

/// A question to the kernel, and where its answer goes.
pub enum KernelQuery {
  /// The matching tasks, in list order.
  ListTasks(TaskSelector, Reply<Vec<TaskInfo>>),
  /// Why matching tasks are (not) running.
  Explain(TaskSelector, Reply<Vec<TaskExplain>>),
  /// Matching tasks with their definitions, in list order.
  Describe(TaskSelector, Reply<Vec<TaskDescription>>),
  /// The folders in a space, in list order.
  Folders(TaskSpaceId, Reply<Vec<FolderInfo>>),
}

pub type Reply<T> = tokio::sync::oneshot::Sender<T>;

/// A task and its definition as it is now: the spec's label is the
/// task's, and a dep it names by path follows that task's moves.
#[derive(Clone, Debug)]
pub struct TaskDescription {
  pub info: TaskInfo,
  pub spec: Option<TaskSpec>,
  pub origin: Option<Source>,
}

#[derive(Clone, Debug)]
pub struct TaskInfo {
  pub id: TaskId,
  pub space: TaskSpaceId,
  pub path: Option<TaskPath>,
  pub label: Option<String>,
  pub kind: TaskKind,
  pub state: TaskState,
  pub reason: Option<StateReason>,
  pub vt: Option<ScreenHandle>,
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
  pub reason: Option<StateReason>,
  pub wanted: bool,
  /// Wanted and every dependency transitively supported and satisfied;
  /// false on a wanted task means it is blocked by a dep below.
  pub supported: bool,
  pub vetoed: bool,
  pub pinned: bool,
  /// Started at the last save; `up` starts it again.
  pub saved_pin: bool,
  pub required_by: Vec<String>,
  pub deps: Vec<DepExplain>,
  pub attempts: u32,
}

#[derive(Clone, Debug)]
pub struct DepExplain {
  pub name: String,
  pub state: TaskState,
  pub reason: Option<StateReason>,
  pub wanted: bool,
  pub satisfied: bool,
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

  #[cfg(test)]
  pub fn register(
    &self,
    def: TaskDef,
    factory: Box<dyn FnOnce(TaskContext) -> Box<dyn Task> + Send>,
  ) -> TaskId {
    let task_id = self.alloc_id();
    drop(self.register_task(TaskRegistration {
      task_id,
      def,
      factory,
    }));
    task_id
  }

  #[cfg(test)]
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

  pub fn register_tasks(
    &self,
    registrations: Vec<Registration>,
  ) -> tokio::sync::oneshot::Receiver<Vec<Result<(), RegisterError>>> {
    let (ack_tx, ack_rx) = tokio::sync::oneshot::channel();
    self.send(KernelCommand::RegisterTasks(registrations, ack_tx));
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

  /// Begins a wait; it ends with its `Done` event, or when the receiver
  /// is dropped.
  pub fn wait(
    &self,
    on: WaitOn,
    cond: WaitCond,
  ) -> tokio::sync::mpsc::UnboundedReceiver<WaitEvent> {
    let (events, rx) = tokio::sync::mpsc::unbounded_channel();
    self.send(KernelCommand::Wait(Box::new(WaitRequest {
      on,
      cond,
      events,
    })));
    rx
  }

  pub fn unsubscribe_path(&self, key: TaskKey, mode: SubMode) {
    self.send(KernelCommand::UnsubscribePath(key, mode));
  }

  /// Asks now, in order with what was sent before; the answer is empty
  /// once the kernel is gone.
  fn ask<T: Default + Send + 'static>(
    &self,
    query: impl FnOnce(Reply<T>) -> KernelQuery,
  ) -> impl std::future::Future<Output = T> + Send + 'static {
    let (tx, rx) = tokio::sync::oneshot::channel();
    self.send(KernelCommand::Query(query(tx)));
    async move { rx.await.unwrap_or_default() }
  }

  /// The tasks a selector matches, in list order.
  pub fn list_tasks(
    &self,
    selector: TaskSelector,
  ) -> impl std::future::Future<Output = Vec<TaskInfo>> + Send + 'static {
    self.ask(|reply| KernelQuery::ListTasks(selector, reply))
  }

  /// Why the tasks a selector matches are (not) running.
  pub fn explain(
    &self,
    selector: TaskSelector,
  ) -> impl std::future::Future<Output = Vec<TaskExplain>> + Send + 'static {
    self.ask(|reply| KernelQuery::Explain(selector, reply))
  }

  /// The tasks a selector matches with their definitions, in list order.
  pub fn describe(
    &self,
    selector: TaskSelector,
  ) -> impl std::future::Future<Output = Vec<TaskDescription>> + Send + 'static
  {
    self.ask(|reply| KernelQuery::Describe(selector, reply))
  }

  /// The folders in a space, in list order.
  pub fn folders(
    &self,
    space: TaskSpaceId,
  ) -> impl std::future::Future<Output = Vec<FolderInfo>> + Send + 'static {
    self.ask(|reply| KernelQuery::Folders(space, reply))
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
