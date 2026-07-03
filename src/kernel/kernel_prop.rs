//! Property harness: drives the kernel one turn at a time over random
//! graphs, task behaviors, and command sequences, checking invariants
//! after every turn. Everything is synchronous and deterministic — no
//! tokio, no time; timers are held as data and fired by the generated
//! sequence.

use std::collections::{HashMap, HashSet};

use proptest::prelude::*;

use super::{Graph, Kernel, SentCmd, TimerRequest};
use crate::kernel::kernel_message::{KernelCommand, KernelMessage};
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

#[derive(Clone, Debug)]
enum Cmd {
  Start(usize),
  Stop(usize),
  Kill(usize),
  Restart(usize),
  Down(usize),
  KeepDown(usize),
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

fn cmd(n: usize) -> impl Strategy<Value = Cmd> {
  prop_oneof![
    3 => (0..n).prop_map(Cmd::Start),
    2 => (0..n).prop_map(Cmd::Stop),
    1 => (0..n).prop_map(Cmd::Kill),
    2 => (0..n).prop_map(Cmd::Restart),
    1 => (0..n).prop_map(Cmd::Down),
    1 => (0..n).prop_map(Cmd::KeepDown),
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

  fn kept_down(&self, t: TaskId) -> Option<bool> {
    self.kernel.graph.tasks.get(&t).map(|h| h.kept_down)
  }

  fn register(&mut self, world: &World, i: usize) {
    let task = &world.tasks[i];
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
      tags: Vec::new(),
    };
    let script = task.script;
    self.kernel.graph.register_task_with_id(
      TaskId(i + 1),
      def,
      Box::new(move |_| Box::new(ScriptedTask { script })),
    );
    self.kernel.graph.settle();
    self.timers.extend(self.kernel.graph.take_timers());
    self.check();
    self.kernel.graph.sent.clear();
  }

  fn exec(&mut self, world: &World, cmd: &Cmd) {
    let n = world.tasks.len();
    let id = |k: usize| TaskId((k % n) + 1);
    match cmd {
      // The intent-bearing commands assert their response: a command on a
      // task in a matching state is never silently swallowed.
      Cmd::Start(t) => {
        let t = id(*t);
        self.turn(INIT_TASK_ID, KernelCommand::Start(t));
        assert!(self.pinned(t), "start did not pin {:?}", t);
        if let Some(kd) = self.kept_down(t) {
          assert!(!kd, "start left {:?} kept down", t);
        }
      }
      Cmd::Stop(t) => {
        let t = id(*t);
        let pre = self.state_of(t);
        let sent = self.turn(INIT_TASK_ID, KernelCommand::Stop(t));
        assert!(!self.pinned(t), "stop left the pin on {:?}", t);
        let must_stop = match pre {
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
        if must_stop {
          assert!(
            sent.contains(&(t, SentCmd::Stop)),
            "stop command swallowed for {:?} in {:?}",
            t,
            pre
          );
        }
      }
      Cmd::Kill(t) => {
        let t = id(*t);
        let pre = self.state_of(t);
        let sent = self.turn(INIT_TASK_ID, KernelCommand::Kill(t));
        assert!(!self.pinned(t), "kill left the pin on {:?}", t);
        let must_kill = match pre {
          Some(
            TaskState::Starting
            | TaskState::Running
            | TaskState::Ready
            | TaskState::Stopping,
          ) => true,
          Some(
            TaskState::Idle
            | TaskState::Backoff
            | TaskState::Done(_)
            | TaskState::Exited(_),
          )
          | None => false,
        };
        if must_kill {
          assert!(
            sent.contains(&(t, SentCmd::Kill)),
            "kill command swallowed for {:?} in {:?}",
            t,
            pre
          );
        }
      }
      Cmd::Restart(t) => {
        let t = id(*t);
        let pre = self.state_of(t);
        let sent = self.turn(INIT_TASK_ID, KernelCommand::Restart(t));
        assert!(self.pinned(t), "restart did not pin {:?}", t);
        if let Some(kd) = self.kept_down(t) {
          assert!(!kd, "restart left {:?} kept down", t);
        }
        let must_stop = match pre {
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
        if must_stop {
          assert!(
            sent.contains(&(t, SentCmd::Stop)),
            "restart did not bounce {:?} in {:?}",
            t,
            pre
          );
        }
      }
      Cmd::Down(t) => {
        let t = id(*t);
        self.turn(INIT_TASK_ID, KernelCommand::Down(t));
        assert!(!self.pinned(t), "down left the pin on {:?}", t);
      }
      Cmd::KeepDown(t) => {
        let t = id(*t);
        self.turn(INIT_TASK_ID, KernelCommand::KeepDown(t));
        assert!(!self.pinned(t), "keep-down left the pin on {:?}", t);
        if let Some(kd) = self.kept_down(t) {
          assert!(kd, "keep-down did not keep {:?} down", t);
        }
      }
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
        self.epochs.remove(&id(*t));
        self.turn(INIT_TASK_ID, KernelCommand::RemoveTask(id(*t)));
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
    for (from, tos) in &g.edges {
      for to in tos {
        assert_ne!(from, to, "self edge");
        assert_ne!(*to, INIT_TASK_ID, "edge into init");
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
