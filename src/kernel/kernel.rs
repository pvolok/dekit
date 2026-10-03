use std::{
  collections::{HashMap, HashSet, VecDeque},
  sync::{Arc, atomic::AtomicUsize},
  task::{Context, Poll},
  time::{Duration, Instant},
};

use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tokio_util::time::{DelayQueue, delay_queue};

use crate::kernel::kernel_message::{KernelSnapshot, TaskContext};
use crate::upgrade::snapshot as snap;

use super::{
  kernel_message::{
    DepExplain, KernelCommand, KernelMessage, KernelQuery, KernelQueryResponse,
    RegisterError, SpaceSelector, TaskExplain, TaskInfo, TaskRegistration,
    TaskSelector, task_name,
  },
  namespace::Namespace,
  path_trie::PathConflict,
  sub_trie::SubMode,
  task::{
    Effects, ExitInfo, INIT_TASK_ID, ReadyMode, STOP_TIMEOUT, Task, TaskCmd,
    TaskDef, TaskEffect, TaskHandle, TaskId, TaskKind, TaskNotification,
    TaskNotify, TaskState,
  },
  task_key::{TaskKey, TaskSpaceId},
  task_path::TaskPath,
};

/// How long a hard-killed task may take to report stopped before the
/// kernel gives up waiting.
pub const KILL_WAIT: Duration = Duration::from_secs(10);

const BACKOFF_MIN: Duration = Duration::from_millis(100);
const BACKOFF_MAX: Duration = Duration::from_secs(30);
/// Uptime after which the restart attempt counter resets.
const BACKOFF_RESET: Duration = Duration::from_secs(10);

fn backoff_delay(attempts: u32) -> Duration {
  let exp = attempts.saturating_sub(1).min(16);
  BACKOFF_MIN.saturating_mul(1 << exp).min(BACKOFF_MAX)
}

/// Reports when the selected set goes between "some task active" and
/// "none active". Backoff counts as active: the task is coming back.
struct ActiveWatch {
  selector: TaskSelector,
  sender: UnboundedSender<bool>,
  active: bool,
}

/// An upgrade freeze in progress: which tasks still owe their snapshot,
/// what the others answered, and every message deferred meanwhile.
struct Frozen {
  /// Tasks echo it, so an answer to an earlier, timed-out freeze is not
  /// taken for this one.
  number: u64,
  /// Taken once answered, with the snapshot or the reason it failed.
  reply: Option<FreezeReply>,
  waiting: HashSet<TaskId>,
  snapshots: HashMap<TaskId, snap::TaskKind>,
  deferred: Vec<KernelMessage>,
}

enum FreezeReply {
  Rpc(tokio::sync::oneshot::Sender<Result<KernelSnapshot, String>>),
  /// The freeze a quit began: the snapshot goes to the save callback,
  /// then the runner quits.
  Quit,
}

/// Writes the graph somewhere the next start finds it.
pub type SaveFn = Box<dyn FnMut(KernelSnapshot) -> Result<(), String> + Send>;

/// A command the kernel sent to a task, logged for the property harness.
#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SentCmd {
  Start,
  Stop,
  Kill,
}

/// Pathless last; then by space and the place in it, or by id.
type ListKey<'a> = (bool, Option<(&'a TaskSpaceId, Vec<usize>)>, TaskId);

struct Graph {
  sender: UnboundedSender<KernelMessage>,

  quitting: bool,
  next_task_id: Arc<AtomicUsize>,
  tasks: HashMap<TaskId, TaskHandle>,
  /// `edges[a]` contains `b` when `a` requires `b`. Edges from
  /// `INIT_TASK_ID` are pins. Every endpoint is a registered task:
  /// `add_edge` refuses anything else and `remove_task` deletes all
  /// incident edges, so dangling edges cannot exist.
  edges: HashMap<TaskId, HashSet<TaskId>>,
  /// Reverse of `edges`: `redges[b]` contains `a` when `a` requires `b`.
  redges: HashMap<TaskId, HashSet<TaskId>>,
  ns: Namespace,
  tags: HashMap<String, HashSet<TaskId>>,

  /// Tasks whose `wanted`/`supported`/drive decision may have changed and
  /// need re-evaluating. Drained by `reconcile`.
  dirty: VecDeque<TaskId>,
  in_queue: HashSet<TaskId>,

  now: Instant,
  /// Tasks whose `deadline` was set, cleared, or re-set at a new epoch
  /// since the runtime last looked; it keeps one timer per task to match.
  timer_changes: Vec<TaskId>,
  /// Effects reported by tasks during the current step; `settle` applies
  /// them one at a time, each against settled state.
  pending_effects: VecDeque<(TaskId, TaskEffect)>,
  active_watches: Vec<ActiveWatch>,
  /// Set whenever a task is added, removed or changes state; watches only
  /// need re-checking then.
  state_changed: bool,
  frozen: Option<Frozen>,
  /// Freezes begun so far; numbers them.
  freezes: u64,
  /// Every state transition and whether the task's start had failed,
  /// for the property harness to check against the legal state diagram.
  #[cfg(test)]
  transitions: Vec<(TaskId, TaskState, TaskState, bool)>,
  /// Every command sent to a task, for the property harness to check
  /// that commands are never silently swallowed.
  #[cfg(test)]
  sent: Vec<(TaskId, SentCmd)>,
}

impl Graph {
  fn new(sender: UnboundedSender<KernelMessage>) -> Self {
    Self {
      sender,

      quitting: false,
      next_task_id: Arc::new(AtomicUsize::new(1)),
      tasks: HashMap::new(),
      edges: HashMap::new(),
      redges: HashMap::new(),
      ns: Namespace::new(),
      tags: HashMap::new(),

      dirty: VecDeque::new(),
      in_queue: HashSet::new(),

      now: Instant::now(),
      timer_changes: Vec::new(),
      pending_effects: VecDeque::new(),
      active_watches: Vec::new(),
      state_changed: false,
      frozen: None,
      freezes: 0,
      #[cfg(test)]
      transitions: Vec::new(),
      #[cfg(test)]
      sent: Vec::new(),
    }
  }

  fn context(&self) -> TaskContext {
    TaskContext::new(
      self.next_task_id.clone(),
      INIT_TASK_ID,
      self.sender.clone(),
    )
  }

  /// Returns whether the task was registered. `saved` is the lifecycle a
  /// restored task comes back in: in place before its edges are counted
  /// and its `Added` goes out, so nothing has to be corrected afterwards.
  fn register_task_with_id(
    &mut self,
    task_id: TaskId,
    def: TaskDef,
    factory: Box<dyn FnOnce(TaskContext) -> Box<dyn Task>>,
    saved: Option<&snap::Task>,
  ) -> Result<(), RegisterError> {
    if self.tasks.contains_key(&task_id) {
      return Err(RegisterError::IdTaken);
    }
    // Deps must exist: refusing here keeps every edge endpoint a
    // registered task, and a new task has no dependents yet, so no edge
    // added here can close a cycle.
    let mut deps: Vec<TaskId> = Vec::new();
    for selector in &def.deps {
      let ids = self.matching_ids(selector);
      if ids.is_empty() {
        return Err(RegisterError::MissingDep(selector.clone()));
      }
      for id in ids {
        if !deps.contains(&id) {
          deps.push(id);
        }
      }
    }
    // A taken path refuses the whole registration, checked before the
    // factory runs so a refused task spawns nothing.
    let space = def.space.clone();
    let after = def
      .after
      .and_then(|id| self.tasks.get(&id))
      .filter(|task| task.space == space)
      .and_then(|task| task.path.clone());
    let path = match def.path {
      Some(p) => {
        let key = TaskKey::new(space.clone(), p.clone());
        match self.ns.insert(&key, task_id, after.as_ref()) {
          Ok(()) => Some(p),
          Err(PathConflict::Taken(_)) => {
            return Err(RegisterError::PathTaken(key));
          }
          Err(PathConflict::Nested(other)) => {
            let other = TaskKey::new(space.clone(), other);
            return Err(RegisterError::PathNested(key, other));
          }
        }
      }
      None => None,
    };
    let ctx =
      TaskContext::new(self.next_task_id.clone(), task_id, self.sender.clone());
    let task = factory(ctx);
    let label = def.label.clone();
    let vt = def.vt.clone();
    let tags = def.tags.clone();
    let mut handle = TaskHandle {
      task,
      state: TaskState::Idle,
      epoch: 0,
      vetoed: false,
      killed: false,
      attempts: 0,
      last_start: None,
      deadline: None,
      wanted: false,
      supported: false,
      wanted_parents: 0,
      active_dependents: 0,
      start_failed: false,
      saved_pin: def.saved_pin,
      kind: def.kind,
      ready: def.ready,
      restart: def.restart,
      stop_timeout: def.stop_timeout,
      space: space.clone(),
      path: path.clone(),
      label: def.label,
      vt: def.vt,
      tags: def.tags,
    };
    if let Some(saved) = saved {
      let state = match saved.state {
        snap::TaskState::Idle {} => TaskState::Idle,
        snap::TaskState::Starting {} => TaskState::Starting,
        snap::TaskState::Running {} => TaskState::Running,
        snap::TaskState::Ready {} => TaskState::Ready,
        snap::TaskState::Stopping {} => TaskState::Stopping,
        snap::TaskState::Backoff(info) => TaskState::Backoff(info.into()),
        snap::TaskState::Done(info) => TaskState::Done(info.into()),
        snap::TaskState::Exited(info) => TaskState::Exited(info.into()),
      };
      // The config may have changed since the save; the task comes back
      // in a state it can be in as now configured (ORCHESTRATION.md
      // "Restore"). A live process follows the current ready check, as
      // after a fresh start. An end state the config would not file its
      // exit as is dropped: the task has not ended as what it now is.
      handle.state = match state {
        TaskState::Running => match handle.ready {
          ReadyMode::Immediate => TaskState::Ready,
          ReadyMode::Reported { .. } => TaskState::Running,
        },
        TaskState::Backoff(info)
        | TaskState::Done(info)
        | TaskState::Exited(info) => {
          let kept = handle.exit_state(info) == state
            && (!info.ready_timeout || handle.ready.timeout().is_some());
          if kept { state } else { TaskState::Idle }
        }
        TaskState::Idle
        | TaskState::Starting
        | TaskState::Ready
        | TaskState::Stopping => state,
      };
      handle.vetoed = saved.vetoed;
      handle.saved_pin = saved.saved_pin;
      handle.killed = saved.killed;
      // The verdict of a ready timeout the task no longer has is dropped:
      // its stop lands in Idle.
      handle.start_failed = saved.start_failed
        && handle.state == TaskState::Stopping
        && handle.ready.timeout().is_some();
      handle.attempts = saved.attempts;
      handle.last_start = saved.last_start_secs_ago.map(|secs| {
        self
          .now
          .checked_sub(Duration::from_secs(secs))
          .unwrap_or(self.now)
      });
    }
    // A ready timeout counts from the start; the other timers go on with
    // what was left of them.
    let timer = match handle.state {
      TaskState::Running => handle.ready.timeout().map(|timeout| {
        let elapsed = handle
          .last_start
          .map_or(Duration::ZERO, |start| self.now.duration_since(start));
        timeout.saturating_sub(elapsed)
      }),
      TaskState::Stopping | TaskState::Backoff(_) => {
        Some(Duration::from_millis(
          saved.and_then(|saved| saved.timer_ms).unwrap_or(0),
        ))
      }
      TaskState::Idle
      | TaskState::Starting
      | TaskState::Ready
      | TaskState::Done(_)
      | TaskState::Exited(_) => None,
    };
    let state = handle.state;
    let kind = handle.kind;
    let after = path
      .as_ref()
      .and_then(|p| self.ns.before(&TaskKey::new(space.clone(), p.clone())));
    self.tasks.insert(task_id, handle);
    self.state_changed = true;
    if let Some(delay) = timer {
      self.schedule_state_timeout(task_id, delay);
    }

    for tag in tags {
      self.tags.entry(tag).or_default().insert(task_id);
    }

    for dep_id in deps {
      self.add_edge(task_id, dep_id);
    }
    if def.pinned {
      self.add_edge(INIT_TASK_ID, task_id);
    }
    self.enqueue(task_id);

    self.notify_subscribers(
      task_id,
      space,
      path.clone(),
      TaskNotify::Added {
        path,
        label,
        after,
        kind,
        state,
        vt,
      },
    );
    Ok(())
  }

  /// Registers the tasks, listed in the order given, each after the ones
  /// among them it depends on. A place is held for every path before the
  /// first task is registered, and dropped again if its task is refused.
  fn register_tasks(
    &mut self,
    tasks: Vec<(Option<&snap::Task>, TaskRegistration)>,
  ) -> Vec<Result<(), RegisterError>> {
    let keys: Vec<Option<TaskKey>> = tasks
      .iter()
      .map(|(_, registration)| {
        let def = &registration.def;
        let path = def.path.clone()?;
        Some(TaskKey::new(def.space.clone(), path))
      })
      .collect();
    for key in keys.iter().flatten() {
      self.ns.place(key);
    }

    let slot: HashMap<TaskId, usize> = tasks
      .iter()
      .enumerate()
      .map(|(i, (_, registration))| (registration.task_id, i))
      .collect();
    let mut missing_deps: Vec<usize> = vec![0; tasks.len()];
    let mut dependents: Vec<Vec<usize>> = vec![Vec::new(); tasks.len()];
    for (i, (_, registration)) in tasks.iter().enumerate() {
      for dep in &registration.def.deps {
        let dep = match dep {
          TaskSelector::Id(dep) => dep,
          TaskSelector::Glob(_, _) | TaskSelector::Tag(_, _) => continue,
        };
        if let Some(&j) = slot.get(dep) {
          missing_deps[i] += 1;
          dependents[j].push(i);
        }
      }
    }
    let mut ready: VecDeque<usize> =
      (0..tasks.len()).filter(|i| missing_deps[*i] == 0).collect();
    let mut order = Vec::with_capacity(tasks.len());
    while let Some(i) = ready.pop_front() {
      order.push(i);
      for &dependent in &dependents[i] {
        missing_deps[dependent] -= 1;
        if missing_deps[dependent] == 0 {
          ready.push_back(dependent);
        }
      }
    }
    // The tasks of a dependency cycle: each is refused for the
    // dependency it lacks.
    order.extend((0..tasks.len()).filter(|i| missing_deps[*i] > 0));

    let mut tasks: Vec<Option<(Option<&snap::Task>, TaskRegistration)>> =
      tasks.into_iter().map(Some).collect();
    let mut results: Vec<Result<(), RegisterError>> =
      tasks.iter().map(|_| Ok(())).collect();
    for i in order {
      let (saved, registration) = tasks[i].take().expect("ordered once");
      results[i] = self.register_task_with_id(
        registration.task_id,
        registration.def,
        registration.factory,
        saved,
      );
    }
    for (key, result) in keys.iter().zip(&results) {
      if let (Some(key), Err(_)) = (key, result) {
        self.ns.prune(key);
      }
    }
    results
  }

  /// Begin quitting. Returns true if a quit was already in progress (the
  /// runtime should stop at once).
  fn begin_quit(&mut self) -> bool {
    if self.quitting {
      return true;
    }
    self.quitting = true;
    let ids: Vec<TaskId> = self.tasks.keys().copied().collect();
    for id in ids {
      self.enqueue(id);
    }
    false
  }

  /// Subscribes and replays `Added` for every task already in scope.
  fn subscribe(&mut self, subscriber: TaskId, key: TaskKey, mode: SubMode) {
    // Tasks the subscriber already hears about are not replayed again.
    let replay: Vec<(TaskPath, TaskId)> = self
      .ns
      .in_scope(&key, mode)
      .into_iter()
      .filter(|(path, _)| !self.ns.is_subscribed(subscriber, &key.space, path))
      .collect();
    self.ns.subscribe(subscriber, &key, mode);
    for (path, id) in replay {
      let t = &self.tasks[&id];
      let notify = TaskNotify::Added {
        after: self
          .ns
          .before(&TaskKey::new(key.space.clone(), path.clone())),
        path: Some(path),
        label: t.label.clone(),
        kind: t.kind,
        state: t.state,
        vt: t.vt.clone(),
      };
      self.deliver(id, notify, HashSet::from([subscriber]));
    }
  }

  fn unsubscribe(&mut self, subscriber: TaskId, key: TaskKey, mode: SubMode) {
    self.ns.unsubscribe(subscriber, &key, mode);
  }

  fn sender_space(&self, sender: TaskId) -> TaskSpaceId {
    self
      .tasks
      .get(&sender)
      .map_or_else(TaskSpaceId::default_space, |task| task.space.clone())
  }

  fn can_register(&self, sender: TaskId, space: &TaskSpaceId) -> bool {
    !space.is_reserved() || self.sender_space(sender) == *space
  }

  fn can_mutate(&self, sender: TaskId, task_id: TaskId) -> bool {
    self.tasks.get(&task_id).is_some_and(|task| {
      !task.space.is_reserved() || self.sender_space(sender) == task.space
    })
  }

  fn take_timer_changes(&mut self) -> Vec<TaskId> {
    std::mem::take(&mut self.timer_changes)
  }

  /// The task's timer: the epoch it is set at and the time left.
  fn timer(&self, task_id: TaskId) -> Option<(u64, Duration)> {
    let task = self.tasks.get(&task_id)?;
    let deadline = task.deadline?;
    Some((task.epoch, deadline.saturating_duration_since(self.now)))
  }

  fn any_active(&self, selector: &TaskSelector) -> bool {
    self.matching_ids(selector).into_iter().any(|id| {
      match self.tasks[&id].state {
        TaskState::Starting
        | TaskState::Running
        | TaskState::Ready
        | TaskState::Stopping
        | TaskState::Backoff(_) => true,
        TaskState::Idle | TaskState::Done(_) | TaskState::Exited(_) => false,
      }
    })
  }

  fn watch_active(
    &mut self,
    selector: TaskSelector,
    sender: UnboundedSender<bool>,
  ) {
    let active = self.any_active(&selector);
    self.active_watches.push(ActiveWatch {
      selector,
      sender,
      active,
    });
  }

  fn check_active_watches(&mut self) {
    if !self.state_changed {
      return;
    }
    self.state_changed = false;
    let mut watches = std::mem::take(&mut self.active_watches);
    watches.retain_mut(|watch| {
      let active = self.any_active(&watch.selector);
      if active == watch.active {
        return true;
      }
      watch.active = active;
      watch.sender.send(active).is_ok()
    });
    self.active_watches = watches;
  }

  // ---- Intent ----

  /// Sets the task's pin as a verb asks. The latest wish replaces what
  /// was saved, so it clears the saved pin (ORCHESTRATION.md "Up").
  fn set_pin(&mut self, task_id: TaskId, pinned: bool) {
    if let Some(task) = self.tasks.get_mut(&task_id) {
      task.saved_pin = false;
    }
    if pinned {
      self.add_edge(INIT_TASK_ID, task_id);
    } else {
      self.remove_edge(INIT_TASK_ID, task_id);
    }
  }

  fn cmd_start(&mut self, task_id: TaskId) {
    self.set_pin(task_id, true);
    self.demand(task_id, true);
  }

  /// `up` for one task of its set: pin it and demand its closure, but a
  /// done job stays done, even this one.
  fn cmd_up(&mut self, task_id: TaskId) {
    self.set_pin(task_id, true);
    self.demand(task_id, false);
  }

  /// A start demands the whole requirement closure: vetoed tasks are
  /// released and dead deps revived, so the pull is never blocked by an
  /// earlier stop or crash. Done jobs stay done, except `task_id` itself
  /// when `rerun_done` (a start that targets it).
  fn demand(&mut self, task_id: TaskId, rerun_done: bool) {
    let mut closure = vec![task_id];
    let mut seen: HashSet<TaskId> = HashSet::from([task_id]);
    let mut i = 0;
    while i < closure.len() {
      if let Some(deps) = self.edges.get(&closure[i]) {
        for dep in deps {
          if seen.insert(*dep) {
            closure.push(*dep);
          }
        }
      }
      i += 1;
    }
    for id in closure {
      let Some(task) = self.tasks.get_mut(&id) else {
        continue;
      };
      // Had the failed start's stop already landed, this demand would
      // revive the task; so it lands in Idle instead.
      task.start_failed = false;
      let state = task.state;
      self.set_vetoed(id, false);
      let revive = match state {
        TaskState::Backoff(_) | TaskState::Exited(_) => true,
        TaskState::Done(_) => rerun_done && id == task_id,
        TaskState::Idle
        | TaskState::Starting
        | TaskState::Running
        | TaskState::Ready
        | TaskState::Stopping => false,
      };
      if revive {
        self.set_state(id, TaskState::Idle);
      }
    }
  }

  fn set_vetoed(&mut self, task_id: TaskId, value: bool) {
    let Some(task) = self.tasks.get_mut(&task_id) else {
      return;
    };
    if task.vetoed == value {
      return;
    }
    task.vetoed = value;
    self.enqueue(task_id);
  }

  fn cmd_stop(&mut self, task_id: TaskId) {
    self.set_pin(task_id, false);
    self.stop_if_active(task_id);
  }

  fn cmd_kill(&mut self, task_id: TaskId) {
    self.set_pin(task_id, false);
    let Some(task) = self.tasks.get(&task_id) else {
      return;
    };
    match task.state {
      TaskState::Starting | TaskState::Running | TaskState::Ready => {
        self.set_state(task_id, TaskState::Stopping);
        self.hard_kill(task_id);
      }
      TaskState::Stopping => {
        // Already stopping gracefully: kill now.
        self.hard_kill(task_id);
      }
      TaskState::Idle
      | TaskState::Backoff(_)
      | TaskState::Done(_)
      | TaskState::Exited(_) => (),
    }
  }

  fn cmd_veto(&mut self, task_id: TaskId) {
    self.set_pin(task_id, false);
    self.set_vetoed(task_id, true);
  }

  fn cmd_restart(&mut self, task_id: TaskId) {
    self.cmd_start(task_id);
    self.stop_if_active(task_id);
  }

  fn stop_if_active(&mut self, task_id: TaskId) {
    let Some(task) = self.tasks.get(&task_id) else {
      return;
    };
    match task.state {
      TaskState::Starting | TaskState::Running | TaskState::Ready => {
        self.stop_task(task_id, false);
      }
      TaskState::Idle
      | TaskState::Stopping
      | TaskState::Backoff(_)
      | TaskState::Done(_)
      | TaskState::Exited(_) => (),
    }
  }

  // ---- Edges ----

  fn add_edge(&mut self, from: TaskId, to: TaskId) {
    if from == to || to == INIT_TASK_ID {
      log::warn!("Invalid edge: {:?} -> {:?}", from, to);
      return;
    }
    if !self.tasks.contains_key(&to)
      || (from != INIT_TASK_ID && !self.tasks.contains_key(&from))
    {
      log::warn!("Edge endpoint is not registered: {:?} -> {:?}", from, to);
      return;
    }
    if self.edges.entry(from).or_default().insert(to) {
      self.redges.entry(to).or_default().insert(from);
      let parent_wanted =
        from == INIT_TASK_ID || self.tasks.get(&from).is_some_and(|t| t.wanted);
      let parent_active = from != INIT_TASK_ID
        && self.tasks.get(&from).is_some_and(|t| t.state.is_active());
      if let Some(task) = self.tasks.get_mut(&to) {
        if parent_wanted {
          task.wanted_parents += 1;
        }
        if parent_active {
          task.active_dependents += 1;
        }
      }
    }
    // `to`'s wanted may change (new parent);
    self.enqueue(to);
    // `from`'s supported may change (new dependency).
    self.enqueue(from);
  }

  fn remove_edge(&mut self, from: TaskId, to: TaskId) {
    let removed = match self.edges.get_mut(&from) {
      Some(set) => {
        let removed = set.remove(&to);
        if set.is_empty() {
          self.edges.remove(&from);
        }
        removed
      }
      None => false,
    };
    if removed {
      if let Some(set) = self.redges.get_mut(&to) {
        set.remove(&from);
        if set.is_empty() {
          self.redges.remove(&to);
        }
      }
      let parent_wanted =
        from == INIT_TASK_ID || self.tasks.get(&from).is_some_and(|t| t.wanted);
      let parent_active = from != INIT_TASK_ID
        && self.tasks.get(&from).is_some_and(|t| t.state.is_active());
      if let Some(task) = self.tasks.get_mut(&to) {
        if parent_wanted {
          task.wanted_parents -= 1;
        }
        if parent_active {
          task.active_dependents -= 1;
        }
      }
    }
    self.enqueue(to);
    self.enqueue(from);
  }

  // ---- Reconciliation ----

  fn enqueue(&mut self, id: TaskId) {
    if id != INIT_TASK_ID
      && self.tasks.contains_key(&id)
      && self.in_queue.insert(id)
    {
      self.dirty.push_back(id);
    }
  }

  fn pop_dirty(&mut self) -> Option<TaskId> {
    let id = self.dirty.pop_front()?;
    self.in_queue.remove(&id);
    Some(id)
  }

  /// Re-evaluate every task that requires `id`: their `supported` reads
  /// `id`'s `supported` and satisfied state.
  fn enqueue_parents(&mut self, id: TaskId) {
    if let Some(parents) = self.redges.get(&id) {
      let parents: Vec<TaskId> = parents.iter().copied().collect();
      for p in parents {
        self.enqueue(p);
      }
    }
  }

  /// Complete the current step: apply queued effects and reconcile until
  /// both are exhausted. Effects are applied before any driving, so a
  /// stop decided by the reconciler can never overtake an exit the task
  /// already reported (a job's success must land in Done, not Idle).
  ///
  /// Termination relies on: drives are the only Idle -> Starting source,
  /// and self-exits (and the stop of a failed start) land in
  /// Backoff/Done/Exited, which only timers (arriving as later messages)
  /// can leave. A restart mode that retries
  /// within the same step would loop here.
  fn settle(&mut self) {
    let budget = self.tasks.len() * 16 + 64;
    let mut steps = 0;
    loop {
      if let Some((task_id, effect)) = self.pending_effects.pop_front() {
        match effect {
          TaskEffect::Started => self.on_task_started(task_id),
          TaskEffect::Ready => self.on_task_ready(task_id),
          TaskEffect::Stopped(info) => self.on_task_stopped(task_id, info),
        }
      } else if !self.dirty.is_empty() {
        self.reconcile_round();
      } else {
        break;
      }
      steps += 1;
      if steps > budget {
        // Keep the queues: the work continues on the next message
        // instead of leaving the caches silently stale.
        log::warn!("Settle did not finish after {} steps", steps);
        return;
      }
    }
    #[cfg(debug_assertions)]
    self.debug_check_invariants();
  }

  /// One reconcile round: propagate caches to a fixed point, then drive
  /// the affected tasks against that snapshot.
  fn reconcile_round(&mut self) {
    let mut to_drive: Vec<TaskId> = Vec::new();
    let mut seen: HashSet<TaskId> = HashSet::new();
    while let Some(id) = self.pop_dirty() {
      if seen.insert(id) {
        to_drive.push(id);
      }
      self.recompute_caches(id);
    }
    for id in to_drive {
      self.drive_task(id);
    }
  }

  /// Recompute one task's cached `wanted`/`supported` from its neighbors'
  /// caches, propagating any change outward.
  fn recompute_caches(&mut self, id: TaskId) {
    if !self.tasks.contains_key(&id) {
      return;
    }

    let new_wanted = self.compute_wanted(id);
    if self.tasks[&id].wanted != new_wanted {
      self.tasks.get_mut(&id).unwrap().wanted = new_wanted;
      // Children count this task among their wanted parents.
      if let Some(deps) = self.edges.get(&id) {
        let deps: Vec<TaskId> = deps.iter().copied().collect();
        for dep in deps {
          if let Some(t) = self.tasks.get_mut(&dep) {
            if new_wanted {
              t.wanted_parents += 1;
            } else {
              t.wanted_parents -= 1;
            }
          }
          self.enqueue(dep);
        }
      }
    }

    let new_supported = self.compute_supported(id);
    if self.tasks[&id].supported != new_supported {
      self.tasks.get_mut(&id).unwrap().supported = new_supported;
      // Parents' `supported` is computed from this one.
      self.enqueue_parents(id);
    }
  }

  /// Move a single task toward its intent, given its cached `supported`.
  fn drive_task(&mut self, id: TaskId) {
    let task = self.tasks.get(&id).expect("queued id live");
    let state = task.state;
    if task.supported {
      match state {
        TaskState::Idle => self.start_task(id),
        TaskState::Starting
        | TaskState::Running
        | TaskState::Ready
        | TaskState::Stopping
        | TaskState::Backoff(_)
        | TaskState::Done(_)
        | TaskState::Exited(_) => (),
      }
    } else {
      match state {
        TaskState::Starting | TaskState::Running | TaskState::Ready => {
          // Ordered shutdown: dependents go down first.
          if !self.has_active_dependent(id) {
            self.stop_task(id, false);
          }
        }
        TaskState::Backoff(_) => {
          // Cancel a pending retry.
          self.set_state(id, TaskState::Idle);
        }
        TaskState::Idle
        | TaskState::Stopping
        | TaskState::Done(_)
        | TaskState::Exited(_) => (),
      }
    }
  }

  /// Whether `id` should be up: reachable from a pin through non-vetoed
  /// nodes. Reads the counted wanted parents (a pin counts as one).
  fn compute_wanted(&self, id: TaskId) -> bool {
    if self.quitting {
      return false;
    }
    let task = self.tasks.get(&id).expect("queued id live");
    !task.vetoed && task.wanted_parents > 0
  }

  /// Whether `id` may run right now: wanted, with every dependency supported
  /// and currently satisfied. Reads only the cached `supported`/state of
  /// immediate dependencies.
  fn compute_supported(&self, id: TaskId) -> bool {
    if !self.tasks.get(&id).expect("queued id live").wanted {
      return false;
    }
    let Some(deps) = self.edges.get(&id) else {
      return true;
    };
    deps.iter().all(|d| {
      self
        .tasks
        .get(d)
        .is_some_and(|t| t.supported && t.is_satisfied())
    })
  }

  fn has_active_dependent(&self, task_id: TaskId) -> bool {
    self
      .tasks
      .get(&task_id)
      .is_some_and(|t| t.active_dependents > 0)
  }

  fn no_active_tasks(&self) -> bool {
    self.tasks.values().all(|task| !task.state.is_active())
  }

  /// Oracle: recompute `wanted`/`supported` from scratch and assert the
  /// incrementally-maintained caches agree. Debug builds only.
  #[cfg(debug_assertions)]
  fn debug_check_invariants(&self) {
    let wanted = self.oracle_wanted_set();
    let supported = self.oracle_supported_set(&wanted);
    for (id, task) in &self.tasks {
      debug_assert_eq!(
        task.wanted,
        wanted.contains(id),
        "cached wanted disagrees with oracle for {:?}",
        id
      );
      debug_assert_eq!(
        task.supported,
        supported.contains(id),
        "cached supported disagrees with oracle for {:?}",
        id
      );
      let mut wanted_parents = 0;
      let mut active_dependents = 0;
      if let Some(parents) = self.redges.get(id) {
        for p in parents {
          if *p == INIT_TASK_ID {
            wanted_parents += 1;
            continue;
          }
          if let Some(t) = self.tasks.get(p) {
            if t.wanted {
              wanted_parents += 1;
            }
            if t.state.is_active() {
              active_dependents += 1;
            }
          }
        }
      }
      debug_assert_eq!(
        task.wanted_parents, wanted_parents,
        "wanted_parents counter disagrees with oracle for {:?}",
        id
      );
      debug_assert_eq!(
        task.active_dependents, active_dependents,
        "active_dependents counter disagrees with oracle for {:?}",
        id
      );
    }
  }

  #[cfg(debug_assertions)]
  fn oracle_wanted_set(&self) -> HashSet<TaskId> {
    let mut wanted = HashSet::new();
    if self.quitting {
      return wanted;
    }
    let mut seen: HashSet<TaskId> = HashSet::from([INIT_TASK_ID]);
    let mut stack = vec![INIT_TASK_ID];
    while let Some(from) = stack.pop() {
      let Some(tos) = self.edges.get(&from) else {
        continue;
      };
      for to in tos {
        if !seen.insert(*to) {
          continue;
        }
        let Some(task) = self.tasks.get(to) else {
          continue;
        };
        if task.vetoed {
          continue;
        }
        wanted.insert(*to);
        stack.push(*to);
      }
    }
    wanted
  }

  #[cfg(debug_assertions)]
  fn oracle_supported_set(&self, wanted: &HashSet<TaskId>) -> HashSet<TaskId> {
    let mut memo: HashMap<TaskId, bool> = HashMap::new();
    for id in self.tasks.keys() {
      self.oracle_supported(*id, wanted, &mut memo);
    }
    memo
      .into_iter()
      .filter_map(|(id, ok)| if ok { Some(id) } else { None })
      .collect()
  }

  #[cfg(debug_assertions)]
  fn oracle_supported(
    &self,
    id: TaskId,
    wanted: &HashSet<TaskId>,
    memo: &mut HashMap<TaskId, bool>,
  ) -> bool {
    if let Some(ok) = memo.get(&id) {
      return *ok;
    }
    let mut ok = wanted.contains(&id);
    if ok && let Some(deps) = self.edges.get(&id) {
      for dep in deps {
        if !self.oracle_supported(*dep, wanted, memo)
          || !self.tasks.get(dep).is_some_and(|t| t.is_satisfied())
        {
          ok = false;
          break;
        }
      }
    }
    memo.insert(id, ok);
    ok
  }

  // ---- Driving tasks ----

  fn start_task(&mut self, task_id: TaskId) {
    let now = self.now;
    self
      .tasks
      .get_mut(&task_id)
      .expect("driven id live")
      .last_start = Some(now);
    self.set_state(task_id, TaskState::Starting);
    self.send_cmd(task_id, TaskCmd::Start);
  }

  /// `start_failed`: the stop lands as a failed exit (see `land_stop`).
  fn stop_task(&mut self, task_id: TaskId, start_failed: bool) {
    self.set_state(task_id, TaskState::Stopping);
    let task = self.tasks.get_mut(&task_id).expect("driven id live");
    task.start_failed = start_failed;
    let grace = task.stop_timeout;
    self.schedule_state_timeout(task_id, grace);
    self.send_cmd(task_id, TaskCmd::Stop);
  }

  /// Not ready within the ready timeout: stop it; the stop lands as a
  /// failed exit.
  fn fail_start(&mut self, task_id: TaskId) {
    log::info!("Task {:?} was not ready in time; stopping it", task_id);
    self.stop_task(task_id, true);
  }

  fn hard_kill(&mut self, task_id: TaskId) {
    let task = self.tasks.get_mut(&task_id).expect("driven id live");
    task.killed = true;
    // Manual bump: the stop grace's timer is replaced by the kill wait's.
    task.epoch += 1;
    self.send_cmd(task_id, TaskCmd::Kill);
    self.schedule_state_timeout(task_id, KILL_WAIT);
  }

  fn send_cmd(&mut self, task_id: TaskId, cmd: TaskCmd) {
    #[cfg(test)]
    match &cmd {
      TaskCmd::Start => self.sent.push((task_id, SentCmd::Start)),
      TaskCmd::Stop => self.sent.push((task_id, SentCmd::Stop)),
      TaskCmd::Kill => self.sent.push((task_id, SentCmd::Kill)),
      TaskCmd::Duplicate(_)
      | TaskCmd::Msg(_)
      | TaskCmd::Freeze(_)
      | TaskCmd::Thaw => (),
    }
    let mut fx = Effects::new();
    if let Some(task) = self.tasks.get_mut(&task_id) {
      task.task.handle_cmd(cmd, &mut fx);
    }
    self.queue_effects(task_id, &mut fx);
  }

  /// Sets the timer on the task's current state, at its current epoch.
  fn schedule_state_timeout(&mut self, task_id: TaskId, delay: Duration) {
    if let Some(task) = self.tasks.get_mut(&task_id) {
      task.deadline = Some(self.now + delay);
      self.timer_changes.push(task_id);
    }
  }

  fn on_state_timeout(&mut self, task_id: TaskId, epoch: u64) {
    let Some(task) = self.tasks.get_mut(&task_id) else {
      return;
    };
    if task.epoch != epoch {
      return;
    }
    // This timer has run out.
    task.deadline = None;
    self.timer_changes.push(task_id);
    let task = &self.tasks[&task_id];
    match task.state {
      TaskState::Backoff(_) => {
        // Retry delay is over; the reconciler restarts it if still wanted.
        self.set_state(task_id, TaskState::Idle);
      }
      TaskState::Stopping => {
        if task.killed {
          // The task ignored a hard kill for a full grace period; stop
          // waiting so the graph (and quit) can make progress. Land like
          // any completed stop: the reconciler restarts the task if it is
          // still wanted (a Start during the grace must not be lost). The
          // real process may be leaked.
          log::warn!("Task {:?} did not stop after kill; giving up", task_id);
          self.land_stop(task_id, ExitInfo::error());
        } else {
          // The stop was ignored for the whole grace period: hard-kill.
          self.hard_kill(task_id);
        }
      }
      TaskState::Running => {
        // Only a ready timeout is armed in Running.
        if task.ready.timeout().is_some() {
          self.fail_start(task_id);
        }
      }
      TaskState::Idle
      | TaskState::Starting
      | TaskState::Ready
      | TaskState::Done(_)
      | TaskState::Exited(_) => (),
    }
  }

  // ---- Task reports ----

  fn on_task_started(&mut self, task_id: TaskId) {
    let Some(task) = self.tasks.get(&task_id) else {
      return;
    };
    match task.state {
      TaskState::Starting => match task.ready {
        ReadyMode::Immediate => self.set_state(task_id, TaskState::Ready),
        ReadyMode::Reported { timeout } => {
          self.set_state(task_id, TaskState::Running);
          if let Some(timeout) = timeout {
            self.schedule_state_timeout(task_id, timeout);
          }
        }
      },
      TaskState::Idle
      | TaskState::Running
      | TaskState::Ready
      | TaskState::Stopping
      | TaskState::Backoff(_)
      | TaskState::Done(_)
      | TaskState::Exited(_) => {
        log::debug!("Ignoring started report in {:?}", task.state);
      }
    }
  }

  fn on_task_ready(&mut self, task_id: TaskId) {
    let Some(task) = self.tasks.get(&task_id) else {
      return;
    };
    match task.state {
      TaskState::Running => self.set_state(task_id, TaskState::Ready),
      TaskState::Idle
      | TaskState::Starting
      | TaskState::Ready
      | TaskState::Stopping
      | TaskState::Backoff(_)
      | TaskState::Done(_)
      | TaskState::Exited(_) => {
        log::debug!("Ignoring ready report in {:?}", task.state);
      }
    }
  }

  fn on_task_stopped(&mut self, task_id: TaskId, info: ExitInfo) {
    let now = self.now;
    let Some(task) = self.tasks.get_mut(&task_id) else {
      return;
    };
    match task.state {
      TaskState::Stopping => self.land_stop(task_id, info),
      TaskState::Starting | TaskState::Running | TaskState::Ready => {
        // Only a start that got ready and stayed up ends the backoff; one
        // that never got ready counts, however long it ran.
        let uptime = task.last_start.map(|t| now.duration_since(t));
        if task.state == TaskState::Ready
          && uptime.is_some_and(|t| t > BACKOFF_RESET)
        {
          task.attempts = 0;
        }
        self.land_exit(task_id, info);
      }
      TaskState::Idle
      | TaskState::Backoff(_)
      | TaskState::Done(_)
      | TaskState::Exited(_) => {
        log::debug!("Ignoring stop report in {:?}", task.state);
      }
    }
  }

  /// A commanded stop lands in Idle; the reconciler decides what happens
  /// next from intent. The stop of a failed start lands as a failed exit.
  fn land_stop(&mut self, task_id: TaskId, info: ExitInfo) {
    let Some(task) = self.tasks.get(&task_id) else {
      return;
    };
    if task.start_failed {
      let info = ExitInfo {
        ready_timeout: true,
        ..info
      };
      self.land_exit(task_id, info);
    } else {
      self.set_state(task_id, TaskState::Idle);
    }
  }

  /// The task ended on its own (or failed to start): a job's success is
  /// done; else it backs off and retries if its restart mode says so, or
  /// stays exited.
  fn land_exit(&mut self, task_id: TaskId, info: ExitInfo) {
    let Some(task) = self.tasks.get_mut(&task_id) else {
      return;
    };
    let state = task.exit_state(info);
    let delay = match state {
      TaskState::Backoff(_) => {
        task.attempts += 1;
        Some(backoff_delay(task.attempts))
      }
      TaskState::Idle
      | TaskState::Starting
      | TaskState::Running
      | TaskState::Ready
      | TaskState::Stopping
      | TaskState::Done(_)
      | TaskState::Exited(_) => None,
    };
    self.set_state(task_id, state);
    if let Some(delay) = delay {
      self.schedule_state_timeout(task_id, delay);
    }
  }

  // ---- State / bookkeeping ----

  fn set_state(&mut self, task_id: TaskId, state: TaskState) {
    let Some(task) = self.tasks.get_mut(&task_id) else {
      return;
    };
    if task.state == state {
      return;
    }
    let was_satisfied = task.is_satisfied();
    let was_active = task.state.is_active();
    #[cfg(test)]
    let (old_state, start_failed) = (task.state, task.start_failed);
    task.state = state;
    task.epoch += 1;
    task.killed = false;
    task.start_failed = false;
    // The epoch bump ended whatever timer was running.
    if task.deadline.take().is_some() {
      self.timer_changes.push(task_id);
    }
    self.state_changed = true;
    let now_satisfied = task.is_satisfied();
    let space = task.space.clone();
    let path = task.path.clone();

    #[cfg(test)]
    self
      .transitions
      .push((task_id, old_state, state, start_failed));
    self.enqueue(task_id);
    if was_satisfied != now_satisfied {
      // Dependents' `supported` depends on whether this task is satisfied.
      self.enqueue_parents(task_id);
    }
    if was_active != state.is_active() {
      // The shutdown gate counts active dependents on each dependency.
      let now_active = state.is_active();
      if let Some(deps) = self.edges.get(&task_id) {
        let deps: Vec<TaskId> = deps.iter().copied().collect();
        for dep in deps {
          if let Some(t) = self.tasks.get_mut(&dep) {
            if now_active {
              t.active_dependents += 1;
            } else {
              t.active_dependents -= 1;
            }
          }
          self.enqueue(dep);
        }
      }
    }

    self.notify_subscribers(
      task_id,
      space,
      path,
      TaskNotify::StateChanged(state),
    );
  }

  fn remove_task(&mut self, task_id: TaskId) {
    let Some(mut handle) = self.tasks.remove(&task_id) else {
      return;
    };
    self.state_changed = true;
    if handle.deadline.is_some() {
      self.timer_changes.push(task_id);
    }
    // Queued ids must be live when reconciliation drives them.
    if self.in_queue.remove(&task_id) {
      self.dirty.retain(|d| *d != task_id);
    }
    if handle.state.is_active() {
      let mut fx = Effects::new();
      handle.task.handle_cmd(TaskCmd::Kill, &mut fx);
    }
    if let Some(path) = &handle.path {
      self
        .ns
        .remove(&TaskKey::new(handle.space.clone(), path.clone()));
    }
    for tag in &handle.tags {
      if let Some(set) = self.tags.get_mut(tag) {
        set.remove(&task_id);
        if set.is_empty() {
          self.tags.remove(tag);
        }
      }
    }

    let was_wanted = handle.wanted;
    let was_active = handle.state.is_active();
    if let Some(deps) = self.edges.remove(&task_id) {
      for dep in deps {
        if let Some(set) = self.redges.get_mut(&dep) {
          set.remove(&task_id);
        }
        if let Some(t) = self.tasks.get_mut(&dep) {
          if was_wanted {
            t.wanted_parents -= 1;
          }
          if was_active {
            t.active_dependents -= 1;
          }
        }
        // A lost parent may make the dependency no longer wanted.
        self.enqueue(dep);
      }
    }
    if let Some(dependents) = self.redges.remove(&task_id) {
      for from in dependents {
        if let Some(set) = self.edges.get_mut(&from) {
          set.remove(&task_id);
        }
        // A lost dependency changes the dependent's `supported`.
        self.enqueue(from);
      }
    }

    self.ns.remove_subscriber(task_id);
    self.notify_subscribers(
      task_id,
      handle.space,
      handle.path,
      TaskNotify::Removed,
    );
  }

  fn set_task_label(&mut self, task_id: TaskId, label: Option<String>) {
    let Some(task) = self.tasks.get_mut(&task_id) else {
      return;
    };
    if task.label == label {
      return;
    }
    task.label = label.clone();
    let space = task.space.clone();
    let path = task.path.clone();
    self.notify_subscribers(
      task_id,
      space,
      path,
      TaskNotify::LabelChanged(label),
    );
  }

  // ---- Reads ----

  fn list_tasks(&self, selector: &TaskSelector) -> Vec<TaskInfo> {
    let mut tasks: Vec<TaskInfo> = self
      .matching_ids(selector)
      .into_iter()
      .filter_map(|id| {
        self.tasks.get(&id).map(|handle| TaskInfo {
          id,
          space: handle.space.clone(),
          path: handle.path.clone(),
          label: handle.label.clone(),
          state: handle.state,
          vt: handle.vt.clone(),
        })
      })
      .collect();
    tasks.sort_by_cached_key(|task| self.list_key(task.id));
    tasks
  }

  /// Sorts tasks into list order: the tasks with a path as their space
  /// lists them, space after space, then the others by id.
  fn list_key(&self, id: TaskId) -> ListKey<'_> {
    let place = self.tasks.get(&id).and_then(|task| {
      let path = task.path.as_ref()?;
      Some((&task.space, self.ns.position(&task.space, path)?))
    });
    (place.is_none(), place, id)
  }

  fn tasks_with_tag(&self, space: &SpaceSelector, tag: &str) -> Vec<TaskId> {
    let Some(set) = self.tags.get(tag) else {
      return Vec::new();
    };
    set
      .iter()
      .filter(|id| match space {
        SpaceSelector::One(space) => {
          self.tasks.get(id).is_some_and(|t| t.space == *space)
        }
        SpaceSelector::Any => true,
      })
      .copied()
      .collect()
  }

  fn matching_ids(&self, selector: &TaskSelector) -> Vec<TaskId> {
    match selector {
      TaskSelector::Id(id) => match self.tasks.contains_key(id) {
        true => vec![*id],
        false => Vec::new(),
      },
      TaskSelector::Glob(space, pattern) => self.ns.glob(space, pattern),
      TaskSelector::Tag(space, tag) => self.tasks_with_tag(space, tag),
    }
  }

  /// `up`'s set: the selector's matches and every task with a saved pin,
  /// each once, that the sender may change. The saved pins take one pass
  /// over the tasks: `up` is a user's command, not a per-event path.
  fn up_set(&self, sender: TaskId, selector: &TaskSelector) -> Vec<TaskId> {
    let mut ids = self.mutable_matching_ids(sender, selector);
    let matched: HashSet<TaskId> = ids.iter().copied().collect();
    let mut saved: Vec<TaskId> = self
      .tasks
      .iter()
      .filter(|(id, task)| {
        task.saved_pin && !matched.contains(id) && self.can_mutate(sender, **id)
      })
      .map(|(id, _)| *id)
      .collect();
    saved.sort_unstable();
    ids.extend(saved);
    ids
  }

  fn mutable_matching_ids(
    &self,
    sender: TaskId,
    selector: &TaskSelector,
  ) -> Vec<TaskId> {
    self
      .matching_ids(selector)
      .into_iter()
      .filter(|id| self.can_mutate(sender, *id))
      .collect()
  }

  fn explain(&self, task_id: TaskId) -> Option<TaskExplain> {
    let task = self.tasks.get(&task_id)?;
    let name = |id: TaskId| match self.tasks.get(&id) {
      Some(t) => task_name(id, &t.space, t.path.as_ref()),
      None => task_name(id, &TaskSpaceId::default_space(), None),
    };
    let pinned = self
      .redges
      .get(&task_id)
      .is_some_and(|s| s.contains(&INIT_TASK_ID));
    let mut required_by: Vec<TaskId> = self
      .redges
      .get(&task_id)
      .into_iter()
      .flatten()
      .copied()
      .filter(|from| *from != INIT_TASK_ID)
      .collect();
    required_by.sort_by_cached_key(|id| self.list_key(*id));
    let mut deps: Vec<TaskId> = self
      .edges
      .get(&task_id)
      .into_iter()
      .flatten()
      .copied()
      .collect();
    deps.sort_by_cached_key(|id| self.list_key(*id));
    let required_by = required_by.into_iter().map(name).collect();
    let deps = deps
      .into_iter()
      .map(|dep| {
        let task = self.tasks.get(&dep);
        DepExplain {
          name: name(dep),
          state: task.map_or(TaskState::Idle, |t| t.state),
          wanted: task.is_some_and(|t| t.wanted),
          satisfied: task.is_some_and(|t| t.is_satisfied()),
        }
      })
      .collect();
    Some(TaskExplain {
      id: task_id,
      name: name(task_id),
      state: task.state,
      wanted: task.wanted,
      supported: task.supported,
      vetoed: task.vetoed,
      pinned,
      saved_pin: task.saved_pin,
      required_by,
      deps,
      attempts: task.attempts,
    })
  }

  /// Effects are queued, never applied mid-step: a reconcile pass in
  /// progress can never be invalidated from underneath.
  fn queue_effects(&mut self, task_id: TaskId, fx: &mut Effects) {
    for effect in fx.drain() {
      self.pending_effects.push_back((task_id, effect));
    }
  }

  // ---- Upgrade freeze ----

  fn begin_freeze(
    &mut self,
    reply: tokio::sync::oneshot::Sender<Result<KernelSnapshot, String>>,
  ) {
    if self.frozen.is_some() {
      let _ = reply.send(Err("a freeze is already in progress".to_string()));
      return;
    }
    // Quit is deferred while frozen, so this holds for the whole freeze.
    if self.quitting {
      let _ = reply.send(Err("the runner is shutting down".to_string()));
      return;
    }
    self.freeze(FreezeReply::Rpc(reply));
  }

  /// Freezes so the graph can be saved before the quit stops anything.
  /// Returns the freeze number to time out on.
  fn begin_quit_freeze(&mut self) -> u64 {
    self.freeze(FreezeReply::Quit);
    self.freezes
  }

  fn freeze(&mut self, reply: FreezeReply) {
    self.freezes += 1;
    let number = self.freezes;
    let ids: Vec<TaskId> = self.tasks.keys().copied().collect();
    let mut dead = None;
    for id in &ids {
      let mut fx = Effects::new();
      if let Some(task) = self.tasks.get_mut(id) {
        task.task.handle_cmd(TaskCmd::Freeze(number), &mut fx);
      }
      for effect in fx.drain() {
        // Tasks answer through the queue. A report here comes from a task
        // whose handler is gone: nothing will ever answer for it.
        match &effect {
          TaskEffect::Stopped(_) => dead = dead.or(Some(*id)),
          TaskEffect::Started | TaskEffect::Ready => (),
        }
        self.pending_effects.push_back((*id, effect));
      }
    }
    let reply = match (dead, reply) {
      (Some(id), FreezeReply::Rpc(reply)) => {
        let task = &self.tasks[&id];
        let name = task_name(id, &task.space, task.path.as_ref());
        let _ = reply.send(Err(format!(
          "task {name} can't be carried across: its handler has stopped"
        )));
        None
      }
      // A dead handler never answers; the quit goes on without saving.
      (Some(id), FreezeReply::Quit) => {
        log::warn!("Not saving the tasks: task {:?} has stopped", id);
        None
      }
      (None, reply) => Some(reply),
    };
    self.frozen = Some(Frozen {
      number,
      reply,
      waiting: ids.into_iter().collect(),
      snapshots: HashMap::new(),
      deferred: Vec::new(),
    });
  }

  fn on_task_frozen(
    &mut self,
    task_id: TaskId,
    number: u64,
    snapshot: snap::TaskKind,
  ) {
    let Some(frozen) = self.frozen.as_mut() else {
      log::debug!("Ignoring TaskFrozen from {:?} outside a freeze", task_id);
      return;
    };
    if frozen.number != number || !frozen.waiting.remove(&task_id) {
      return;
    }
    frozen.snapshots.insert(task_id, snapshot);
  }

  /// Answers a complete freeze. A quit's freeze hands its snapshot back
  /// for saving instead.
  fn finish_freeze_if_complete(&mut self) -> Option<KernelSnapshot> {
    let frozen = self.frozen.as_mut()?;
    if !frozen.waiting.is_empty() {
      return None;
    }
    let reply = frozen.reply.take()?;
    let kinds = std::mem::take(&mut frozen.snapshots);
    let snapshot = self.snapshot(kinds);
    match reply {
      FreezeReply::Rpc(reply) => {
        let _ = reply.send(Ok(snapshot));
        None
      }
      FreezeReply::Quit => Some(snapshot),
    }
  }

  /// The longest the stops of a quit can take: each active task gets its
  /// grace, then the hard kill's wait, and a task stops only after its
  /// active dependents, so the bound is the heaviest chain of deps.
  fn stop_within(&self) -> Duration {
    let mut chains = HashMap::new();
    self
      .tasks
      .keys()
      .map(|&id| self.chain(id, &mut chains))
      .max()
      .unwrap_or_default()
  }

  /// `id`'s own stop time plus its heaviest chain of deps.
  fn chain(
    &self,
    id: TaskId,
    chains: &mut HashMap<TaskId, Duration>,
  ) -> Duration {
    if let Some(&chain) = chains.get(&id) {
      return chain;
    }
    let own = match self.tasks.get(&id) {
      Some(task) if task.state.is_active() => task.stop_timeout + KILL_WAIT,
      Some(_) | None => Duration::ZERO,
    };
    let deps = match self.edges.get(&id) {
      Some(deps) => deps
        .iter()
        .map(|&dep| self.chain(dep, chains))
        .max()
        .unwrap_or_default(),
      None => Duration::ZERO,
    };
    chains.insert(id, own + deps);
    own + deps
  }

  /// Whether the current freeze belongs to a quit.
  fn freezing_for_quit(&self) -> bool {
    match &self.frozen {
      Some(frozen) => match frozen.reply {
        Some(FreezeReply::Quit) => true,
        Some(FreezeReply::Rpc(_)) | None => false,
      },
      None => false,
    }
  }

  fn thaw(&mut self) -> Vec<KernelMessage> {
    let Some(frozen) = self.frozen.take() else {
      return Vec::new();
    };
    let ids: Vec<TaskId> = self.tasks.keys().copied().collect();
    for id in ids {
      self.send_cmd(id, TaskCmd::Thaw);
    }
    frozen.deferred
  }

  /// Everything in the graph, in the current snapshot schema. Task
  /// kinds are the tasks' own `TaskFrozen` answers.
  fn snapshot(
    &self,
    mut kinds: HashMap<TaskId, snap::TaskKind>,
  ) -> KernelSnapshot {
    let mut tasks: Vec<snap::Task> = Vec::with_capacity(kinds.len());
    for (id, task) in &self.tasks {
      let Some(kind) = kinds.remove(id) else {
        continue;
      };
      let state = match task.state {
        TaskState::Idle => snap::TaskState::Idle {},
        TaskState::Starting => snap::TaskState::Starting {},
        TaskState::Running => snap::TaskState::Running {},
        TaskState::Ready => snap::TaskState::Ready {},
        TaskState::Stopping => snap::TaskState::Stopping {},
        TaskState::Backoff(info) => snap::TaskState::Backoff(info.into()),
        TaskState::Done(info) => snap::TaskState::Done(info.into()),
        TaskState::Exited(info) => snap::TaskState::Exited(info.into()),
      };
      let mut deps: Vec<usize> = self
        .edges
        .get(id)
        .map(|set| set.iter().map(|dep| dep.0).collect())
        .unwrap_or_default();
      deps.sort_unstable();
      tasks.push(snap::Task {
        id: id.0,
        space: task.space.as_str().to_string(),
        path: task.path.as_ref().map(|path| path.as_str().to_string()),
        label: task.label.clone(),
        tags: task.tags.clone(),
        pinned: self
          .redges
          .get(id)
          .is_some_and(|set| set.contains(&INIT_TASK_ID)),
        deps,
        restart: task.restart.into(),
        job: match task.kind {
          TaskKind::Service => false,
          TaskKind::Job => true,
        },
        ready_timeout_ms: task.ready.timeout().map(|t| t.as_millis() as u64),
        stop_timeout_ms: (task.stop_timeout != STOP_TIMEOUT)
          .then_some(task.stop_timeout.as_millis() as u64),
        state,
        vetoed: task.vetoed,
        killed: task.killed,
        start_failed: task.start_failed,
        saved_pin: task.saved_pin,
        attempts: task.attempts,
        last_start_secs_ago: task
          .last_start
          .map(|start| self.now.saturating_duration_since(start).as_secs()),
        timer_ms: task.deadline.map(|deadline| {
          deadline.saturating_duration_since(self.now).as_millis() as u64
        }),
        kind,
      });
    }
    // In list order: a resume lists the tasks as it finds them here.
    tasks.sort_by_cached_key(|task| self.list_key(TaskId(task.id)));
    KernelSnapshot {
      next_task_id: self
        .next_task_id
        .load(std::sync::atomic::Ordering::Relaxed),
      tasks,
    }
  }

  fn notify_subscribers(
    &mut self,
    from: TaskId,
    from_space: TaskSpaceId,
    from_path: Option<TaskPath>,
    notify: TaskNotify,
  ) {
    let mut targets = HashSet::new();
    if let Some(path) = &from_path {
      self.ns.collect(
        &TaskKey::new(from_space.clone(), path.clone()),
        &mut targets,
      );
    }
    self.deliver(from, notify, targets);
  }

  fn deliver(
    &mut self,
    from: TaskId,
    notify: TaskNotify,
    targets: HashSet<TaskId>,
  ) {
    for listener_id in targets {
      if let Some(listener) = self.tasks.get_mut(&listener_id) {
        let mut fx = Effects::new();
        listener.task.handle_cmd(
          TaskCmd::msg(TaskNotification {
            from,
            notify: notify.clone(),
          }),
          &mut fx,
        );
        self.queue_effects(listener_id, &mut fx);
      }
    }
  }
}

pub struct Kernel {
  graph: Graph,
  sender: UnboundedSender<KernelMessage>,
  receiver: UnboundedReceiver<KernelMessage>,
  /// What a thaw handed back, handled before anything newer.
  replay: VecDeque<KernelMessage>,
  /// Set by a runner: a quit first freezes and saves the graph.
  save: Option<SaveFn>,
}

/// How long a quit waits for every task to freeze before giving up on
/// saving.
pub const SAVE_TIMEOUT: Duration = Duration::from_secs(10);

/// One timer per task, kept matching the graph's `deadline`s; each is
/// tagged with the epoch it was set at (ORCHESTRATION.md "State timers").
struct StateTimers {
  queue: DelayQueue<(TaskId, u64)>,
  keys: HashMap<TaskId, (u64, delay_queue::Key)>,
}

impl StateTimers {
  fn new() -> Self {
    Self {
      queue: DelayQueue::new(),
      keys: HashMap::new(),
    }
  }

  /// For each task whose timer changed: none without a deadline, else
  /// one at the task's epoch. A timer already at that epoch is the same
  /// one, since a deadline is set at most once per epoch.
  fn update(&mut self, graph: &mut Graph) {
    for task_id in graph.take_timer_changes() {
      let timer = graph.timer(task_id);
      if let (Some((armed, _)), Some((epoch, _))) =
        (self.keys.get(&task_id), timer)
        && *armed == epoch
      {
        continue;
      }
      if let Some((_, key)) = self.keys.remove(&task_id) {
        self.queue.remove(&key);
      }
      if let Some((epoch, delay)) = timer {
        let key = self.queue.insert((task_id, epoch), delay);
        self.keys.insert(task_id, (epoch, key));
      }
    }
  }

  fn poll_expired(
    &mut self,
    cx: &mut Context<'_>,
  ) -> Poll<Option<(TaskId, u64)>> {
    let Poll::Ready(expired) = self.queue.poll_expired(cx) else {
      return Poll::Pending;
    };
    Poll::Ready(expired.map(|expired| {
      let (task_id, epoch) = expired.into_inner();
      self.keys.remove(&task_id);
      (task_id, epoch)
    }))
  }
}

impl Kernel {
  pub fn new() -> Self {
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    let graph = Graph::new(sender.clone());
    Self {
      graph,
      sender,
      receiver,
      replay: VecDeque::new(),
      save: None,
    }
  }

  pub fn context(&self) -> TaskContext {
    self.graph.context()
  }

  /// Every quit then saves the graph first, so the next start brings the
  /// tasks back.
  pub fn save_on_quit(&mut self, save: SaveFn) {
    self.save = Some(save);
  }

  #[cfg(test)]
  pub fn register_task(
    &mut self,
    def: TaskDef,
    factory: impl FnOnce(TaskContext) -> Box<dyn Task> + 'static,
  ) -> TaskId {
    let task_id = TaskId(
      self
        .graph
        .next_task_id
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed),
    );
    let _ =
      self
        .graph
        .register_task_with_id(task_id, def, Box::new(factory), None);
    task_id
  }

  /// Installs a task before the kernel starts processing untrusted messages.
  pub fn register_task_registration(
    &mut self,
    registration: TaskRegistration,
  ) -> Result<(), RegisterError> {
    self.graph.register_task_with_id(
      registration.task_id,
      registration.def,
      registration.factory,
      None,
    )
  }

  /// Rebuilds the graph from an upgrade snapshot before `run`. Each task
  /// comes with the registration its kind built from the snapshot, and
  /// its saved lifecycle state; a task without one (added from the config
  /// since the snapshot) starts idle. They are listed in the order given
  /// and registered dependencies first.
  #[cfg_attr(windows, allow(dead_code))]
  pub fn restore(
    &mut self,
    next_task_id: usize,
    tasks: Vec<(Option<&snap::Task>, TaskRegistration)>,
  ) -> anyhow::Result<()> {
    self
      .graph
      .next_task_id
      .store(next_task_id, std::sync::atomic::Ordering::Relaxed);
    self.graph.now = Instant::now();
    let ids: Vec<TaskId> = tasks
      .iter()
      .map(|(_, registration)| registration.task_id)
      .collect();
    let snapshot: HashSet<TaskId> = ids.iter().copied().collect();
    for (_, registration) in &tasks {
      for dep in &registration.def.deps {
        let dep = match dep {
          TaskSelector::Id(dep) => dep,
          TaskSelector::Glob(_, _) | TaskSelector::Tag(_, _) => continue,
        };
        if !snapshot.contains(dep) {
          anyhow::bail!(
            "task {} has a dependency that is not in the snapshot",
            registration.task_id.0
          );
        }
      }
    }
    let results = self.graph.register_tasks(tasks);
    for (id, result) in ids.into_iter().zip(results) {
      result
        .map_err(|err| anyhow::anyhow!("restoring task {}: {err}", id.0))?;
    }
    Ok(())
  }

  pub async fn run(mut self) {
    // Made here, in the runtime whose clock it follows.
    let mut timers = StateTimers::new();
    self.after_dispatch(&mut timers);
    loop {
      // Deferred messages take one turn each, as if they had arrived
      // right after the thaw.
      let msg = match self.replay.pop_front() {
        Some(msg) => msg,
        None => tokio::select! {
          msg = self.receiver.recv() => match msg {
            Some(msg) => msg,
            None => {
              log::debug!("Kernel receiver returned None.");
              break;
            }
          },
          Some((task_id, epoch)) =
            std::future::poll_fn(|cx| timers.poll_expired(cx)) =>
          {
            KernelMessage {
              from: INIT_TASK_ID,
              command: KernelCommand::StateTimeout(task_id, epoch),
            }
          }
        },
      };
      self.graph.now = Instant::now();
      if self.dispatch(msg) {
        break;
      }
      self.after_dispatch(&mut timers);
      if self.graph.quitting && self.graph.no_active_tasks() {
        break;
      }
    }
    log::debug!("After kernel loop.");
  }

  /// Settles and sets timers; skipped while frozen so nothing drives.
  fn after_dispatch(&mut self, timers: &mut StateTimers) {
    if self.graph.frozen.is_some() {
      return;
    }
    self.graph.settle();
    self.graph.check_active_watches();
    timers.update(&mut self.graph);
  }

  /// Returns true when the loop should exit at once (a second quit).
  fn dispatch(&mut self, msg: KernelMessage) -> bool {
    let for_quit = self.graph.freezing_for_quit();
    let Some(frozen) = self.graph.frozen.as_mut() else {
      return self.dispatch_now(msg);
    };
    match msg.command {
      // Facts about what a task did before it froze, and reads.
      KernelCommand::TaskStarted
      | KernelCommand::TaskReady
      | KernelCommand::TaskStopped(_)
      | KernelCommand::Query(..)
      | KernelCommand::Freeze(_)
      | KernelCommand::TaskFrozen(..)
      | KernelCommand::FreezeTimeout(_)
      | KernelCommand::Thaw => (),
      // The quit under way covers this one; replaying it after the save
      // would read as a second quit and skip the graceful stop. The save
      // ends within its timeout, and while frozen the chain only shrinks.
      KernelCommand::Quit if for_quit => return false,
      KernelCommand::QuitWithin { save: true, reply } if for_quit => {
        let _ = reply.send(SAVE_TIMEOUT + self.graph.stop_within());
        return false;
      }
      // Drops the save under way.
      KernelCommand::QuitWithoutSave
      | KernelCommand::QuitWithin { save: false, .. }
        if for_quit => {}
      // Intent and delivery wait for the thaw (or die with this image).
      KernelCommand::Quit
      | KernelCommand::QuitWithoutSave
      | KernelCommand::QuitWithin { .. }
      | KernelCommand::RegisterTask(..)
      | KernelCommand::RegisterTasks(..)
      | KernelCommand::Start(..)
      | KernelCommand::Stop(..)
      | KernelCommand::Kill(..)
      | KernelCommand::Restart(..)
      | KernelCommand::ForceRestart(..)
      | KernelCommand::Unpin(..)
      | KernelCommand::Up(..)
      | KernelCommand::Veto(..)
      | KernelCommand::Remove(..)
      | KernelCommand::SetLabel(..)
      | KernelCommand::Duplicate(..)
      | KernelCommand::TaskMsg(..)
      | KernelCommand::SubscribePath(..)
      | KernelCommand::UnsubscribePath(..)
      | KernelCommand::WatchActive(..)
      | KernelCommand::StateTimeout(..) => {
        frozen.deferred.push(msg);
        return false;
      }
    }
    self.dispatch_now(msg)
  }

  /// Begins a quit, saving first when asked and a save is set up, and
  /// answers `reply` with the longest the runner can take to exit, read
  /// from the graph the quit begins on (ORCHESTRATION.md "Quit bound").
  /// Returns true when the loop should exit at once (a second quit).
  fn quit(
    &mut self,
    save: bool,
    reply: Option<tokio::sync::oneshot::Sender<Duration>>,
  ) -> bool {
    if save && self.save.is_some() && !self.graph.quitting {
      if let Some(reply) = reply {
        let _ = reply.send(SAVE_TIMEOUT + self.graph.stop_within());
      }
      let number = self.graph.begin_quit_freeze();
      let sender = self.sender.clone();
      tokio::spawn(async move {
        tokio::time::sleep(SAVE_TIMEOUT).await;
        let _ = sender.send(KernelMessage {
          from: INIT_TASK_ID,
          command: KernelCommand::FreezeTimeout(number),
        });
      });
      return false;
    }
    // Drops a save a quit has begun.
    if self.graph.freezing_for_quit() {
      self.replay.extend(self.graph.thaw());
    }
    let at_once = self.graph.begin_quit();
    if let Some(reply) = reply {
      let within = if at_once {
        Duration::ZERO
      } else {
        self.graph.stop_within()
      };
      let _ = reply.send(within);
    }
    at_once
  }

  fn dispatch_now(&mut self, msg: KernelMessage) -> bool {
    match msg.command {
      KernelCommand::Quit => {
        if self.quit(true, None) {
          return true;
        }
      }
      KernelCommand::QuitWithoutSave => {
        if self.quit(false, None) {
          return true;
        }
      }
      KernelCommand::QuitWithin { save, reply } => {
        if self.quit(save, Some(reply)) {
          return true;
        }
      }

      KernelCommand::RegisterTask(registration, ack) => {
        let registered =
          if self.graph.can_register(msg.from, &registration.def.space) {
            self.graph.register_task_with_id(
              registration.task_id,
              registration.def,
              registration.factory,
              None,
            )
          } else {
            Err(RegisterError::ReservedSpace(registration.def.space))
          };
        let _ = ack.send(registered);
      }
      KernelCommand::RegisterTasks(registrations, ack) => {
        let mut refused = Vec::with_capacity(registrations.len());
        let mut allowed = Vec::with_capacity(registrations.len());
        for registration in registrations {
          if self.graph.can_register(msg.from, &registration.def.space) {
            refused.push(None);
            allowed.push((None, registration));
          } else {
            refused
              .push(Some(RegisterError::ReservedSpace(registration.def.space)));
          }
        }
        let mut registered = self.graph.register_tasks(allowed).into_iter();
        let results = refused
          .into_iter()
          .map(|refused| match refused {
            Some(err) => Err(err),
            None => registered.next().expect("one result for each task"),
          })
          .collect();
        let _ = ack.send(results);
      }
      KernelCommand::Start(selector, ack) => {
        let ids = self.graph.mutable_matching_ids(msg.from, &selector);
        for id in &ids {
          self.graph.cmd_start(*id);
        }
        if let Some(ack) = ack {
          let _ = ack.send(ids.len());
        }
      }
      KernelCommand::Up(selector, ack) => {
        let ids = self.graph.up_set(msg.from, &selector);
        for id in &ids {
          self.graph.cmd_up(*id);
        }
        if let Some(ack) = ack {
          let _ = ack.send(ids.len());
        }
      }
      KernelCommand::Stop(selector, ack) => {
        let ids = self.graph.mutable_matching_ids(msg.from, &selector);
        for id in &ids {
          self.graph.cmd_stop(*id);
        }
        if let Some(ack) = ack {
          let _ = ack.send(ids.len());
        }
      }
      KernelCommand::Kill(selector, ack) => {
        let ids = self.graph.mutable_matching_ids(msg.from, &selector);
        for id in &ids {
          self.graph.cmd_kill(*id);
        }
        if let Some(ack) = ack {
          let _ = ack.send(ids.len());
        }
      }
      KernelCommand::Restart(selector, ack) => {
        let ids = self.graph.mutable_matching_ids(msg.from, &selector);
        for id in &ids {
          self.graph.cmd_restart(*id);
        }
        if let Some(ack) = ack {
          let _ = ack.send(ids.len());
        }
      }
      KernelCommand::ForceRestart(selector, ack) => {
        let ids = self.graph.mutable_matching_ids(msg.from, &selector);
        for id in &ids {
          self.graph.cmd_kill(*id);
          self.graph.cmd_start(*id);
        }
        if let Some(ack) = ack {
          let _ = ack.send(ids.len());
        }
      }
      KernelCommand::Unpin(selector, ack) => {
        let ids = self.graph.mutable_matching_ids(msg.from, &selector);
        for id in &ids {
          self.graph.set_pin(*id, false);
        }
        if let Some(ack) = ack {
          let _ = ack.send(ids.len());
        }
      }
      KernelCommand::Veto(selector, ack) => {
        let ids = self.graph.mutable_matching_ids(msg.from, &selector);
        for id in &ids {
          self.graph.cmd_veto(*id);
        }
        if let Some(ack) = ack {
          let _ = ack.send(ids.len());
        }
      }
      KernelCommand::Remove(selector, ack) => {
        let ids = self.graph.mutable_matching_ids(msg.from, &selector);
        for id in &ids {
          self.graph.remove_task(*id);
        }
        if let Some(ack) = ack {
          let _ = ack.send(ids.len());
        }
      }
      KernelCommand::SetLabel(selector, label, ack) => {
        let ids = self.graph.mutable_matching_ids(msg.from, &selector);
        for id in &ids {
          self.graph.set_task_label(*id, label.clone());
        }
        if let Some(ack) = ack {
          let _ = ack.send(ids.len());
        }
      }
      KernelCommand::Duplicate(selector, label, ack) => {
        let ids = self.graph.mutable_matching_ids(msg.from, &selector);
        for id in &ids {
          self.graph.send_cmd(*id, TaskCmd::Duplicate(label.clone()));
        }
        if let Some(ack) = ack {
          let _ = ack.send(ids.len());
        }
      }

      KernelCommand::TaskMsg(task_id, m) => {
        self.graph.send_cmd(task_id, TaskCmd::Msg(m));
      }

      KernelCommand::Query(query, response_tx) => {
        let _ = response_tx.send(self.handle_query(query));
      }

      KernelCommand::TaskStarted => self.graph.on_task_started(msg.from),
      KernelCommand::TaskReady => self.graph.on_task_ready(msg.from),
      KernelCommand::TaskStopped(info) => {
        self.graph.on_task_stopped(msg.from, info)
      }

      KernelCommand::StateTimeout(task_id, epoch) => {
        self.graph.on_state_timeout(task_id, epoch)
      }

      KernelCommand::SubscribePath(path, mode) => {
        self.graph.subscribe(msg.from, path, mode);
      }
      KernelCommand::UnsubscribePath(path, mode) => {
        self.graph.unsubscribe(msg.from, path, mode);
      }
      KernelCommand::WatchActive(selector, sender) => {
        self.graph.watch_active(selector, sender);
      }

      KernelCommand::Freeze(reply) => self.graph.begin_freeze(reply),
      KernelCommand::TaskFrozen(number, snapshot) => {
        self.graph.on_task_frozen(msg.from, number, snapshot)
      }
      KernelCommand::FreezeTimeout(number) => {
        let timed_out = self.graph.freezing_for_quit()
          && self
            .graph
            .frozen
            .as_ref()
            .is_some_and(|f| f.number == number);
        if timed_out {
          log::warn!("Not saving the tasks: a task did not freeze in time");
          self.replay.extend(self.graph.thaw());
          return self.graph.begin_quit();
        }
      }
      KernelCommand::Thaw => self.replay.extend(self.graph.thaw()),
    }
    if let Some(snapshot) = self.graph.finish_freeze_if_complete() {
      if let Some(save) = self.save.as_mut()
        && let Err(err) = save(snapshot)
      {
        log::warn!("Not saving the tasks: {err}");
      }
      self.replay.extend(self.graph.thaw());
      return self.graph.begin_quit();
    }
    false
  }

  fn handle_query(&self, query: KernelQuery) -> KernelQueryResponse {
    match query {
      KernelQuery::ListTasks(selector) => {
        KernelQueryResponse::TaskList(self.graph.list_tasks(&selector))
      }
      KernelQuery::Explain(selector) => KernelQueryResponse::Explain(
        self
          .graph
          .matching_ids(&selector)
          .into_iter()
          .filter_map(|id| self.graph.explain(id))
          .collect(),
      ),
    }
  }
}

#[cfg(test)]
#[path = "kernel_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "kernel_prop.rs"]
mod kernel_prop;
