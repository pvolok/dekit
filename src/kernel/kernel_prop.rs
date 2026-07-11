//! Property harness: drives the kernel one turn at a time over random
//! graphs, task behaviors, and command sequences, checking invariants
//! after every turn. Everything is synchronous and deterministic — no
//! tokio, no time; timers are held as data and fired by the generated
//! sequence.

use std::collections::{HashMap, HashSet};

use proptest::prelude::*;

use super::{Graph, Kernel, SentCmd, TimerRequest};
use crate::kernel::kernel_message::{
  KernelCommand, KernelMessage, TaskSelector,
};
use crate::kernel::sub_trie::SubMode;
use crate::kernel::task::{
  Effects, ExitInfo, INIT_TASK_ID, ReadyMode, RestartMode, Task, TaskCmd,
  TaskDef, TaskId, TaskKind, TaskState,
};
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
      TaskCmd::Msg(_) => self.script.on_msg.apply(fx),
    }
  }
}

#[derive(Clone, Debug)]
struct TaskGen {
  job: bool,
  reported: bool,
  restart: RestartMode,
  pinned: bool,
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
  Stop,
  Kill,
  Restart,
  Down,
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
  Stop(Sel),
  Kill(Sel),
  Restart(Sel),
  Down(Sel),
  Veto(Sel),
  AddEdge(usize, usize),
  RemoveEdge(usize, usize),
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
    restart_mode(),
    any::<bool>(),
    prop::collection::vec(0..n, 0..3),
    script(),
  )
    .prop_map(|(job, reported, restart, pinned, deps, script)| TaskGen {
      job,
      reported,
      restart,
      pinned,
      deps,
      script,
    })
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
    2 => sel(n).prop_map(Cmd::Stop),
    1 => sel(n).prop_map(Cmd::Kill),
    2 => sel(n).prop_map(Cmd::Restart),
    1 => sel(n).prop_map(Cmd::Down),
    1 => sel(n).prop_map(Cmd::Veto),
    2 => (0..n, 0..n).prop_map(|(a, b)| Cmd::AddEdge(a, b)),
    1 => (0..n, 0..n).prop_map(|(a, b)| Cmd::RemoveEdge(a, b)),
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
  timers: Vec<TimerRequest>,
  /// Last seen epoch per task; cleared on removal so re-registration
  /// starts a fresh baseline.
  epochs: HashMap<TaskId, u64>,
}

impl Run {
  fn new() -> Self {
    Run {
      kernel: Kernel::new(),
      timers: Vec::new(),
      epochs: HashMap::new(),
    }
  }

  fn graph(&self) -> &Graph {
    &self.kernel.graph
  }

  /// One turn: dispatch a single message, settle, collect armed timers.
  /// Returns the commands the kernel sent to tasks during the turn.
  fn turn(
    &mut self,
    from: TaskId,
    command: KernelCommand,
  ) -> Vec<(TaskId, SentCmd)> {
    let _ = self.kernel.dispatch(KernelMessage { from, command });
    self.kernel.graph.settle();
    self.timers.extend(self.kernel.graph.take_timers());
    self.check();
    std::mem::take(&mut self.kernel.graph.sent)
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
        ReadyMode::Reported
      } else {
        ReadyMode::Immediate
      },
      restart: task.restart,
      deps: task.deps.iter().map(|d| TaskId(d + 1)).collect(),
      pinned: task.pinned,
      path: Some(TaskPath::new(format!("/t{}", i + 1)).unwrap()),
      label: None,
      vt: None,
      tags: vec![tag_name(i).to_string()],
    };
    let script = task.script;
    // Predicted outcome: refused on a duplicate id or a missing dep.
    let live = |t: &TaskId| self.kernel.graph.tasks.contains_key(t);
    let expect_ok = !live(&task_id) && def.deps.iter().all(live);
    let registered = self.kernel.graph.register_task_with_id(
      task_id,
      def,
      Box::new(move |_| Box::new(ScriptedTask { script })),
    );
    assert_eq!(
      registered, expect_ok,
      "registration outcome for {:?} (dup or missing dep misjudged)",
      task_id
    );
    self.kernel.graph.settle();
    self.timers.extend(self.kernel.graph.take_timers());
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
      Sel::All => TaskSelector::All,
      Sel::Exact(k) => TaskSelector::Glob(format!("/t{}", (k % n) + 1)),
      Sel::Wild => TaskSelector::Glob("/*".to_string()),
      Sel::Tag(even) => {
        TaskSelector::Tag(if *even { "even" } else { "odd" }.to_string())
      }
    }
  }

  /// Run an intent command and check the ack count and per-id effects
  /// against the pre-turn expectation.
  fn exec_intent(&mut self, world: &World, sel: &Sel, intent: Intent) {
    let expected = self.expect_matched(world, sel);
    let pre: Vec<(TaskId, Option<TaskState>)> =
      expected.iter().map(|t| (*t, self.state_of(*t))).collect();
    let selector = self.to_selector(world, sel);
    let (tx, mut rx) = tokio::sync::oneshot::channel();
    let command = match intent {
      Intent::Start => KernelCommand::Start(selector, Some(tx)),
      Intent::Stop => KernelCommand::Stop(selector, Some(tx)),
      Intent::Kill => KernelCommand::Kill(selector, Some(tx)),
      Intent::Restart => KernelCommand::Restart(selector, Some(tx)),
      Intent::Down => KernelCommand::Down(selector, Some(tx)),
      Intent::Veto => KernelCommand::Veto(selector, Some(tx)),
    };
    let sent = self.turn(INIT_TASK_ID, command);
    assert_eq!(
      rx.try_recv().expect("ack not answered in dispatch"),
      expected.len(),
      "ack count differs from act-time membership for {:?}",
      sel
    );

    // A command on a task in a matching state is never silently
    // swallowed; pins and vetoes follow the verb.
    for (t, pre_state) in pre {
      let must_bounce = match pre_state {
        Some(TaskState::Starting | TaskState::Running | TaskState::Ready) => {
          true
        }
        Some(
          TaskState::Idle
          | TaskState::Stopping
          | TaskState::Backoff
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
        Intent::Down => {
          assert!(!self.pinned(t), "down left the pin on {:?}", t);
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
      Cmd::Stop(sel) => self.exec_intent(world, sel, Intent::Stop),
      Cmd::Kill(sel) => self.exec_intent(world, sel, Intent::Kill),
      Cmd::Restart(sel) => self.exec_intent(world, sel, Intent::Restart),
      Cmd::Down(sel) => self.exec_intent(world, sel, Intent::Down),
      Cmd::Veto(sel) => self.exec_intent(world, sel, Intent::Veto),
      Cmd::AddEdge(a, b) => {
        self.turn(
          INIT_TASK_ID,
          KernelCommand::AddEdge {
            from: id(*a),
            to: id(*b),
          },
        );
      }
      Cmd::RemoveEdge(a, b) => {
        self.turn(
          INIT_TASK_ID,
          KernelCommand::RemoveEdge {
            from: id(*a),
            to: id(*b),
          },
        );
      }
      Cmd::Register(k) => self.register(world, k % n),
      Cmd::Remove(t) => {
        let t = id(*t);
        self.epochs.remove(&t);
        self.turn(INIT_TASK_ID, KernelCommand::RemoveTask(t));
      }
      Cmd::Subscribe(a, b) => {
        let path = TaskPath::new(format!("/t{}", (b % n) + 1)).unwrap();
        self.turn(id(*a), KernelCommand::SubscribePath(path, SubMode::Subtree));
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
        if !self.timers.is_empty() {
          let req = self.timers.remove(k % self.timers.len());
          self.turn(
            INIT_TASK_ID,
            KernelCommand::StateTimeout(req.task_id, req.epoch),
          );
        }
      }
    }
  }

  fn fire_all_timers(&mut self) {
    let due: Vec<TimerRequest> = std::mem::take(&mut self.timers);
    for req in due {
      self.turn(
        INIT_TASK_ID,
        KernelCommand::StateTimeout(req.task_id, req.epoch),
      );
    }
  }

  // ---- Invariants, checked after every turn ----

  fn check(&mut self) {
    // Every transition follows the legal state diagram; in particular a
    // commanded stop always lands in Idle, never in a dead-end state.
    for (id, from, to) in std::mem::take(&mut self.kernel.graph.transitions) {
      assert!(
        legal_transition(from, to),
        "illegal transition {:?} -> {:?} for {:?}",
        from,
        to,
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
        | TaskState::Backoff
        | TaskState::Done(_)
        | TaskState::Exited(_) => (),
      }
      // Epochs only move forward.
      let last = self.epochs.entry(*id).or_insert(task.epoch);
      assert!(task.epoch >= *last, "epoch went backward for {:?}", id);
      *last = task.epoch;
    }
  }
}

fn tag_name(i: usize) -> &'static str {
  if i % 2 == 0 { "even" } else { "odd" }
}

fn legal_transition(from: TaskState, to: TaskState) -> bool {
  match (from, to) {
    (TaskState::Idle, TaskState::Starting) => true,
    (
      TaskState::Starting,
      TaskState::Running
      | TaskState::Ready
      | TaskState::Stopping
      | TaskState::Backoff
      | TaskState::Done(_)
      | TaskState::Exited(_),
    ) => true,
    (
      TaskState::Running,
      TaskState::Ready
      | TaskState::Stopping
      | TaskState::Backoff
      | TaskState::Done(_)
      | TaskState::Exited(_),
    ) => true,
    (
      TaskState::Ready,
      TaskState::Stopping
      | TaskState::Backoff
      | TaskState::Done(_)
      | TaskState::Exited(_),
    ) => true,
    (TaskState::Stopping, TaskState::Idle) => true,
    (TaskState::Backoff, TaskState::Idle) => true,
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
  run.turn(INIT_TASK_ID, KernelCommand::Quit);
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
