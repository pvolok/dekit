//! Property harness: drives the kernel one turn at a time over random
//! graphs, task behaviors, and command sequences, checking invariants
//! after every turn. Everything is synchronous and deterministic — no
//! tokio, no time; timers are held as data, one per task as the kernel
//! shell keeps them, and fired by the generated sequence.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::Duration;

use proptest::prelude::*;

use super::{Graph, Kernel, SentCmd};
use crate::kernel::kernel_message::{
  KernelCommand, KernelMessage, SpaceSelector, TaskSelector,
};
use crate::kernel::sub_trie::SubMode;
use crate::kernel::task::{
  Effects, ExitInfo, INIT_TASK_ID, ReadyMode, RestartMode, STOP_TIMEOUT, Task,
  TaskCmd, TaskDef, TaskId, TaskKind, TaskState,
};
use crate::kernel::task_key::{TaskKey, TaskSpaceId};
use crate::kernel::task_path::TaskPath;

// ---- Generated world ----

/// What a task reports back when it receives a command or notification.
#[derive(Clone, Copy, Debug)]
enum Reaction {
  Ignore,
  Started,
  StartedReady,
  ExitOk,
  ExitErr,
  StartedThenExitOk,
}

impl Reaction {
  fn apply(self, fx: &mut Effects) {
    match self {
      Reaction::Ignore => (),
      Reaction::Started => fx.started(),
      Reaction::StartedReady => {
        fx.started();
        fx.ready();
      }
      Reaction::ExitOk => fx.stopped(ExitInfo::code(0)),
      Reaction::ExitErr => fx.stopped(ExitInfo::code(1)),
      Reaction::StartedThenExitOk => {
        fx.started();
        fx.stopped(ExitInfo::code(0));
      }
    }
  }
}

#[derive(Clone, Copy, Debug)]
struct Script {
  on_start: Reaction,
  on_stop: Reaction,
  on_kill: Reaction,
  on_msg: Reaction,
}

struct ScriptedTask {
  script: Script,
}

impl Task for ScriptedTask {
  fn handle_cmd(&mut self, cmd: TaskCmd, fx: &mut Effects) {
    match cmd {
      TaskCmd::Start => self.script.on_start.apply(fx),
      TaskCmd::Stop => self.script.on_stop.apply(fx),
      TaskCmd::Kill => self.script.on_kill.apply(fx),
      // Never frozen: the harness drives no upgrades.
      TaskCmd::Duplicate(_) | TaskCmd::Freeze(_) | TaskCmd::Thaw => (),
      TaskCmd::Msg(_) => self.script.on_msg.apply(fx),
    }
  }
}

#[derive(Clone, Debug)]
struct TaskGen {
  job: bool,
  reported: bool,
  /// With `reported`: a ready timeout, fired like any timer.
  ready_timeout: bool,
  restart: RestartMode,
  pinned: bool,
  /// Without `pinned`: registered as from a saved file.
  saved_pin: bool,
  deps: Vec<usize>,
  script: Script,
}

#[derive(Clone, Copy, Debug)]
enum ReportKind {
  Started,
  Ready,
  StoppedOk,
  StoppedErr,
}

#[derive(Clone, Copy, Debug)]
enum Intent {
  Start,
  Up,
  Stop,
  Kill,
  Restart,
  Unpin,
  Veto,
}

/// Generated selector, resolved against the fixed `/t{i}` paths and
/// even/odd tags.
#[derive(Clone, Debug)]
enum Sel {
  Id(usize),
  All,
  Exact(usize),
  Wild,
  Tag(bool),
}

#[derive(Clone, Debug)]
enum Cmd {
  Start(Sel),
  Up(Sel),
  Stop(Sel),
  Kill(Sel),
  Restart(Sel),
  Unpin(Sel),
  Veto(Sel),
  Register(usize),
  Remove(usize),
  Subscribe(usize, usize),
  Report(usize, ReportKind),
  FireTimer(usize),
}

#[derive(Clone, Debug)]
struct World {
  tasks: Vec<TaskGen>,
  registered: Vec<bool>,
  cmds: Vec<Cmd>,
}

fn reaction() -> impl Strategy<Value = Reaction> {
  prop_oneof![
    3 => Just(Reaction::Started),
    2 => Just(Reaction::StartedReady),
    2 => Just(Reaction::ExitOk),
    1 => Just(Reaction::Ignore),
    1 => Just(Reaction::ExitErr),
    1 => Just(Reaction::StartedThenExitOk),
  ]
}

fn script() -> impl Strategy<Value = Script> {
  (reaction(), reaction(), reaction(), reaction()).prop_map(
    |(on_start, on_stop, on_kill, on_msg)| Script {
      on_start,
      on_stop,
      on_kill,
      on_msg,
    },
  )
}

fn restart_mode() -> impl Strategy<Value = RestartMode> {
  prop_oneof![
    Just(RestartMode::Never),
    Just(RestartMode::OnFailure),
    Just(RestartMode::Always),
  ]
}

fn task_gen(n: usize) -> impl Strategy<Value = TaskGen> {
  (
    any::<bool>(),
    any::<bool>(),
    any::<bool>(),
    restart_mode(),
    any::<bool>(),
    any::<bool>(),
    prop::collection::vec(0..n, 0..3),
    script(),
  )
    .prop_map(
      |(
        job,
        reported,
        ready_timeout,
        restart,
        pinned,
        saved_pin,
        deps,
        script,
      )| {
        TaskGen {
          job,
          reported,
          ready_timeout,
          restart,
          pinned,
          saved_pin,
          deps,
          script,
        }
      },
    )
}

fn sel(n: usize) -> impl Strategy<Value = Sel> {
  prop_oneof![
    4 => (0..n).prop_map(Sel::Id),
    1 => Just(Sel::All),
    2 => (0..n).prop_map(Sel::Exact),
    1 => Just(Sel::Wild),
    1 => any::<bool>().prop_map(Sel::Tag),
  ]
}

fn cmd(n: usize) -> impl Strategy<Value = Cmd> {
  prop_oneof![
    3 => sel(n).prop_map(Cmd::Start),
    2 => sel(n).prop_map(Cmd::Up),
    2 => sel(n).prop_map(Cmd::Stop),
    1 => sel(n).prop_map(Cmd::Kill),
    2 => sel(n).prop_map(Cmd::Restart),
    1 => sel(n).prop_map(Cmd::Unpin),
    1 => sel(n).prop_map(Cmd::Veto),
    2 => (0..n).prop_map(Cmd::Register),
    1 => (0..n).prop_map(Cmd::Remove),
    1 => (0..n, 0..n).prop_map(|(a, b)| Cmd::Subscribe(a, b)),
    2 => (0..n, report_kind()).prop_map(|(t, k)| Cmd::Report(t, k)),
    3 => (0..16usize).prop_map(Cmd::FireTimer),
  ]
}

fn report_kind() -> impl Strategy<Value = ReportKind> {
  prop_oneof![
    Just(ReportKind::Started),
    Just(ReportKind::Ready),
    Just(ReportKind::StoppedOk),
    Just(ReportKind::StoppedErr),
  ]
}

fn world() -> impl Strategy<Value = World> {
  (2..=7usize).prop_flat_map(|n| {
    (
      prop::collection::vec(task_gen(n), n),
      prop::collection::vec(any::<bool>(), n),
      prop::collection::vec(cmd(n), 1..40),
    )
      .prop_map(|(tasks, registered, cmds)| World {
        tasks,
        registered,
        cmds,
      })
  })
}

// ---- Turn runner ----

struct Run {
  kernel: Kernel,
  /// The shell's timers: one per task, at the epoch it was set at.
  timers: BTreeMap<TaskId, u64>,
  /// Timers the shell dropped. They are still fired now and then (as a
  /// timeout deferred by a freeze can arrive late); each must be ignored.
  dropped: Vec<(TaskId, u64)>,
  /// Last seen epoch per task; cleared on removal so re-registration
  /// starts a fresh baseline.
  epochs: HashMap<TaskId, u64>,
}

impl Run {
  fn new() -> Self {
    Run {
      kernel: Kernel::new(),
      timers: BTreeMap::new(),
      dropped: Vec::new(),
      epochs: HashMap::new(),
    }
  }

  fn graph(&self) -> &Graph {
    &self.kernel.graph
  }

  /// One turn: dispatch a single message, settle, update the timers.
  /// Returns the commands the kernel sent to tasks during the turn.
  fn turn(
    &mut self,
    from: TaskId,
    command: KernelCommand,
  ) -> Vec<(TaskId, SentCmd)> {
    let _ = self.kernel.dispatch(KernelMessage { from, command });
    self.finish_turn()
  }

  fn finish_turn(&mut self) -> Vec<(TaskId, SentCmd)> {
    self.kernel.graph.settle();
    self.update_timers();
    self.check();
    std::mem::take(&mut self.kernel.graph.sent)
  }

  /// What `StateTimers::update` does. A timer it drops must be one whose
  /// `StateTimeout` would fail the epoch check.
  fn update_timers(&mut self) {
    let g = &mut self.kernel.graph;
    for task_id in g.take_timer_changes() {
      let timer = g.timer(task_id).map(|(epoch, _)| epoch);
      match (self.timers.get(&task_id), timer) {
        (Some(armed), Some(epoch)) if *armed == epoch => continue,
        _ => (),
      }
      if let Some(armed) = self.timers.remove(&task_id) {
        assert!(
          g.tasks.get(&task_id).is_none_or(|t| t.epoch != armed),
          "dropped the live timer {:?}@{}",
          task_id,
          armed
        );
        self.dropped.push((task_id, armed));
      }
      if let Some(epoch) = timer {
        self.timers.insert(task_id, epoch);
      }
    }
  }

  fn fire(&mut self, task_id: TaskId, epoch: u64) {
    self.timers.remove(&task_id);
    self.turn(INIT_TASK_ID, KernelCommand::StateTimeout(task_id, epoch));
  }

  fn fire_dropped(&mut self, task_id: TaskId, epoch: u64) {
    let _ = self.kernel.dispatch(KernelMessage {
      from: INIT_TASK_ID,
      command: KernelCommand::StateTimeout(task_id, epoch),
    });
    let g = &self.kernel.graph;
    assert!(
      g.transitions.is_empty()
        && g.timer_changes.is_empty()
        && g.pending_effects.is_empty()
        && g.sent.is_empty(),
      "the dropped timer {:?}@{} acted",
      task_id,
      epoch
    );
    self.finish_turn();
  }

  fn state_of(&self, t: TaskId) -> Option<TaskState> {
    self.kernel.graph.tasks.get(&t).map(|h| h.state)
  }

  fn pinned(&self, t: TaskId) -> bool {
    self
      .kernel
      .graph
      .edges
      .get(&INIT_TASK_ID)
      .is_some_and(|s| s.contains(&t))
  }

  fn vetoed(&self, t: TaskId) -> Option<bool> {
    self.kernel.graph.tasks.get(&t).map(|h| h.vetoed)
  }

  fn register(&mut self, world: &World, i: usize) {
    let task = &world.tasks[i];
    let task_id = TaskId(i + 1);
    let def = TaskDef {
      kind: if task.job {
        TaskKind::Job
      } else {
        TaskKind::Service
      },
      ready: if task.reported {
        ReadyMode::Reported {
          timeout: task.ready_timeout.then_some(Duration::from_secs(30)),
        }
      } else {
        ReadyMode::Immediate
      },
      restart: task.restart,
      stop_timeout: STOP_TIMEOUT,
      deps: task
        .deps
        .iter()
        .map(|d| TaskSelector::Id(TaskId(d + 1)))
        .collect(),
      pinned: task.pinned,
      saved_pin: task.saved_pin && !task.pinned,
      space: TaskSpaceId::default_space(),
      path: Some(TaskPath::new(format!("t{}", i + 1)).unwrap()),
      label: None,
      after: None,
      vt: None,
      tags: vec![tag_name(i).to_string()],
    };
    let script = task.script;
    // Predicted outcome: refused on a duplicate id or a missing dep.
    let live = |t: &TaskId| self.kernel.graph.tasks.contains_key(t);
    let expect_ok =
      !live(&task_id) && task.deps.iter().all(|d| live(&TaskId(d + 1)));
    let registered = self.kernel.graph.register_task_with_id(
      task_id,
      def,
      Box::new(move |_| Box::new(ScriptedTask { script })),
      None,
    );
    assert_eq!(
      registered.is_ok(),
      expect_ok,
      "registration outcome for {:?} (dup or missing dep misjudged)",
      task_id
    );
    self.kernel.graph.settle();
    self.update_timers();
    self.check();
    self.kernel.graph.sent.clear();
  }

  /// The expected match set, resolved before the turn from the live task
  /// set and the fixed per-index paths/tags. The selector command must
  /// act on exactly this set (membership at act time).
  fn expect_matched(&self, world: &World, sel: &Sel) -> Vec<TaskId> {
    let n = world.tasks.len();
    let live = |k: usize| {
      let t = TaskId((k % n) + 1);
      self.kernel.graph.tasks.contains_key(&t).then_some(t)
    };
    match sel {
      Sel::Id(k) | Sel::Exact(k) => live(*k).into_iter().collect(),
      Sel::All | Sel::Wild => (0..n).filter_map(live).collect(),
      Sel::Tag(even) => (0..n)
        .filter(|i| (i % 2 == 0) == *even)
        .filter_map(live)
        .collect(),
    }
  }

  fn to_selector(&self, world: &World, sel: &Sel) -> TaskSelector {
    let n = world.tasks.len();
    match sel {
      Sel::Id(k) => TaskSelector::Id(TaskId((k % n) + 1)),
      Sel::All => TaskSelector::all(),
      Sel::Exact(k) => TaskSelector::Glob(
        SpaceSelector::default_space(),
        format!("t{}", (k % n) + 1),
      ),
      Sel::Wild => {
        TaskSelector::Glob(SpaceSelector::default_space(), "*".to_string())
      }
      Sel::Tag(even) => TaskSelector::Tag(
        SpaceSelector::default_space(),
        if *even { "even" } else { "odd" }.to_string(),
      ),
    }
  }

  /// Run an intent command and check the ack count and per-id effects
  /// against the pre-turn expectation.
  fn exec_intent(&mut self, world: &World, sel: &Sel, intent: Intent) {
    let mut expected = self.expect_matched(world, sel);
    match intent {
      // `up` also takes every task with a saved pin.
      Intent::Up => {
        let mut saved: Vec<TaskId> = self
          .graph()
          .tasks
          .iter()
          .filter(|(id, t)| t.saved_pin && !expected.contains(id))
          .map(|(id, _)| *id)
          .collect();
        saved.sort_unstable();
        expected.extend(saved);
      }
      Intent::Start
      | Intent::Stop
      | Intent::Kill
      | Intent::Restart
      | Intent::Unpin
      | Intent::Veto => (),
    }
    let pre: Vec<(TaskId, Option<TaskState>)> =
      expected.iter().map(|t| (*t, self.state_of(*t))).collect();
    let done_before: Vec<(TaskId, TaskState)> = self
      .graph()
      .tasks
      .iter()
      .filter_map(|(id, t)| match t.state {
        TaskState::Done(_) => Some((*id, t.state)),
        TaskState::Idle
        | TaskState::Starting
        | TaskState::Running
        | TaskState::Ready
        | TaskState::Stopping
        | TaskState::Backoff(_)
        | TaskState::Exited(_) => None,
      })
      .collect();
    let selector = self.to_selector(world, sel);
    let (tx, mut rx) = tokio::sync::oneshot::channel();
    let command = match intent {
      Intent::Start => KernelCommand::Start(selector, Some(tx)),
      Intent::Up => KernelCommand::Up(selector, Some(tx)),
      Intent::Stop => KernelCommand::Stop(selector, Some(tx)),
      Intent::Kill => KernelCommand::Kill(selector, Some(tx)),
      Intent::Restart => KernelCommand::Restart(selector, Some(tx)),
      Intent::Unpin => KernelCommand::Unpin(selector, Some(tx)),
      Intent::Veto => KernelCommand::Veto(selector, Some(tx)),
    };
    let sent = self.turn(INIT_TASK_ID, command);
    assert_eq!(
      rx.try_recv().expect("ack not answered in dispatch"),
      expected.len(),
      "ack count differs from act-time membership for {:?}",
      sel
    );

    // `up` consumes every saved pin and never runs a done job again.
    match intent {
      Intent::Up => {
        for (id, task) in &self.graph().tasks {
          assert!(!task.saved_pin, "up left the saved pin on {:?}", id);
        }
        for (id, state) in done_before {
          assert_eq!(self.state_of(id), Some(state), "up revived {:?}", id);
        }
      }
      Intent::Start
      | Intent::Stop
      | Intent::Kill
      | Intent::Restart
      | Intent::Unpin
      | Intent::Veto => (),
    }

    // A command on a task in a matching state is never silently
    // swallowed; pins and vetoes follow the verb, and the latest wish
    // replaces a saved pin.
    for (t, pre_state) in pre {
      if let Some(task) = self.graph().tasks.get(&t) {
        assert!(
          !task.saved_pin,
          "{:?} left the saved pin on {:?}",
          intent, t
        );
      }
      let must_bounce = match pre_state {
        Some(TaskState::Starting | TaskState::Running | TaskState::Ready) => {
          true
        }
        Some(
          TaskState::Idle
          | TaskState::Stopping
          | TaskState::Backoff(_)
          | TaskState::Done(_)
          | TaskState::Exited(_),
        )
        | None => false,
      };
      match intent {
        Intent::Start => {
          assert!(self.pinned(t), "start did not pin {:?}", t);
          if let Some(v) = self.vetoed(t) {
            assert!(!v, "start left {:?} vetoed", t);
          }
        }
        Intent::Up => {
          assert!(self.pinned(t), "up did not pin {:?}", t);
          if let Some(v) = self.vetoed(t) {
            assert!(!v, "up left {:?} vetoed", t);
          }
        }
        Intent::Stop => {
          assert!(!self.pinned(t), "stop left the pin on {:?}", t);
          if must_bounce {
            assert!(
              sent.contains(&(t, SentCmd::Stop)),
              "stop command swallowed for {:?} in {:?}",
              t,
              pre_state
            );
          }
        }
        Intent::Kill => {
          assert!(!self.pinned(t), "kill left the pin on {:?}", t);
          let must_kill = must_bounce || pre_state == Some(TaskState::Stopping);
          if must_kill {
            assert!(
              sent.contains(&(t, SentCmd::Kill)),
              "kill command swallowed for {:?} in {:?}",
              t,
              pre_state
            );
          }
        }
        Intent::Restart => {
          assert!(self.pinned(t), "restart did not pin {:?}", t);
          if let Some(v) = self.vetoed(t) {
            assert!(!v, "restart left {:?} vetoed", t);
          }
          if must_bounce {
            assert!(
              sent.contains(&(t, SentCmd::Stop)),
              "restart did not bounce {:?} in {:?}",
              t,
              pre_state
            );
          }
        }
        Intent::Unpin => {
          assert!(!self.pinned(t), "unpin left the pin on {:?}", t);
        }
        Intent::Veto => {
          assert!(!self.pinned(t), "veto left the pin on {:?}", t);
          if let Some(v) = self.vetoed(t) {
            assert!(v, "veto did not veto {:?}", t);
          }
        }
      }
    }
  }

  fn exec(&mut self, world: &World, cmd: &Cmd) {
    let n = world.tasks.len();
    let id = |k: usize| TaskId((k % n) + 1);
    match cmd {
      Cmd::Start(sel) => self.exec_intent(world, sel, Intent::Start),
      Cmd::Up(sel) => self.exec_intent(world, sel, Intent::Up),
      Cmd::Stop(sel) => self.exec_intent(world, sel, Intent::Stop),
      Cmd::Kill(sel) => self.exec_intent(world, sel, Intent::Kill),
      Cmd::Restart(sel) => self.exec_intent(world, sel, Intent::Restart),
      Cmd::Unpin(sel) => self.exec_intent(world, sel, Intent::Unpin),
      Cmd::Veto(sel) => self.exec_intent(world, sel, Intent::Veto),
      Cmd::Register(k) => self.register(world, k % n),
      Cmd::Remove(t) => {
        let t = id(*t);
        self.epochs.remove(&t);
        self.turn(
          INIT_TASK_ID,
          KernelCommand::Remove(TaskSelector::Id(t), None),
        );
        // The kernel never reuses an id; this harness does on
        // re-registration.
        self.dropped.retain(|(task_id, _)| *task_id != t);
      }
      Cmd::Subscribe(a, b) => {
        let path = TaskPath::new(format!("t{}", (b % n) + 1)).unwrap();
        self.turn(
          id(*a),
          KernelCommand::SubscribePath(
            TaskKey::default_space(path),
            SubMode::Subtree,
          ),
        );
      }
      Cmd::Report(t, kind) => {
        let command = match kind {
          ReportKind::Started => KernelCommand::TaskStarted,
          ReportKind::Ready => KernelCommand::TaskReady,
          ReportKind::StoppedOk => {
            KernelCommand::TaskStopped(ExitInfo::code(0))
          }
          ReportKind::StoppedErr => {
            KernelCommand::TaskStopped(ExitInfo::code(1))
          }
        };
        self.turn(id(*t), command);
      }
      Cmd::FireTimer(k) => {
        let total = self.timers.len() + self.dropped.len();
        if total == 0 {
          return;
        }
        let i = k % total;
        let live = self.timers.iter().nth(i).map(|(t, e)| (*t, *e));
        match live {
          Some((task_id, epoch)) => self.fire(task_id, epoch),
          None => {
            let (task_id, epoch) = self.dropped.remove(i - self.timers.len());
            self.fire_dropped(task_id, epoch);
          }
        }
      }
    }
  }

  fn fire_all_timers(&mut self) {
    let due: Vec<(TaskId, u64)> =
      self.timers.iter().map(|(t, e)| (*t, *e)).collect();
    for (task_id, epoch) in due {
      // An earlier firing may have replaced or dropped it.
      if self.timers.get(&task_id) == Some(&epoch) {
        self.fire(task_id, epoch);
      }
    }
  }

  // ---- Invariants, checked after every turn ----

  fn check(&mut self) {
    // Every transition follows the legal state diagram; in particular a
    // commanded stop always lands in Idle, and only the stop of a failed
    // start lands as a failed exit.
    for (id, from, to, start_failed) in
      std::mem::take(&mut self.kernel.graph.transitions)
    {
      assert!(
        legal_transition(from, to, start_failed),
        "illegal transition {:?} -> {:?} (start failed: {}) for {:?}",
        from,
        to,
        start_failed,
        id
      );
    }

    let g = &self.kernel.graph;
    assert!(
      g.pending_effects.is_empty(),
      "settle left effects pending (budget fired?)"
    );
    assert!(g.dirty.is_empty(), "settle left dirty tasks");
    #[cfg(debug_assertions)]
    g.debug_check_invariants();

    // Graph shape: edges/redges are exact inverses, no self edges, no
    // edges into init, and no cycle survives insertion checks.
    // Every edge endpoint is a registered task (INIT may only be a
    // source): dangling edges cannot exist.
    for (from, tos) in &g.edges {
      assert!(
        *from == INIT_TASK_ID || g.tasks.contains_key(from),
        "edge from unregistered {:?}",
        from
      );
      for to in tos {
        assert_ne!(from, to, "self edge");
        assert_ne!(*to, INIT_TASK_ID, "edge into init");
        assert!(
          g.tasks.contains_key(to),
          "edge {:?}->{:?} points at an unregistered id",
          from,
          to
        );
        assert!(
          g.redges.get(to).is_some_and(|s| s.contains(from)),
          "edge {:?}->{:?} missing from redges",
          from,
          to
        );
      }
    }
    for (to, froms) in &g.redges {
      for from in froms {
        assert!(
          g.edges.get(from).is_some_and(|s| s.contains(to)),
          "redge {:?}<-{:?} missing from edges",
          to,
          from
        );
      }
    }
    assert!(!has_cycle(&g.edges), "cycle in edges");

    for (id, task) in &g.tasks {
      if task.killed {
        assert_eq!(
          task.state,
          TaskState::Stopping,
          "killed flag outside Stopping for {:?}",
          id
        );
      }
      if task.start_failed {
        assert_eq!(
          task.state,
          TaskState::Stopping,
          "start_failed outside Stopping for {:?}",
          id
        );
        assert!(task.ready.timeout().is_some(), "failed without a timeout");
      }
      // A saved pin is only on an unpinned task: whatever pins clears it.
      if task.saved_pin {
        assert!(
          !g.edges.get(&INIT_TASK_ID).is_some_and(|s| s.contains(id)),
          "saved pin on the pinned {:?}",
          id
        );
      }
      // A settled supported task is never left sitting Idle.
      if task.supported {
        assert_ne!(
          task.state,
          TaskState::Idle,
          "supported task left idle: {:?}",
          id
        );
      }
      // An unsupported task keeps running only while a dependent holds
      // it up (Stopping means the stop is already underway).
      match task.state {
        TaskState::Starting | TaskState::Running | TaskState::Ready => {
          assert!(
            task.supported || task.active_dependents > 0,
            "unsupported task left up with no active dependent: {:?}",
            id
          );
        }
        TaskState::Idle
        | TaskState::Stopping
        | TaskState::Backoff(_)
        | TaskState::Done(_)
        | TaskState::Exited(_) => (),
      }
      // An end state is where its own exit lands as the task is
      // configured (what a restore keeps); so only a job is ever done.
      match task.state {
        TaskState::Backoff(info)
        | TaskState::Done(info)
        | TaskState::Exited(info) => {
          assert_eq!(
            task.exit_state(info),
            task.state,
            "end state its config does not file its exit as, for {:?}",
            id
          );
          assert!(
            !info.ready_timeout || task.ready.timeout().is_some(),
            "not ready in time without a ready timeout, for {:?}",
            id
          );
        }
        TaskState::Idle
        | TaskState::Starting
        | TaskState::Running
        | TaskState::Ready
        | TaskState::Stopping => (),
      }
      // A timer runs exactly in the states that have one, and the shell
      // holds exactly it: every timer the shell keeps passes the epoch
      // check.
      let timed = match task.state {
        TaskState::Stopping | TaskState::Backoff(_) => true,
        TaskState::Running => task.ready.timeout().is_some(),
        TaskState::Idle
        | TaskState::Starting
        | TaskState::Ready
        | TaskState::Done(_)
        | TaskState::Exited(_) => false,
      };
      assert_eq!(
        task.deadline.is_some(),
        timed,
        "deadline in {:?} for {:?}",
        task.state,
        id
      );
      assert_eq!(
        self.timers.get(id).copied(),
        task.deadline.map(|_| task.epoch),
        "shell timer disagrees with the deadline of {:?}",
        id
      );
      // Epochs only move forward.
      let last = self.epochs.entry(*id).or_insert(task.epoch);
      assert!(task.epoch >= *last, "epoch went backward for {:?}", id);
      *last = task.epoch;
    }
    for id in self.timers.keys() {
      assert!(g.tasks.contains_key(id), "timer of a removed task {:?}", id);
    }
  }
}

fn tag_name(i: usize) -> &'static str {
  if i % 2 == 0 { "even" } else { "odd" }
}

fn legal_transition(
  from: TaskState,
  to: TaskState,
  start_failed: bool,
) -> bool {
  match (from, to) {
    (TaskState::Stopping, TaskState::Idle) => !start_failed,
    (
      TaskState::Stopping,
      TaskState::Backoff(info) | TaskState::Exited(info),
    ) => start_failed && info.ready_timeout,
    (_, TaskState::Backoff(info) | TaskState::Exited(info))
      if info.ready_timeout =>
    {
      false
    }
    _ if start_failed => false,
    (TaskState::Idle, TaskState::Starting) => true,
    (
      TaskState::Starting,
      TaskState::Running
      | TaskState::Ready
      | TaskState::Stopping
      | TaskState::Backoff(_)
      | TaskState::Done(_)
      | TaskState::Exited(_),
    ) => true,
    (
      TaskState::Running,
      TaskState::Ready
      | TaskState::Stopping
      | TaskState::Backoff(_)
      | TaskState::Done(_)
      | TaskState::Exited(_),
    ) => true,
    (
      TaskState::Ready,
      TaskState::Stopping
      | TaskState::Backoff(_)
      | TaskState::Done(_)
      | TaskState::Exited(_),
    ) => true,
    (TaskState::Backoff(_), TaskState::Idle) => true,
    (TaskState::Done(_), TaskState::Idle) => true,
    (TaskState::Exited(_), TaskState::Idle) => true,
    _ => false,
  }
}

fn has_cycle(edges: &HashMap<TaskId, HashSet<TaskId>>) -> bool {
  fn visit(
    id: TaskId,
    edges: &HashMap<TaskId, HashSet<TaskId>>,
    done: &mut HashSet<TaskId>,
    stack: &mut HashSet<TaskId>,
  ) -> bool {
    if done.contains(&id) {
      return false;
    }
    if !stack.insert(id) {
      return true;
    }
    if let Some(tos) = edges.get(&id) {
      for to in tos {
        if visit(*to, edges, done, stack) {
          return true;
        }
      }
    }
    stack.remove(&id);
    done.insert(id);
    false
  }
  let mut done = HashSet::new();
  let mut stack = HashSet::new();
  edges
    .keys()
    .any(|id| visit(*id, edges, &mut done, &mut stack))
}

// ---- The property ----

fn run_case(world: &World) {
  let mut run = Run::new();
  for i in 0..world.tasks.len() {
    if world.registered[i] {
      run.register(world, i);
    }
  }
  for cmd in &world.cmds {
    run.exec(world, cmd);
  }

  // Drain: fire everything armed. Crash-looping tasks keep re-arming, so
  // this is bounded, not run to empty.
  for _ in 0..8 {
    if run.timers.is_empty() {
      break;
    }
    run.fire_all_timers();
  }

  // Quit liveness: from any reachable state, quit plus firing the armed
  // timers must reach no-active within the stop -> kill -> give-up chain.
  // Its bound is read from the graph it begins on.
  let within = run.graph().stop_within();
  let (reply, mut rx) = tokio::sync::oneshot::channel();
  run.turn(
    INIT_TASK_ID,
    KernelCommand::QuitWithin { save: true, reply },
  );
  assert_eq!(
    rx.try_recv()
      .expect("quit bound not answered in its dispatch"),
    within
  );
  let mut rounds = 0;
  while !run.graph().no_active_tasks() {
    assert!(
      !run.timers.is_empty(),
      "active tasks under quit with no timer to make progress"
    );
    run.fire_all_timers();
    rounds += 1;
    // Each chain level may need a full stop -> kill -> give-up sequence.
    assert!(rounds <= 32, "quit did not wind down within timer budget");
  }
}

proptest! {
  #[test]
  fn kernel_holds_invariants_under_random_traffic(w in world()) {
    run_case(&w);
  }
}
