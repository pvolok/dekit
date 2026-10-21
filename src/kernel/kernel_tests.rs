use std::time::Duration;

use tokio::sync::mpsc::{
  UnboundedReceiver, UnboundedSender, error::TryRecvError, unbounded_channel,
};

use super::*;
use crate::kernel::kernel_message::{
  Ack, FolderDef, FolderSettings, KernelSnapshot, Place, Registration,
};
use crate::kernel::task::RestartMode;
use crate::upgrade::snapshot::TaskKind as TaskKindSnapshot;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum RecordedCmd {
  Start,
  Stop,
  Kill,
}

/// Test directive delivered via `TaskMsg`, reported back through Effects.
enum Report {
  Started,
  Ready,
  Stopped(ExitInfo),
}

struct RecordingTask {
  name: &'static str,
  tx: UnboundedSender<(&'static str, RecordedCmd)>,
  ctx: TaskContext,
}

impl Task for RecordingTask {
  fn handle_cmd(&mut self, cmd: TaskCmd, fx: &mut Effects) {
    match cmd {
      TaskCmd::Start(_) => {
        self.tx.send((self.name, RecordedCmd::Start)).unwrap();
        fx.started();
      }
      TaskCmd::Stop => {
        self.tx.send((self.name, RecordedCmd::Stop)).unwrap();
        fx.stopped(ExitInfo::code(0));
      }
      TaskCmd::Kill => {
        self.tx.send((self.name, RecordedCmd::Kill)).unwrap();
        fx.stopped(ExitInfo::signal(9));
      }
      TaskCmd::Thaw => (),
      TaskCmd::Freeze(number) => self.ctx.send(KernelCommand::TaskFrozen(
        number,
        TaskKindSnapshot::Console {
          groups: Default::default(),
        },
      )),
      TaskCmd::Msg(m) => match m.downcast::<Report>() {
        Ok(report) => match *report {
          Report::Started => fx.started(),
          Report::Ready => fx.ready(),
          Report::Stopped(info) => fx.stopped(info),
        },
        Err(_) => (),
      },
    }
  }
}

/// Records commands like `RecordingTask`; reports success the moment
/// any message arrives.
struct ExitOnNotify {
  name: &'static str,
  tx: UnboundedSender<(&'static str, RecordedCmd)>,
}

impl Task for ExitOnNotify {
  fn handle_cmd(&mut self, cmd: TaskCmd, fx: &mut Effects) {
    match cmd {
      TaskCmd::Start(_) => {
        self.tx.send((self.name, RecordedCmd::Start)).unwrap();
        fx.started();
      }
      TaskCmd::Stop => {
        self.tx.send((self.name, RecordedCmd::Stop)).unwrap();
        fx.stopped(ExitInfo::code(1));
      }
      TaskCmd::Kill => {
        self.tx.send((self.name, RecordedCmd::Kill)).unwrap();
        fx.stopped(ExitInfo::signal(9));
      }
      // Never frozen by these tests.
      TaskCmd::Freeze(_) | TaskCmd::Thaw => (),
      TaskCmd::Msg(_) => fx.stopped(ExitInfo::code(0)),
    }
  }
}

/// Records commands like `RecordingTask` but reports starting only when
/// told: stays in Starting until then, or until commanded down.
struct SilentTask {
  name: &'static str,
  tx: UnboundedSender<(&'static str, RecordedCmd)>,
}

impl Task for SilentTask {
  fn handle_cmd(&mut self, cmd: TaskCmd, fx: &mut Effects) {
    match cmd {
      TaskCmd::Start(_) => {
        self.tx.send((self.name, RecordedCmd::Start)).unwrap();
      }
      TaskCmd::Stop => {
        self.tx.send((self.name, RecordedCmd::Stop)).unwrap();
        fx.stopped(ExitInfo::code(0));
      }
      TaskCmd::Kill => {
        self.tx.send((self.name, RecordedCmd::Kill)).unwrap();
        fx.stopped(ExitInfo::signal(9));
      }
      // Never frozen by these tests.
      TaskCmd::Freeze(_) | TaskCmd::Thaw => (),
      TaskCmd::Msg(m) => {
        if let Ok(report) = m.downcast::<Report>() {
          match *report {
            Report::Started => fx.started(),
            Report::Ready => fx.ready(),
            Report::Stopped(info) => fx.stopped(info),
          }
        }
      }
    }
  }
}

/// Records commands like `RecordingTask` but never reports stopping.
struct StubbornTask {
  name: &'static str,
  tx: UnboundedSender<(&'static str, RecordedCmd)>,
  ctx: TaskContext,
}

impl Task for StubbornTask {
  fn handle_cmd(&mut self, cmd: TaskCmd, fx: &mut Effects) {
    match cmd {
      TaskCmd::Start(_) => {
        self.tx.send((self.name, RecordedCmd::Start)).unwrap();
        fx.started();
      }
      TaskCmd::Stop => {
        self.tx.send((self.name, RecordedCmd::Stop)).unwrap();
      }
      TaskCmd::Kill => {
        self.tx.send((self.name, RecordedCmd::Kill)).unwrap();
      }
      TaskCmd::Thaw => (),
      TaskCmd::Freeze(number) => self.ctx.send(KernelCommand::TaskFrozen(
        number,
        TaskKindSnapshot::Console {
          groups: Default::default(),
        },
      )),
      TaskCmd::Msg(_) => (),
    }
  }
}

struct Fixture {
  kernel: Option<Kernel>,
  pc: TaskContext,
  rx: UnboundedReceiver<(&'static str, RecordedCmd)>,
  tx: UnboundedSender<(&'static str, RecordedCmd)>,
}

impl Fixture {
  fn new() -> Self {
    let kernel = Kernel::new();
    let pc = kernel.context();
    let (tx, rx) = unbounded_channel();
    Self {
      kernel: Some(kernel),
      pc,
      rx,
      tx,
    }
  }

  fn add(&mut self, name: &'static str, def: TaskDef) -> TaskId {
    let tx = self.tx.clone();
    self
      .kernel
      .as_mut()
      .unwrap()
      .register_task(def, move |ctx| Box::new(RecordingTask { name, tx, ctx }))
  }

  fn run(&mut self) -> tokio::task::JoinHandle<()> {
    tokio::spawn(self.kernel.take().unwrap().run())
  }

  async fn recv(&mut self) -> (&'static str, RecordedCmd) {
    tokio::time::timeout(Duration::from_secs(1), self.rx.recv())
      .await
      .expect("timed out waiting for task command")
      .expect("task command channel closed")
  }

  fn assert_no_cmd(&mut self) {
    match self.rx.try_recv() {
      Ok(cmd) => panic!("unexpected task command: {cmd:?}"),
      Err(TryRecvError::Disconnected) => {
        panic!("task command channel closed")
      }
      Err(TryRecvError::Empty) => {}
    }
  }

  /// Round-trip a query so all previously sent messages are processed.
  async fn flush(&self) {
    let rx = self.pc.list_tasks(TaskSelector::all());
    tokio::time::timeout(Duration::from_secs(1), rx)
      .await
      .expect("timed out waiting for kernel query response");
  }

  async fn quit(mut self, handle: tokio::task::JoinHandle<()>) {
    self.pc.send(KernelCommand::Quit);
    // Drain commands so recording sends don't panic on a closed channel.
    let drain =
      tokio::spawn(async move { while self.rx.recv().await.is_some() {} });
    tokio::time::timeout(Duration::from_secs(2), handle)
      .await
      .expect("timed out waiting for kernel to quit")
      .unwrap();
    drain.abort();
  }
}

fn path_def(path: &str) -> TaskDef {
  TaskDef {
    path: Some(TaskPath::new(path).unwrap()),
    ..Default::default()
  }
}

#[tokio::test]
async fn start_starts_and_unpin_stops() {
  let mut fx = Fixture::new();
  let a = fx.add("a", path_def("a"));
  let handle = fx.run();

  fx.pc.send(KernelCommand::Start(TaskSelector::Id(a), None));
  assert_eq!(fx.recv().await, ("a", RecordedCmd::Start));

  fx.pc.send(KernelCommand::Unpin(TaskSelector::Id(a), None));
  assert_eq!(fx.recv().await, ("a", RecordedCmd::Stop));

  fx.quit(handle).await;
}

#[tokio::test]
async fn second_task_at_same_path_is_refused() {
  let mut fx = Fixture::new();
  let a = fx.add("a", path_def("x"));
  let b = fx.add("b", path_def("x"));
  let handle = fx.run();

  fx.pc.send(KernelCommand::Start(TaskSelector::Id(b), None));
  fx.flush().await;
  fx.assert_no_cmd();

  fx.pc.send(KernelCommand::Start(TaskSelector::Id(a), None));
  assert_eq!(fx.recv().await, ("a", RecordedCmd::Start));

  fx.quit(handle).await;
}

#[tokio::test]
async fn registration_ack_reports_the_outcome() {
  let mut fx = Fixture::new();
  let _a = fx.add("a", path_def("x"));
  let handle = fx.run();

  let taken =
    fx.pc
      .spawn_async_with_id(fx.pc.alloc_id(), path_def("x"), |_, _| async {});
  assert!(matches!(
    taken.await,
    Ok(Err(RegisterError::PathTaken(ref key))) if key.path.as_str() == "x"
  ));

  let free =
    fx.pc
      .spawn_async_with_id(fx.pc.alloc_id(), path_def("y"), |_, _| async {});
  assert_eq!(free.await, Ok(Ok(())));

  let under = fx.pc.spawn_async_with_id(
    fx.pc.alloc_id(),
    path_def("x/a"),
    |_, _| async {},
  );
  let Ok(Err(err)) = under.await else {
    panic!("expected a refusal");
  };
  assert_eq!(
    err.to_string(),
    "a task can't have tasks under it: 'x/a' is under 'x'"
  );

  fx.quit(handle).await;
}

#[tokio::test]
async fn start_pulls_dependencies_up_in_order() {
  let mut fx = Fixture::new();
  let dep = fx.add("dep", path_def("dep"));
  let app = fx.add(
    "app",
    TaskDef {
      deps: vec![TaskSelector::Id(dep)],
      ..path_def("app")
    },
  );
  let handle = fx.run();

  fx.pc
    .send(KernelCommand::Start(TaskSelector::Id(app), None));
  assert_eq!(fx.recv().await, ("dep", RecordedCmd::Start));
  assert_eq!(fx.recv().await, ("app", RecordedCmd::Start));

  fx.quit(handle).await;
}

#[tokio::test]
async fn registering_pinned_task_starts_it() {
  let mut fx = Fixture::new();
  fx.add(
    "a",
    TaskDef {
      pinned: true,
      ..path_def("a")
    },
  );
  let handle = fx.run();

  fx.flush().await;
  assert_eq!(fx.recv().await, ("a", RecordedCmd::Start));

  fx.quit(handle).await;
}

#[tokio::test]
async fn veto_breaks_dependents_leaf_first() {
  let mut fx = Fixture::new();
  let dep = fx.add("dep", path_def("dep"));
  let app = fx.add(
    "app",
    TaskDef {
      deps: vec![TaskSelector::Id(dep)],
      ..path_def("app")
    },
  );
  let handle = fx.run();

  fx.pc
    .send(KernelCommand::Start(TaskSelector::Id(app), None));
  assert_eq!(fx.recv().await, ("dep", RecordedCmd::Start));
  assert_eq!(fx.recv().await, ("app", RecordedCmd::Start));

  // Keeping the dep down takes the dependent down first.
  fx.pc.send(KernelCommand::Veto(TaskSelector::Id(dep), None));
  assert_eq!(fx.recv().await, ("app", RecordedCmd::Stop));
  assert_eq!(fx.recv().await, ("dep", RecordedCmd::Stop));

  // The dependent stays wanted but blocked; starting the dep again brings
  // both back.
  fx.pc
    .send(KernelCommand::Start(TaskSelector::Id(dep), None));
  assert_eq!(fx.recv().await, ("dep", RecordedCmd::Start));
  assert_eq!(fx.recv().await, ("app", RecordedCmd::Start));

  fx.quit(handle).await;
}

#[tokio::test]
async fn start_of_dependent_releases_vetoed_dep() {
  let mut fx = Fixture::new();
  let dep = fx.add("dep", path_def("dep"));
  let app = fx.add(
    "app",
    TaskDef {
      deps: vec![TaskSelector::Id(dep)],
      ..path_def("app")
    },
  );
  let handle = fx.run();

  fx.pc
    .send(KernelCommand::Start(TaskSelector::Id(app), None));
  assert_eq!(fx.recv().await, ("dep", RecordedCmd::Start));
  assert_eq!(fx.recv().await, ("app", RecordedCmd::Start));

  // Keep the dep down: dependent breaks first.
  fx.pc.send(KernelCommand::Veto(TaskSelector::Id(dep), None));
  assert_eq!(fx.recv().await, ("app", RecordedCmd::Stop));
  assert_eq!(fx.recv().await, ("dep", RecordedCmd::Stop));

  // Starting the dependent demands the dep: it is released and both come
  // back, dep first.
  fx.pc
    .send(KernelCommand::Start(TaskSelector::Id(app), None));
  assert_eq!(fx.recv().await, ("dep", RecordedCmd::Start));
  assert_eq!(fx.recv().await, ("app", RecordedCmd::Start));

  fx.quit(handle).await;
}

#[tokio::test]
async fn start_of_dependent_revives_exited_dep() {
  let mut fx = Fixture::new();
  let dep = fx.add("dep", path_def("dep"));
  let app = fx.add(
    "app",
    TaskDef {
      deps: vec![TaskSelector::Id(dep)],
      ..path_def("app")
    },
  );
  let handle = fx.run();

  fx.pc
    .send(KernelCommand::Start(TaskSelector::Id(app), None));
  assert_eq!(fx.recv().await, ("dep", RecordedCmd::Start));
  assert_eq!(fx.recv().await, ("app", RecordedCmd::Start));

  // The dep dies on its own (restart: Never => Exited); the dependent
  // breaks and waits.
  fx.pc.send_msg(dep, Report::Stopped(ExitInfo::code(0)));
  assert_eq!(fx.recv().await, ("app", RecordedCmd::Stop));
  fx.flush().await;
  fx.assert_no_cmd();

  fx.pc
    .send(KernelCommand::Start(TaskSelector::Id(app), None));
  assert_eq!(fx.recv().await, ("dep", RecordedCmd::Start));
  assert_eq!(fx.recv().await, ("app", RecordedCmd::Start));

  fx.quit(handle).await;
}

#[tokio::test]
async fn start_of_dependent_does_not_rerun_done_job() {
  let mut fx = Fixture::new();
  let job = fx.add(
    "job",
    TaskDef {
      kind: TaskKind::Job,
      ..path_def("job")
    },
  );
  let app = fx.add(
    "app",
    TaskDef {
      deps: vec![TaskSelector::Id(job)],
      ..path_def("app")
    },
  );
  let handle = fx.run();

  fx.pc
    .send(KernelCommand::Start(TaskSelector::Id(app), None));
  assert_eq!(fx.recv().await, ("job", RecordedCmd::Start));
  fx.pc.send_msg(job, Report::Stopped(ExitInfo::code(0)));
  assert_eq!(fx.recv().await, ("app", RecordedCmd::Start));

  // Cycling the dependent leaves the completed job alone.
  fx.pc
    .send(KernelCommand::Restart(TaskSelector::Id(app), None));
  assert_eq!(fx.recv().await, ("app", RecordedCmd::Stop));
  assert_eq!(fx.recv().await, ("app", RecordedCmd::Start));
  fx.flush().await;
  fx.assert_no_cmd();

  fx.quit(handle).await;
}

#[tokio::test]
async fn unpin_keeps_task_wanted_by_another() {
  let mut fx = Fixture::new();
  let dep = fx.add("dep", path_def("dep"));
  let app = fx.add(
    "app",
    TaskDef {
      deps: vec![TaskSelector::Id(dep)],
      ..path_def("app")
    },
  );
  let handle = fx.run();

  fx.pc
    .send(KernelCommand::Start(TaskSelector::Id(app), None));
  fx.pc
    .send(KernelCommand::Start(TaskSelector::Id(dep), None));
  assert_eq!(fx.recv().await, ("dep", RecordedCmd::Start));
  assert_eq!(fx.recv().await, ("app", RecordedCmd::Start));

  // Unpinning the dep is a no-op while the app still wants it.
  fx.pc
    .send(KernelCommand::Unpin(TaskSelector::Id(dep), None));
  fx.flush().await;
  fx.assert_no_cmd();

  // Unpinning the app winds both down, dependent first.
  fx.pc
    .send(KernelCommand::Unpin(TaskSelector::Id(app), None));
  assert_eq!(fx.recv().await, ("app", RecordedCmd::Stop));
  assert_eq!(fx.recv().await, ("dep", RecordedCmd::Stop));

  fx.quit(handle).await;
}

#[tokio::test]
async fn dependent_waits_for_readiness() {
  let mut fx = Fixture::new();
  let dep = fx.add(
    "dep",
    TaskDef {
      ready: ReadyMode::Reported { timeout: None },
      ..path_def("dep")
    },
  );
  let app = fx.add(
    "app",
    TaskDef {
      deps: vec![TaskSelector::Id(dep)],
      ..path_def("app")
    },
  );
  let handle = fx.run();

  fx.pc
    .send(KernelCommand::Start(TaskSelector::Id(app), None));
  assert_eq!(fx.recv().await, ("dep", RecordedCmd::Start));
  fx.flush().await;
  fx.assert_no_cmd();

  fx.pc.send_msg(dep, Report::Ready);
  assert_eq!(fx.recv().await, ("app", RecordedCmd::Start));

  fx.quit(handle).await;
}

#[tokio::test]
async fn job_satisfies_dependents_only_when_done() {
  let mut fx = Fixture::new();
  let job = fx.add(
    "job",
    TaskDef {
      kind: TaskKind::Job,
      ..path_def("job")
    },
  );
  let app = fx.add(
    "app",
    TaskDef {
      deps: vec![TaskSelector::Id(job)],
      ..path_def("app")
    },
  );
  let handle = fx.run();

  fx.pc
    .send(KernelCommand::Start(TaskSelector::Id(app), None));
  assert_eq!(fx.recv().await, ("job", RecordedCmd::Start));
  fx.flush().await;
  fx.assert_no_cmd();

  // The job completing successfully unblocks the dependent and does not
  // get restarted.
  fx.pc.send_msg(job, Report::Stopped(ExitInfo::code(0)));
  assert_eq!(fx.recv().await, ("app", RecordedCmd::Start));
  fx.flush().await;
  fx.assert_no_cmd();

  fx.quit(handle).await;
}

#[tokio::test]
async fn crash_restarts_with_backoff() {
  let mut fx = Fixture::new();
  let a = fx.add(
    "a",
    TaskDef {
      restart: RestartMode::OnFailure,
      ..path_def("a")
    },
  );
  let handle = fx.run();

  fx.pc.send(KernelCommand::Start(TaskSelector::Id(a), None));
  assert_eq!(fx.recv().await, ("a", RecordedCmd::Start));

  fx.pc.send_msg(a, Report::Stopped(ExitInfo::code(1)));
  // Restarted after the backoff delay.
  assert_eq!(fx.recv().await, ("a", RecordedCmd::Start));

  fx.quit(handle).await;
}

/// A long run resets the backoff count only if the task got ready. The
/// kernel's clock is std time, so the test moves it by hand.
#[test]
fn only_a_ready_run_resets_the_backoff() {
  let mut kernel = Kernel::new();
  let id = TaskId(1);
  kernel
    .graph
    .register_task_with_id(
      id,
      TaskDef {
        ready: ReadyMode::Reported { timeout: None },
        restart: RestartMode::OnFailure,
        ..path_def("a")
      },
      Box::new(|_| Box::new(crate::kernel::task::TargetTask)),
      None,
    )
    .unwrap();
  let graph = &mut kernel.graph;
  for (state, attempts) in [(TaskState::Running, 3), (TaskState::Ready, 1)] {
    let task = graph.tasks.get_mut(&id).unwrap();
    task.state = state;
    task.attempts = 2;
    task.last_start = Some(graph.now);
    graph.now += BACKOFF_RESET * 2;
    graph.on_task_stopped(id, ExitInfo::code(1));
    assert_eq!(
      graph.tasks[&id].state,
      TaskState::Backoff(ExitInfo::code(1)),
      "{state:?}"
    );
    assert_eq!(graph.tasks[&id].attempts, attempts, "{state:?}");
  }
}

/// A restored Running task waits on the config's ready check, which may
/// have changed since it was saved.
#[test]
fn restored_running_task_follows_the_new_ready_check() {
  let saved = snap::Task {
    id: 1,
    space: String::new(),
    path: Some("a".to_string()),
    label: None,
    tags: Vec::new(),
    pinned: false,
    deps: Vec::new(),
    restart: snap::Restart::Never,
    job: false,
    ready_timeout_ms: None,
    stop_timeout_ms: None,
    state: snap::TaskState::Running {},
    vetoed: false,
    killed: false,
    start_failed: false,
    saved_pin: false,
    attempts: 0,
    last_start_secs_ago: Some(40),
    timer_ms: Some(5000),
    spec: None,
    run_spec: None,
    origin: None,
    kind: TaskKindSnapshot::Console {
      groups: Default::default(),
    },
  };
  let secs = Duration::from_secs;
  for (ready, state, timer) in [
    (ReadyMode::Immediate, TaskState::Ready, None),
    (
      ReadyMode::Reported { timeout: None },
      TaskState::Running,
      None,
    ),
    (
      ReadyMode::Reported {
        timeout: Some(secs(60)),
      },
      TaskState::Running,
      Some(secs(20)),
    ),
    (
      ReadyMode::Reported {
        timeout: Some(secs(30)),
      },
      TaskState::Running,
      Some(Duration::ZERO),
    ),
  ] {
    let mut kernel = Kernel::new();
    kernel
      .graph
      .register_task_with_id(
        TaskId(1),
        TaskDef {
          ready,
          ..path_def("a")
        },
        Box::new(|_| Box::new(crate::kernel::task::TargetTask)),
        Some(&saved),
      )
      .unwrap();
    assert_eq!(kernel.graph.tasks[&TaskId(1)].state, state, "{ready:?}");
    let timer_left = kernel.graph.timer(TaskId(1)).map(|(_, left)| left);
    assert_eq!(timer_left, timer, "{ready:?}");
  }
}

/// A quit's stops take at most the heaviest chain of active deps, each
/// with its grace and the kill's wait.
#[test]
fn stop_within_is_the_heaviest_active_chain() {
  let mut kernel = Kernel::new();
  let secs = Duration::from_secs;
  for (id, deps, stop_timeout, state) in [
    (1, vec![], secs(5), TaskState::Ready),
    (2, vec![1], secs(30), TaskState::Running),
    // Not running: nothing to stop.
    (3, vec![2], secs(60), TaskState::Idle),
    (4, vec![], secs(20), TaskState::Ready),
  ] {
    kernel
      .graph
      .register_task_with_id(
        TaskId(id),
        TaskDef {
          stop_timeout,
          deps: deps
            .into_iter()
            .map(|d| TaskSelector::Id(TaskId(d)))
            .collect(),
          pinned: true,
          ..path_def(&format!("t{id}"))
        },
        Box::new(|_| Box::new(crate::kernel::task::TargetTask)),
        None,
      )
      .unwrap();
    kernel.graph.tasks.get_mut(&TaskId(id)).unwrap().state = state;
  }
  // t2 then t1: 30s + 10s, then 5s + 10s.
  assert_eq!(kernel.graph.stop_within(), secs(55));
}

#[tokio::test]
async fn clean_exit_does_not_restart() {
  let mut fx = Fixture::new();
  let a = fx.add(
    "a",
    TaskDef {
      restart: RestartMode::OnFailure,
      ..path_def("a")
    },
  );
  let handle = fx.run();

  fx.pc.send(KernelCommand::Start(TaskSelector::Id(a), None));
  assert_eq!(fx.recv().await, ("a", RecordedCmd::Start));

  fx.pc.send_msg(a, Report::Stopped(ExitInfo::code(0)));
  fx.flush().await;
  fx.assert_no_cmd();

  fx.quit(handle).await;
}

#[tokio::test]
async fn always_restarts_after_a_clean_exit() {
  let mut fx = Fixture::new();
  let a = fx.add(
    "a",
    TaskDef {
      restart: RestartMode::Always,
      ..path_def("a")
    },
  );
  let handle = fx.run();

  fx.pc.send(KernelCommand::Start(TaskSelector::Id(a), None));
  assert_eq!(fx.recv().await, ("a", RecordedCmd::Start));

  fx.pc.send_msg(a, Report::Stopped(ExitInfo::code(0)));
  assert_eq!(fx.recv().await, ("a", RecordedCmd::Start));

  fx.quit(handle).await;
}

/// A stop dekit was asked for is never a failure, whatever the exit.
#[tokio::test(start_paused = true)]
async fn commanded_stop_never_autorestarts() {
  let mut fx = Fixture::new();
  let tx = fx.tx.clone();
  // Exits with 1 when stopped.
  let a = fx.kernel.as_mut().unwrap().register_task(
    TaskDef {
      restart: RestartMode::Always,
      ..path_def("a")
    },
    move |_| Box::new(ExitOnNotify { name: "a", tx }),
  );
  let handle = fx.run();

  fx.pc.send(KernelCommand::Start(TaskSelector::Id(a), None));
  assert_eq!(fx.recv().await, ("a", RecordedCmd::Start));
  fx.pc.send(KernelCommand::Stop(TaskSelector::Id(a), None));
  assert_eq!(fx.recv().await, ("a", RecordedCmd::Stop));
  tokio::time::advance(Duration::from_secs(60)).await;
  fx.flush().await;
  fx.assert_no_cmd();
  assert_eq!(state_of(&fx.pc, a).await, Some(TaskState::Idle));

  fx.quit(handle).await;
}

#[tokio::test]
async fn restart_cycles_task() {
  let mut fx = Fixture::new();
  let a = fx.add("a", path_def("a"));
  let handle = fx.run();

  fx.pc.send(KernelCommand::Start(TaskSelector::Id(a), None));
  assert_eq!(fx.recv().await, ("a", RecordedCmd::Start));

  fx.pc
    .send(KernelCommand::Restart(TaskSelector::Id(a), None));
  assert_eq!(fx.recv().await, ("a", RecordedCmd::Stop));
  assert_eq!(fx.recv().await, ("a", RecordedCmd::Start));

  // Restart on a stopped, unpinned task starts it.
  fx.pc.send(KernelCommand::Stop(TaskSelector::Id(a), None));
  assert_eq!(fx.recv().await, ("a", RecordedCmd::Stop));
  fx.pc
    .send(KernelCommand::Restart(TaskSelector::Id(a), None));
  assert_eq!(fx.recv().await, ("a", RecordedCmd::Start));

  fx.quit(handle).await;
}

#[tokio::test]
async fn stop_of_leaf_keeps_it_down_until_started() {
  let mut fx = Fixture::new();
  let a = fx.add("a", path_def("a"));
  let handle = fx.run();

  fx.pc.send(KernelCommand::Start(TaskSelector::Id(a), None));
  assert_eq!(fx.recv().await, ("a", RecordedCmd::Start));

  // Nothing wants the task once the stop unpins it.
  fx.pc.send(KernelCommand::Stop(TaskSelector::Id(a), None));
  assert_eq!(fx.recv().await, ("a", RecordedCmd::Stop));
  fx.flush().await;
  fx.assert_no_cmd();

  fx.pc.send(KernelCommand::Start(TaskSelector::Id(a), None));
  assert_eq!(fx.recv().await, ("a", RecordedCmd::Start));

  fx.quit(handle).await;
}

#[tokio::test]
async fn quit_stops_tasks_in_reverse_dependency_order() {
  let mut fx = Fixture::new();
  let dep = fx.add("dep", path_def("dep"));
  let app = fx.add(
    "app",
    TaskDef {
      deps: vec![TaskSelector::Id(dep)],
      ..path_def("app")
    },
  );
  let handle = fx.run();

  fx.pc
    .send(KernelCommand::Start(TaskSelector::Id(app), None));
  assert_eq!(fx.recv().await, ("dep", RecordedCmd::Start));
  assert_eq!(fx.recv().await, ("app", RecordedCmd::Start));

  fx.pc.send(KernelCommand::Quit);
  assert_eq!(fx.recv().await, ("app", RecordedCmd::Stop));
  assert_eq!(fx.recv().await, ("dep", RecordedCmd::Stop));
  tokio::time::timeout(Duration::from_secs(1), handle)
    .await
    .expect("timed out waiting for kernel to quit")
    .unwrap();
}

#[test]
fn registration_with_missing_dep_is_refused() {
  let mut fx = Fixture::new();
  let mut kernel = fx.kernel.take().unwrap();
  let dep_id = fx.pc.alloc_id();
  let app_id = fx.pc.alloc_id();

  // Dep not registered: the whole registration is refused, nothing is
  // claimed.
  let tx = fx.tx.clone();
  let registered = kernel.graph.register_task_with_id(
    app_id,
    TaskDef {
      deps: vec![TaskSelector::Id(dep_id)],
      ..path_def("app")
    },
    Box::new(move |ctx| {
      Box::new(RecordingTask {
        name: "app",
        tx,
        ctx,
      })
    }),
    None,
  );
  assert_eq!(
    registered,
    Err(RegisterError::MissingDep(TaskSelector::Id(dep_id)))
  );
  assert!(!kernel.graph.tasks.contains_key(&app_id));
  assert!(
    kernel
      .graph
      .matching_ids(&TaskSelector::Glob(
        SpaceSelector::default_space(),
        "app".to_string()
      ))
      .is_empty()
  );

  // Dep first, then the app registers and starts behind it.
  let tx = fx.tx.clone();
  assert!(
    kernel
      .graph
      .register_task_with_id(
        dep_id,
        path_def("dep"),
        Box::new(move |ctx| Box::new(RecordingTask {
          name: "dep",
          tx,
          ctx
        })),
        None,
      )
      .is_ok()
  );
  let tx = fx.tx.clone();
  assert!(
    kernel
      .graph
      .register_task_with_id(
        app_id,
        TaskDef {
          deps: vec![TaskSelector::Id(dep_id)],
          ..path_def("app")
        },
        Box::new(move |ctx| Box::new(RecordingTask {
          name: "app",
          tx,
          ctx
        })),
        None,
      )
      .is_ok()
  );
  turn(
    &mut kernel,
    KernelCommand::Start(TaskSelector::Id(app_id), None),
  );
  assert_eq!(fx.rx.try_recv().unwrap(), ("dep", RecordedCmd::Start));
  assert_eq!(fx.rx.try_recv().unwrap(), ("app", RecordedCmd::Start));
}

#[test]
fn add_edge_to_unregistered_id_is_refused() {
  let mut fx = Fixture::new();
  let a = fx.add("a", path_def("a"));
  let mut kernel = fx.kernel.take().unwrap();

  turn(&mut kernel, KernelCommand::Start(TaskSelector::Id(a), None));
  assert_eq!(fx.rx.try_recv().unwrap(), ("a", RecordedCmd::Start));

  // No edge to something that does not exist; `a` stays up.
  let dep_id = fx.pc.alloc_id();
  kernel.graph.add_edge(a, dep_id);
  kernel.graph.settle();
  assert!(
    !kernel
      .graph
      .edges
      .get(&a)
      .is_some_and(|s| s.contains(&dep_id)),
    "edge to an unregistered id was added"
  );
  assert!(fx.rx.try_recv().is_err(), "task was disturbed");
}

/// A batch is listed in the order given, whatever order its dependencies
/// register it in. A refused task leaves no place behind, and a task
/// that names another is listed right after it.
#[tokio::test]
async fn lists_a_batch_in_the_order_given() {
  let mut fx = Fixture::new();
  let handle = fx.run();
  let pc = fx.pc.clone();

  let (tx, mut added) = unbounded_channel();
  let (subscribed_tx, subscribed_rx) = tokio::sync::oneshot::channel();
  let ack = pc.spawn_async_with_id(
    pc.alloc_id(),
    TaskDef::default(),
    move |pc, mut cmds| async move {
      pc.subscribe_path(
        TaskKey::default_space(TaskPath::root()),
        SubMode::Subtree,
      );
      subscribed_tx.send(()).unwrap();
      while let Some(cmd) = cmds.recv().await {
        match cmd {
          TaskCmd::Freeze(number) => pc.send(KernelCommand::TaskFrozen(
            number,
            TaskKindSnapshot::Console {
              groups: Default::default(),
            },
          )),
          TaskCmd::Msg(msg) => {
            if let Ok(n) = msg.downcast::<TaskNotification>()
              && let TaskNotify::Added { after, .. } = n.notify
            {
              tx.send((n.from, after)).unwrap();
            }
          }
          TaskCmd::Start(_) | TaskCmd::Stop | TaskCmd::Kill | TaskCmd::Thaw => {
          }
        }
      }
    },
  );
  assert!(ack.await.unwrap().is_ok());
  subscribed_rx.await.unwrap();

  let registration = |id: TaskId, def: TaskDef| {
    let tx = fx.tx.clone();
    TaskRegistration {
      task_id: id,
      def,
      factory: Box::new(move |ctx| {
        Box::new(RecordingTask {
          name: "task",
          tx,
          ctx,
        })
      }),
    }
  };
  let ids: Vec<TaskId> = (0..5).map(|_| pc.alloc_id()).collect();
  let [refused, web, api_a, db, api_b] = ids[..] else {
    unreachable!()
  };
  let needs = |path: &str, dep: TaskId| TaskDef {
    deps: vec![TaskSelector::Id(dep)],
    ..path_def(path)
  };
  let results = pc
    .register_tasks(vec![
      registration(refused, needs("gone/x", TaskId(usize::MAX))).into(),
      registration(web, needs("web", db)).into(),
      registration(api_a, needs("api/a", db)).into(),
      registration(db, path_def("db")).into(),
      registration(api_b, path_def("api/b")).into(),
    ])
    .await
    .unwrap();
  let registered: Vec<bool> = results.iter().map(Result::is_ok).collect();
  assert_eq!(registered, [false, true, true, true, true]);

  // Each task is announced with the task listed before it at that time.
  let mut announced = Vec::new();
  for _ in 0..4 {
    announced.push(added.recv().await.unwrap());
  }
  assert_eq!(
    announced,
    [(db, None), (api_b, None), (web, None), (api_a, Some(web))]
  );

  let copy = pc.alloc_id();
  let def = TaskDef {
    place: Place::After(TaskPath::new("api/a").unwrap()),
    ..path_def("api/a-2")
  };
  let ack = pc.register_task(registration(copy, def));
  assert!(ack.await.unwrap().is_ok());
  assert_eq!(added.recv().await.unwrap(), (copy, Some(api_a)));
  let last = pc.alloc_id();
  let ack = pc.register_task(registration(last, path_def("gone/y")));
  assert!(ack.await.unwrap().is_ok());
  assert_eq!(added.recv().await.unwrap(), (last, Some(db)));

  let listed = ["web", "api/a", "api/a-2", "api/b", "db", "gone/y"];
  let tasks = pc
    .list_tasks(TaskSelector::Glob(
      SpaceSelector::default_space(),
      "**".to_string(),
    ))
    .await;
  let names: Vec<String> = tasks.iter().map(|task| task.name()).collect();
  assert_eq!(names, listed);

  let snapshot = freeze(&fx).await;
  let saved: Vec<&str> = snapshot
    .tasks
    .iter()
    .filter_map(|task| task.path.as_deref())
    .collect();
  assert_eq!(saved, listed);

  fx.pc.send(KernelCommand::Thaw);
  fx.quit(handle).await;
}

async fn label_of(pc: &TaskContext, id: TaskId) -> Option<String> {
  let rx = pc.list_tasks(TaskSelector::all());
  let list = tokio::time::timeout(Duration::from_secs(1), rx)
    .await
    .expect("timed out listing tasks");
  list.into_iter().find(|t| t.id == id).and_then(|t| t.label)
}

#[tokio::test]
async fn task_label_is_stored_and_updatable() {
  let mut fx = Fixture::new();
  // The label may hold characters that aren't valid in a path (spaces).
  let id = fx.add(
    "a",
    TaskDef {
      label: Some("web server".to_string()),
      ..path_def("1")
    },
  );
  let handle = fx.run();

  assert_eq!(label_of(&fx.pc, id).await.as_deref(), Some("web server"));

  fx.pc.send(KernelCommand::SetLabel(
    TaskSelector::Id(id),
    Some("renamed".to_string()),
    None,
  ));
  assert_eq!(label_of(&fx.pc, id).await.as_deref(), Some("renamed"));

  fx.quit(handle).await;
}

async fn state_of(pc: &TaskContext, id: TaskId) -> Option<TaskState> {
  let rx = pc.list_tasks(TaskSelector::all());
  let list = tokio::time::timeout(Duration::from_secs(1), rx)
    .await
    .expect("timed out listing tasks");
  list.into_iter().find(|t| t.id == id).map(|t| t.state)
}

async fn resolve(pc: &TaskContext, path: &str) -> Option<TaskId> {
  resolve_in(pc, TaskSpaceId::default_space(), path).await
}

async fn resolve_in(
  pc: &TaskContext,
  space: TaskSpaceId,
  path: &str,
) -> Option<TaskId> {
  let rx = pc.list_tasks(TaskSelector::Glob(
    SpaceSelector::One(space),
    path.to_string(),
  ));
  let tasks = tokio::time::timeout(Duration::from_secs(1), rx)
    .await
    .expect("timed out resolving path");
  tasks.first().map(|t| t.id)
}

#[tokio::test]
async fn register_path_conflict_keeps_owner() {
  let mut fx = Fixture::new();
  let a = fx.add("a", path_def("x"));
  let b = fx.add("b", path_def("x"));
  let handle = fx.run();

  // The loser is registered without a path.
  assert_eq!(resolve(&fx.pc, "x").await, Some(a));

  // Removing the loser must not free the owner's path.
  fx.pc.send(KernelCommand::Remove(TaskSelector::Id(b), None));
  assert_eq!(resolve(&fx.pc, "x").await, Some(a));

  fx.quit(handle).await;
}

#[tokio::test]
async fn stale_started_report_is_ignored() {
  let mut fx = Fixture::new();
  let a = fx.add("a", path_def("a"));
  let handle = fx.run();

  fx.pc.send(KernelCommand::Start(TaskSelector::Id(a), None));
  assert_eq!(fx.recv().await, ("a", RecordedCmd::Start));

  fx.pc.send(KernelCommand::Stop(TaskSelector::Id(a), None));
  assert_eq!(fx.recv().await, ("a", RecordedCmd::Stop));

  // A started report that was in flight when the stop landed must not
  // resurrect the task (or stop it again).
  fx.pc.send_msg(a, Report::Started);
  fx.flush().await;
  fx.assert_no_cmd();

  // The task still starts normally when demanded again.
  fx.pc.send(KernelCommand::Start(TaskSelector::Id(a), None));
  assert_eq!(fx.recv().await, ("a", RecordedCmd::Start));

  fx.quit(handle).await;
}

#[tokio::test]
async fn kill_hard_kills_and_unpins() {
  let mut fx = Fixture::new();
  let a = fx.add("a", path_def("a"));
  let handle = fx.run();

  fx.pc.send(KernelCommand::Start(TaskSelector::Id(a), None));
  assert_eq!(fx.recv().await, ("a", RecordedCmd::Start));

  // Kill skips the graceful stop; the unpin keeps the task down.
  fx.pc.send(KernelCommand::Kill(TaskSelector::Id(a), None));
  assert_eq!(fx.recv().await, ("a", RecordedCmd::Kill));
  fx.flush().await;
  fx.assert_no_cmd();

  fx.pc.send(KernelCommand::Start(TaskSelector::Id(a), None));
  assert_eq!(fx.recv().await, ("a", RecordedCmd::Start));

  fx.quit(handle).await;
}

#[tokio::test]
async fn dep_crash_breaks_dependents_in_order_and_recovers() {
  let mut fx = Fixture::new();
  let c = fx.add(
    "c",
    TaskDef {
      restart: RestartMode::OnFailure,
      ..path_def("c")
    },
  );
  let b = fx.add(
    "b",
    TaskDef {
      deps: vec![TaskSelector::Id(c)],
      ..path_def("b")
    },
  );
  let a = fx.add(
    "a",
    TaskDef {
      deps: vec![TaskSelector::Id(b)],
      ..path_def("a")
    },
  );
  let handle = fx.run();

  fx.pc.send(KernelCommand::Start(TaskSelector::Id(a), None));
  assert_eq!(fx.recv().await, ("c", RecordedCmd::Start));
  assert_eq!(fx.recv().await, ("b", RecordedCmd::Start));
  assert_eq!(fx.recv().await, ("a", RecordedCmd::Start));

  // The crash breaks dependents top-down; after the backoff retry the
  // whole chain returns bottom-up.
  fx.pc.send_msg(c, Report::Stopped(ExitInfo::code(1)));
  assert_eq!(fx.recv().await, ("a", RecordedCmd::Stop));
  assert_eq!(fx.recv().await, ("b", RecordedCmd::Stop));
  assert_eq!(fx.recv().await, ("c", RecordedCmd::Start));
  assert_eq!(fx.recv().await, ("b", RecordedCmd::Start));
  assert_eq!(fx.recv().await, ("a", RecordedCmd::Start));

  fx.quit(handle).await;
}

#[tokio::test]
async fn veto_of_leaf_dep_tears_down_chain_in_order() {
  let mut fx = Fixture::new();
  let c = fx.add("c", path_def("c"));
  let b = fx.add(
    "b",
    TaskDef {
      deps: vec![TaskSelector::Id(c)],
      ..path_def("b")
    },
  );
  let a = fx.add(
    "a",
    TaskDef {
      deps: vec![TaskSelector::Id(b)],
      ..path_def("a")
    },
  );
  let handle = fx.run();

  fx.pc.send(KernelCommand::Start(TaskSelector::Id(a), None));
  assert_eq!(fx.recv().await, ("c", RecordedCmd::Start));
  assert_eq!(fx.recv().await, ("b", RecordedCmd::Start));
  assert_eq!(fx.recv().await, ("a", RecordedCmd::Start));

  // Keeping the deepest dep down unwinds the chain dependents-first.
  fx.pc.send(KernelCommand::Veto(TaskSelector::Id(c), None));
  assert_eq!(fx.recv().await, ("a", RecordedCmd::Stop));
  assert_eq!(fx.recv().await, ("b", RecordedCmd::Stop));
  assert_eq!(fx.recv().await, ("c", RecordedCmd::Stop));

  fx.quit(handle).await;
}

#[tokio::test]
async fn stop_of_required_task_bounces_it() {
  let mut fx = Fixture::new();
  let dep = fx.add("dep", path_def("dep"));
  let app = fx.add(
    "app",
    TaskDef {
      deps: vec![TaskSelector::Id(dep)],
      ..path_def("app")
    },
  );
  let handle = fx.run();

  fx.pc
    .send(KernelCommand::Start(TaskSelector::Id(app), None));
  assert_eq!(fx.recv().await, ("dep", RecordedCmd::Start));
  assert_eq!(fx.recv().await, ("app", RecordedCmd::Start));

  // The app still wants the dep, so the stop is a bounce: the dep is
  // stopped directly, the app breaks and recovers along the way. The
  // middle two land in one reconcile pass, so their order is not defined.
  fx.pc.send(KernelCommand::Stop(TaskSelector::Id(dep), None));
  assert_eq!(fx.recv().await, ("dep", RecordedCmd::Stop));
  let mut cmds = [fx.recv().await, fx.recv().await];
  cmds.sort();
  assert_eq!(
    cmds,
    [("app", RecordedCmd::Stop), ("dep", RecordedCmd::Start)]
  );
  assert_eq!(fx.recv().await, ("app", RecordedCmd::Start));

  fx.quit(handle).await;
}

#[tokio::test]
async fn restart_of_dep_bounces_it_and_its_dependent() {
  let mut fx = Fixture::new();
  let dep = fx.add("dep", path_def("dep"));
  let app = fx.add(
    "app",
    TaskDef {
      deps: vec![TaskSelector::Id(dep)],
      ..path_def("app")
    },
  );
  let handle = fx.run();

  fx.pc
    .send(KernelCommand::Start(TaskSelector::Id(app), None));
  assert_eq!(fx.recv().await, ("dep", RecordedCmd::Start));
  assert_eq!(fx.recv().await, ("app", RecordedCmd::Start));

  // The dep is stopped directly; the dependent breaks and recovers once
  // the dep is ready again.
  fx.pc
    .send(KernelCommand::Restart(TaskSelector::Id(dep), None));
  assert_eq!(fx.recv().await, ("dep", RecordedCmd::Stop));
  let mut cmds = [fx.recv().await, fx.recv().await];
  cmds.sort();
  assert_eq!(
    cmds,
    [("app", RecordedCmd::Stop), ("dep", RecordedCmd::Start)]
  );
  assert_eq!(fx.recv().await, ("app", RecordedCmd::Start));

  fx.quit(handle).await;
}

#[tokio::test]
async fn restart_pins_like_start() {
  let mut fx = Fixture::new();
  let dep = fx.add("dep", path_def("dep"));
  let app = fx.add(
    "app",
    TaskDef {
      deps: vec![TaskSelector::Id(dep)],
      ..path_def("app")
    },
  );
  let handle = fx.run();

  fx.pc
    .send(KernelCommand::Start(TaskSelector::Id(app), None));
  assert_eq!(fx.recv().await, ("dep", RecordedCmd::Start));
  assert_eq!(fx.recv().await, ("app", RecordedCmd::Start));

  fx.pc
    .send(KernelCommand::Restart(TaskSelector::Id(dep), None));
  assert_eq!(fx.recv().await, ("dep", RecordedCmd::Stop));
  let mut cmds = [fx.recv().await, fx.recv().await];
  cmds.sort();
  assert_eq!(
    cmds,
    [("app", RecordedCmd::Stop), ("dep", RecordedCmd::Start)]
  );
  assert_eq!(fx.recv().await, ("app", RecordedCmd::Start));

  // The restart pinned the dep, so it survives its dependent going away.
  fx.pc
    .send(KernelCommand::Unpin(TaskSelector::Id(app), None));
  assert_eq!(fx.recv().await, ("app", RecordedCmd::Stop));
  fx.flush().await;
  fx.assert_no_cmd();

  fx.quit(handle).await;
}

#[tokio::test]
async fn stop_unpins_so_revival_is_temporary() {
  let mut fx = Fixture::new();
  let a = fx.add("a", path_def("a"));
  let b = fx.add(
    "b",
    TaskDef {
      deps: vec![TaskSelector::Id(a)],
      ..path_def("b")
    },
  );
  let handle = fx.run();

  // Pin a, then stop it: the stop also unpins.
  fx.pc.send(KernelCommand::Start(TaskSelector::Id(a), None));
  assert_eq!(fx.recv().await, ("a", RecordedCmd::Start));
  fx.pc.send(KernelCommand::Stop(TaskSelector::Id(a), None));
  assert_eq!(fx.recv().await, ("a", RecordedCmd::Stop));

  // Starting a dependent revives a, but only while b wants it.
  fx.pc.send(KernelCommand::Start(TaskSelector::Id(b), None));
  assert_eq!(fx.recv().await, ("a", RecordedCmd::Start));
  assert_eq!(fx.recv().await, ("b", RecordedCmd::Start));

  fx.pc.send(KernelCommand::Unpin(TaskSelector::Id(b), None));
  assert_eq!(fx.recv().await, ("b", RecordedCmd::Stop));
  assert_eq!(fx.recv().await, ("a", RecordedCmd::Stop));

  fx.quit(handle).await;
}

#[tokio::test]
async fn remove_of_running_task_hard_kills_it() {
  let mut fx = Fixture::new();
  let a = fx.add("a", path_def("a"));
  let handle = fx.run();

  fx.pc.send(KernelCommand::Start(TaskSelector::Id(a), None));
  assert_eq!(fx.recv().await, ("a", RecordedCmd::Start));

  fx.pc.send(KernelCommand::Remove(TaskSelector::Id(a), None));
  assert_eq!(fx.recv().await, ("a", RecordedCmd::Kill));
  assert_eq!(state_of(&fx.pc, a).await, None);

  fx.quit(handle).await;
}

#[tokio::test]
async fn dead_channel_task_is_marked_exited() {
  use super::super::task::ChannelTask;

  let mut fx = Fixture::new();
  let a = fx
    .kernel
    .as_mut()
    .unwrap()
    .register_task(path_def("a"), |_| {
      let (tx, rx) = unbounded_channel();
      drop(rx);
      Box::new(ChannelTask::new(tx))
    });
  let handle = fx.run();

  // The driving future is gone; starting must not wedge in Starting.
  fx.pc.send(KernelCommand::Start(TaskSelector::Id(a), None));
  fx.flush().await;
  assert_eq!(
    state_of(&fx.pc, a).await,
    Some(TaskState::Exited(ExitInfo::error()))
  );

  fx.quit(handle).await;
}

#[tokio::test(start_paused = true)]
async fn unresponsive_task_is_killed_then_given_up() {
  let mut fx = Fixture::new();
  let tx = fx.tx.clone();
  let a = fx
    .kernel
    .as_mut()
    .unwrap()
    .register_task(path_def("a"), move |ctx| {
      Box::new(StubbornTask { name: "a", tx, ctx })
    });
  let handle = fx.run();

  fx.pc.send(KernelCommand::Start(TaskSelector::Id(a), None));
  assert_eq!(fx.recv().await, ("a", RecordedCmd::Start));

  fx.pc.send(KernelCommand::Stop(TaskSelector::Id(a), None));
  assert_eq!(fx.recv().await, ("a", RecordedCmd::Stop));
  fx.flush().await;

  // The stop is ignored: after the grace period the kernel hard-kills.
  tokio::time::advance(STOP_TIMEOUT + Duration::from_millis(1)).await;
  assert_eq!(fx.recv().await, ("a", RecordedCmd::Kill));
  fx.flush().await;

  // The kill is also ignored: the kernel gives up so the graph (and
  // quit) can make progress. Nothing wants the task, so it stays down.
  tokio::time::advance(KILL_WAIT + Duration::from_millis(1)).await;
  fx.flush().await;
  assert_eq!(state_of(&fx.pc, a).await, Some(TaskState::Idle));

  fx.quit(handle).await;
}

#[tokio::test]
async fn stop_while_starting_stops_it() {
  let mut fx = Fixture::new();
  let tx = fx.tx.clone();
  let a = fx
    .kernel
    .as_mut()
    .unwrap()
    .register_task(path_def("a"), move |_| {
      Box::new(SilentTask { name: "a", tx })
    });
  let handle = fx.run();

  fx.pc.send(KernelCommand::Start(TaskSelector::Id(a), None));
  assert_eq!(fx.recv().await, ("a", RecordedCmd::Start));

  // Still Starting: the stop must reach the task, not wait for it to
  // finish starting.
  fx.pc.send(KernelCommand::Stop(TaskSelector::Id(a), None));
  assert_eq!(fx.recv().await, ("a", RecordedCmd::Stop));
  fx.flush().await;
  assert_eq!(state_of(&fx.pc, a).await, Some(TaskState::Idle));

  fx.quit(handle).await;
}

#[tokio::test]
async fn restart_while_starting_bounces_it() {
  let mut fx = Fixture::new();
  let tx = fx.tx.clone();
  let a = fx
    .kernel
    .as_mut()
    .unwrap()
    .register_task(path_def("a"), move |_| {
      Box::new(SilentTask { name: "a", tx })
    });
  let handle = fx.run();

  fx.pc.send(KernelCommand::Start(TaskSelector::Id(a), None));
  assert_eq!(fx.recv().await, ("a", RecordedCmd::Start));

  fx.pc
    .send(KernelCommand::Restart(TaskSelector::Id(a), None));
  assert_eq!(fx.recv().await, ("a", RecordedCmd::Stop));
  assert_eq!(fx.recv().await, ("a", RecordedCmd::Start));

  fx.quit(handle).await;
}

#[tokio::test(start_paused = true)]
async fn start_during_stop_grace_survives_give_up() {
  let mut fx = Fixture::new();
  let tx = fx.tx.clone();
  let a = fx
    .kernel
    .as_mut()
    .unwrap()
    .register_task(path_def("a"), move |ctx| {
      Box::new(StubbornTask { name: "a", tx, ctx })
    });
  let handle = fx.run();

  fx.pc.send(KernelCommand::Start(TaskSelector::Id(a), None));
  assert_eq!(fx.recv().await, ("a", RecordedCmd::Start));

  fx.pc.send(KernelCommand::Stop(TaskSelector::Id(a), None));
  assert_eq!(fx.recv().await, ("a", RecordedCmd::Stop));

  // Change of mind while the stop grace is running.
  fx.pc.send(KernelCommand::Start(TaskSelector::Id(a), None));
  fx.flush().await;
  fx.assert_no_cmd();

  // The stop is ignored: hard kill, then give-up. The start intent
  // survives both; the task comes back instead of wedging.
  tokio::time::advance(STOP_TIMEOUT + Duration::from_millis(1)).await;
  assert_eq!(fx.recv().await, ("a", RecordedCmd::Kill));
  tokio::time::advance(KILL_WAIT + Duration::from_millis(1)).await;
  assert_eq!(fx.recv().await, ("a", RecordedCmd::Start));

  // Quit must wind the stubborn task down through both graces again.
  fx.pc.send(KernelCommand::Quit);
  assert_eq!(fx.recv().await, ("a", RecordedCmd::Stop));
  tokio::time::advance(STOP_TIMEOUT + Duration::from_millis(1)).await;
  assert_eq!(fx.recv().await, ("a", RecordedCmd::Kill));
  tokio::time::advance(KILL_WAIT + Duration::from_millis(1)).await;
  tokio::time::timeout(Duration::from_secs(1), handle)
    .await
    .expect("timed out waiting for kernel to quit")
    .unwrap();
}

/// A job that reports success in the same step where the reconciler
/// would stop it must land in Done, not be treated as merely stopped.
#[tokio::test]
async fn job_success_beats_stop_decided_in_same_step() {
  let mut fx = Fixture::new();
  let d = fx.add("d", path_def("d"));
  let tx = fx.tx.clone();
  let j = fx.kernel.as_mut().unwrap().register_task(
    TaskDef {
      kind: TaskKind::Job,
      deps: vec![TaskSelector::Id(d)],
      ..path_def("j")
    },
    move |ctx| {
      ctx.subscribe_path(
        TaskKey::default_space(TaskPath::new("d").unwrap()),
        SubMode::Subtree,
      );
      Box::new(ExitOnNotify { name: "j", tx })
    },
  );
  let handle = fx.run();

  fx.pc.send(KernelCommand::Start(TaskSelector::Id(j), None));
  assert_eq!(fx.recv().await, ("d", RecordedCmd::Start));
  assert_eq!(fx.recv().await, ("j", RecordedCmd::Start));
  fx.flush().await;

  // Stopping the dep breaks j's support in the same step in which j's
  // success report is queued (j exits when it hears the dep stopping).
  // The success must win: j is Done, never commanded to stop.
  fx.pc.send(KernelCommand::Stop(TaskSelector::Id(d), None));
  assert_eq!(fx.recv().await, ("d", RecordedCmd::Stop));
  assert_eq!(
    state_of(&fx.pc, j).await,
    Some(TaskState::Done(ExitInfo::code(0)))
  );

  fx.quit(handle).await;
}

#[tokio::test]
async fn explain_reports_block_reason() {
  let mut fx = Fixture::new();
  let dep = fx.add(
    "dep",
    TaskDef {
      ready: ReadyMode::Reported { timeout: None },
      ..path_def("dep")
    },
  );
  let base = fx.add("base", path_def("base"));
  let app = fx.add(
    "app",
    TaskDef {
      deps: vec![TaskSelector::Id(dep), TaskSelector::Id(base)],
      ..path_def("app")
    },
  );
  for user in ["c-user", "a-user", "b-user"] {
    fx.add(
      user,
      TaskDef {
        deps: vec![TaskSelector::Id(app)],
        ..path_def(user)
      },
    );
  }
  let handle = fx.run();

  fx.pc
    .send(KernelCommand::Start(TaskSelector::Id(app), None));
  let mut started = vec![fx.recv().await, fx.recv().await];
  started.sort();
  assert_eq!(
    started,
    [("base", RecordedCmd::Start), ("dep", RecordedCmd::Start)]
  );

  let rx = fx.pc.explain(TaskSelector::Id(app));
  let mut explains = tokio::time::timeout(Duration::from_secs(1), rx)
    .await
    .unwrap();
  assert_eq!(explains.len(), 1);
  let explain = explains.pop().unwrap();
  assert_eq!(explain.name, "app");
  assert_eq!(explain.state, TaskState::Idle);
  assert!(explain.wanted);
  // Wanted but blocked: the dep has not reported ready yet.
  assert!(!explain.supported);
  assert!(explain.pinned);
  assert!(!explain.vetoed);
  // In list order: without an order, as the tasks were created.
  assert_eq!(explain.required_by, ["c-user", "a-user", "b-user"]);
  assert_eq!(explain.deps.len(), 2);
  assert_eq!(explain.deps[0].name, "dep");
  assert_eq!(explain.deps[0].state, TaskState::Running);
  assert!(explain.deps[0].wanted);
  assert!(!explain.deps[0].satisfied);
  assert_eq!(explain.deps[1].name, "base");
  assert!(explain.deps[1].satisfied);

  fx.quit(handle).await;
}

fn tagged_def(path: &str, tag: &str) -> TaskDef {
  TaskDef {
    path: Some(TaskPath::new(path).unwrap()),
    tags: vec![tag.to_string()],
    ..Default::default()
  }
}

/// Dispatch one command synchronously and settle, like the runtime loop.
fn turn(kernel: &mut Kernel, command: KernelCommand) {
  let _ = kernel.dispatch(KernelMessage {
    from: INIT_TASK_ID,
    command,
  });
  kernel.graph.settle();
}

fn turn_matching(
  kernel: &mut Kernel,
  make: impl FnOnce(Option<tokio::sync::oneshot::Sender<usize>>) -> KernelCommand,
) -> usize {
  let (tx, mut rx) = tokio::sync::oneshot::channel();
  turn(kernel, make(Some(tx)));
  rx.try_recv()
    .expect("ack not answered in the same dispatch")
}

fn pinned(kernel: &Kernel, id: TaskId) -> bool {
  kernel
    .graph
    .edges
    .get(&INIT_TASK_ID)
    .is_some_and(|s| s.contains(&id))
}

#[test]
fn glob_selector_pins_exactly_the_matches() {
  let mut fx = Fixture::new();
  let a = fx.add("a", path_def("a"));
  let ab = fx.add("ab", path_def("ab"));
  let b = fx.add("b", path_def("b"));
  let mut kernel = fx.kernel.take().unwrap();

  let n = turn_matching(&mut kernel, |ack| {
    KernelCommand::Start(
      TaskSelector::Glob(SpaceSelector::default_space(), "a".to_string()),
      ack,
    )
  });
  assert_eq!(n, 1);
  assert!(pinned(&kernel, a));
  assert!(!pinned(&kernel, ab));
  assert!(!pinned(&kernel, b));

  let n = turn_matching(&mut kernel, |ack| {
    KernelCommand::Start(
      TaskSelector::Glob(SpaceSelector::default_space(), "*".to_string()),
      ack,
    )
  });
  assert_eq!(n, 3);
  assert!(pinned(&kernel, ab));
  assert!(pinned(&kernel, b));
}

#[test]
fn tag_and_all_selectors() {
  let mut fx = Fixture::new();
  let a = fx.add("a", tagged_def("a", "web"));
  let b = fx.add("b", tagged_def("b", "web"));
  let c = fx.add("c", path_def("c"));
  let mut kernel = fx.kernel.take().unwrap();

  let n = turn_matching(&mut kernel, |ack| {
    KernelCommand::Start(
      TaskSelector::Tag(SpaceSelector::default_space(), "web".to_string()),
      ack,
    )
  });
  assert_eq!(n, 2);
  assert!(pinned(&kernel, a));
  assert!(pinned(&kernel, b));
  assert!(!pinned(&kernel, c));

  let n = turn_matching(&mut kernel, |ack| {
    KernelCommand::Unpin(TaskSelector::all(), ack)
  });
  assert_eq!(n, 3);
  assert!(!pinned(&kernel, a));
  assert!(!pinned(&kernel, b));

  let n = turn_matching(&mut kernel, |ack| {
    KernelCommand::Start(
      TaskSelector::Tag(SpaceSelector::default_space(), "nope".to_string()),
      ack,
    )
  });
  assert_eq!(n, 0);
  assert!(!pinned(&kernel, a));
}

#[test]
fn id_selector_matches_only_a_live_task() {
  let mut fx = Fixture::new();
  let a = fx.add("a", path_def("a"));
  let never_registered = fx.pc.alloc_id();
  let mut kernel = fx.kernel.take().unwrap();

  let n = turn_matching(&mut kernel, |ack| {
    KernelCommand::Start(TaskSelector::Id(a), ack)
  });
  assert_eq!(n, 1);
  assert!(pinned(&kernel, a));

  turn(
    &mut kernel,
    KernelCommand::Remove(TaskSelector::Id(a), None),
  );
  let n = turn_matching(&mut kernel, |ack| {
    KernelCommand::Start(TaskSelector::Id(a), ack)
  });
  assert_eq!(n, 0);
  assert!(!pinned(&kernel, a));

  // Unlike bare `Start`, the selector never pre-pins an id that has
  // not registered yet.
  let n = turn_matching(&mut kernel, |ack| {
    KernelCommand::Start(TaskSelector::Id(never_registered), ack)
  });
  assert_eq!(n, 0);
  assert!(!pinned(&kernel, never_registered));
}

#[test]
fn commands_on_a_removed_id_leave_no_edges() {
  let mut fx = Fixture::new();
  let a = fx.add("a", path_def("a"));
  let b = fx.add("b", path_def("b"));
  let mut kernel = fx.kernel.take().unwrap();

  turn(
    &mut kernel,
    KernelCommand::Remove(TaskSelector::Id(a), None),
  );

  turn(&mut kernel, KernelCommand::Start(TaskSelector::Id(a), None));
  assert!(!pinned(&kernel, a));
  turn(
    &mut kernel,
    KernelCommand::Restart(TaskSelector::Id(a), None),
  );
  assert!(!pinned(&kernel, a));

  kernel.graph.add_edge(b, a);
  assert!(
    !kernel.graph.edges.get(&b).is_some_and(|s| s.contains(&a)),
    "edge to a removed id was added"
  );
  kernel.graph.add_edge(a, b);
  assert!(kernel.graph.edges.get(&a).is_none());
}

#[tokio::test]
async fn start_matching_tag_starts_the_tagged_tasks() {
  let mut fx = Fixture::new();
  fx.add("a", tagged_def("a", "web"));
  fx.add("b", tagged_def("b", "web"));
  fx.add("c", path_def("c"));
  let handle = fx.run();

  let (tx, rx) = tokio::sync::oneshot::channel();
  fx.pc.send(KernelCommand::Start(
    TaskSelector::Tag(SpaceSelector::default_space(), "web".to_string()),
    Some(tx),
  ));
  assert_eq!(rx.await.unwrap(), 2);

  let mut started = vec![fx.recv().await, fx.recv().await];
  started.sort();
  assert_eq!(
    started,
    vec![("a", RecordedCmd::Start), ("b", RecordedCmd::Start)]
  );
  fx.flush().await;
  fx.assert_no_cmd();

  fx.quit(handle).await;
}

#[tokio::test]
async fn spaces_keep_paths_separate() {
  let mut kernel = Kernel::new();
  let pc = kernel.context();
  let default_id = kernel.register_task(path_def("same"), |_| {
    Box::new(crate::kernel::task::TargetTask)
  });
  let mut dekit_def = path_def("same");
  dekit_def.space = TaskSpaceId::dekit();
  let dekit_id = kernel
    .register_task(dekit_def, |_| Box::new(crate::kernel::task::TargetTask));
  let handle = tokio::spawn(kernel.run());

  let default = pc.list_tasks(TaskSelector::all()).await;
  assert_eq!(default.len(), 1);
  assert_eq!(default[0].id, default_id);

  let dekit = pc
    .list_tasks(TaskSelector::Glob(
      SpaceSelector::One(TaskSpaceId::dekit()),
      "**".to_string(),
    ))
    .await;
  assert_eq!(dekit.len(), 1);
  assert_eq!(dekit[0].id, dekit_id);

  pc.send(KernelCommand::Quit);
  handle.await.unwrap();
}

#[test]
fn selectors_are_space_local() {
  let mut kernel = Kernel::new();
  let mut default_def = path_def("same");
  default_def.tags.push("tagged".to_string());
  let default_id = kernel
    .register_task(default_def, |_| Box::new(crate::kernel::task::TargetTask));
  let mut dekit_def = path_def("same");
  dekit_def.tags.push("tagged".to_string());
  dekit_def.space = TaskSpaceId::dekit();
  let dekit_id = kernel
    .register_task(dekit_def, |_| Box::new(crate::kernel::task::TargetTask));

  assert_eq!(
    kernel.graph.matching_ids(&TaskSelector::Tag(
      SpaceSelector::default_space(),
      "tagged".to_string(),
    )),
    vec![default_id]
  );
  assert_eq!(
    kernel.graph.matching_ids(&TaskSelector::Glob(
      SpaceSelector::One(TaskSpaceId::dekit()),
      "same".to_string(),
    )),
    vec![dekit_id]
  );
}

#[tokio::test]
async fn default_context_cannot_register_reserved_task() {
  let kernel = Kernel::new();
  let pc = kernel.context();
  let handle = tokio::spawn(kernel.run());
  let task_id = pc.alloc_id();
  let ack = pc.spawn_async_with_id(
    task_id,
    TaskDef {
      space: TaskSpaceId::dekit(),
      path: Some(TaskPath::new("console").unwrap()),
      ..Default::default()
    },
    |_, _| async {},
  );

  assert!(ack.await.unwrap().is_err());
  assert_eq!(resolve_in(&pc, TaskSpaceId::dekit(), "console").await, None);

  pc.send(KernelCommand::Quit);
  handle.await.unwrap();
}

#[tokio::test]
async fn reserved_task_controls_its_space() {
  let mut kernel = Kernel::new();
  let pc = kernel.context();
  let mut provider_def = path_def("console");
  provider_def.space = TaskSpaceId::dekit();
  let provider = kernel
    .register_task(provider_def, |_| Box::new(crate::kernel::task::TargetTask));
  let provider_pc = TaskContext::new(
    kernel.graph.next_task_id.clone(),
    provider,
    kernel.sender.clone(),
  );
  let handle = tokio::spawn(kernel.run());

  pc.send(KernelCommand::Remove(TaskSelector::Id(provider), None));
  let (tx, rx) = tokio::sync::oneshot::channel();
  pc.send(KernelCommand::Start(TaskSelector::Id(provider), Some(tx)));
  assert_eq!(rx.await.unwrap(), 0);
  assert_eq!(
    resolve_in(&pc, TaskSpaceId::dekit(), "console").await,
    Some(provider)
  );

  let child = provider_pc.alloc_id();
  let ack = provider_pc.spawn_async_with_id(
    child,
    TaskDef {
      space: TaskSpaceId::dekit(),
      path: Some(TaskPath::new("consoles/main").unwrap()),
      ..Default::default()
    },
    |_, mut rx| async move { while rx.recv().await.is_some() {} },
  );
  assert!(ack.await.unwrap().is_ok());
  provider_pc.send(KernelCommand::Remove(TaskSelector::Id(child), None));
  provider_pc.send(KernelCommand::Remove(TaskSelector::Id(provider), None));
  assert_eq!(resolve_in(&pc, TaskSpaceId::dekit(), "console").await, None);

  pc.send(KernelCommand::Quit);
  handle.await.unwrap();
}

#[tokio::test]
async fn active_watch_reports_transitions_only() {
  let mut fx = Fixture::new();
  let a = fx.add("a", path_def("a"));
  let b = fx.add("b", path_def("b"));
  let handle = fx.run();

  let mut watch = fx.pc.watch_active(TaskSelector::all());
  fx.flush().await;
  assert!(watch.try_recv().is_err(), "no report without a transition");

  fx.pc.send(KernelCommand::Start(TaskSelector::Id(a), None));
  fx.pc.send(KernelCommand::Start(TaskSelector::Id(b), None));
  assert_eq!(fx.recv().await, ("a", RecordedCmd::Start));
  assert_eq!(fx.recv().await, ("b", RecordedCmd::Start));
  assert_eq!(watch.recv().await, Some(true));

  fx.pc.send(KernelCommand::Stop(TaskSelector::Id(a), None));
  assert_eq!(fx.recv().await, ("a", RecordedCmd::Stop));
  fx.flush().await;
  assert!(watch.try_recv().is_err(), "one task is still active");

  fx.pc.send(KernelCommand::Stop(TaskSelector::Id(b), None));
  assert_eq!(fx.recv().await, ("b", RecordedCmd::Stop));
  assert_eq!(watch.recv().await, Some(false));

  fx.quit(handle).await;
}

/// A watch sees a task move out of what it selects.
#[tokio::test]
async fn active_watch_sees_a_move() {
  let mut fx = Fixture::new();
  let a = fx.add("a", path_def("web/a"));
  let handle = fx.run();

  let glob =
    TaskSelector::Glob(SpaceSelector::default_space(), "web/**".into());
  let mut watch = fx.pc.watch_active(glob);
  fx.pc.send(KernelCommand::Start(TaskSelector::Id(a), None));
  assert_eq!(fx.recv().await, ("a", RecordedCmd::Start));
  assert_eq!(watch.recv().await, Some(true));

  let (reply, rx) = tokio::sync::oneshot::channel();
  fx.pc.send(KernelCommand::Move(
    MoveTask {
      space: TaskSpaceId::default_space(),
      from: TaskPath::new("web/a").unwrap(),
      to: TaskPath::new("a").unwrap(),
      place: Place::Last,
    },
    reply,
  ));
  assert_eq!(rx.await.unwrap(), Ok(1));
  assert_eq!(watch.recv().await, Some(false));

  fx.quit(handle).await;
}

#[tokio::test]
async fn subscribe_replays_existing_tasks() {
  let kernel = Kernel::new();
  let pc = kernel.context();
  let a = pc.register(
    path_def("a"),
    Box::new(|_| Box::new(super::super::task::TargetTask)),
  );
  let other = pc.register(
    path_def("b/c"),
    Box::new(|_| Box::new(super::super::task::TargetTask)),
  );
  let sibling = pc.register(
    path_def("b/d"),
    Box::new(|_| Box::new(super::super::task::TargetTask)),
  );
  let handle = tokio::spawn(kernel.run());

  let (tx, mut rx) = unbounded_channel();
  let (subscribed_tx, subscribed_rx) = tokio::sync::oneshot::channel();
  let listener = pc.alloc_id();
  let ack = pc.spawn_async_with_id(
    listener,
    TaskDef::default(),
    move |pc, mut cmds| async move {
      pc.subscribe_path(
        TaskKey::default_space(TaskPath::new("b").unwrap()),
        SubMode::Subtree,
      );
      pc.subscribe_path(
        TaskKey::default_space(TaskPath::new("b/c").unwrap()),
        SubMode::Exact,
      );
      pc.subscribe_path(
        TaskKey::default_space(TaskPath::new("b").unwrap()),
        SubMode::Subtree,
      );
      subscribed_tx.send(()).unwrap();
      while let Some(cmd) = cmds.recv().await {
        if let TaskCmd::Msg(msg) = cmd
          && let Ok(n) = msg.downcast::<TaskNotification>()
          && let TaskNotify::Added { path, .. } = n.notify
        {
          tx.send((n.from, path)).unwrap();
        }
      }
    },
  );
  assert!(ack.await.unwrap().is_ok());
  subscribed_rx.await.unwrap();
  pc.list_tasks(TaskSelector::all()).await;

  let mut replayed = HashSet::new();
  for _ in 0..2 {
    replayed.insert(
      tokio::time::timeout(Duration::from_secs(1), rx.recv())
        .await
        .unwrap()
        .unwrap(),
    );
  }
  assert_eq!(
    replayed,
    HashSet::from([
      (other, Some(TaskPath::new("b/c").unwrap())),
      (sibling, Some(TaskPath::new("b/d").unwrap())),
    ])
  );
  assert!(!replayed.iter().any(|(from, _)| *from == a));
  assert!(
    rx.try_recv().is_err(),
    "overlapping and duplicate subscriptions must not replay tasks again"
  );

  pc.send(KernelCommand::Quit);
  handle.await.unwrap();
}

// ---- Upgrade freeze / thaw / restore ----

async fn try_freeze(fx: &Fixture) -> Result<KernelSnapshot, String> {
  let (tx, rx) = tokio::sync::oneshot::channel();
  fx.pc.send(KernelCommand::Freeze(tx));
  tokio::time::timeout(Duration::from_secs(1), rx)
    .await
    .expect("timed out waiting for the freeze")
    .expect("freeze reply dropped")
}

async fn freeze(fx: &Fixture) -> KernelSnapshot {
  try_freeze(fx).await.expect("freeze refused")
}

#[tokio::test]
async fn freeze_defers_intent_until_thaw() {
  let mut fx = Fixture::new();
  let a = fx.add("a", path_def("a"));
  let handle = fx.run();

  fx.pc.send(KernelCommand::Start(TaskSelector::Id(a), None));
  assert_eq!(fx.recv().await, ("a", RecordedCmd::Start));

  let snapshot = freeze(&fx).await;
  assert_eq!(snapshot.tasks.len(), 1);
  assert_eq!(snapshot.tasks[0].state, snap::TaskState::Ready {});
  assert!(snapshot.tasks[0].pinned);

  // Intent waits; reads still work.
  fx.pc.send(KernelCommand::Unpin(TaskSelector::Id(a), None));
  fx.flush().await;
  fx.assert_no_cmd();

  fx.pc.send(KernelCommand::Thaw);
  assert_eq!(fx.recv().await, ("a", RecordedCmd::Stop));

  fx.quit(handle).await;
}

#[tokio::test]
async fn second_freeze_is_refused_and_reports_still_apply() {
  let mut fx = Fixture::new();
  let a = fx.add("a", path_def("a"));
  let handle = fx.run();
  fx.pc.send(KernelCommand::Start(TaskSelector::Id(a), None));
  assert_eq!(fx.recv().await, ("a", RecordedCmd::Start));

  let _first = freeze(&fx).await;
  assert!(
    try_freeze(&fx).await.is_err(),
    "a second freeze must be refused"
  );

  // A task exit reported while frozen lands in the graph without any
  // driving (no restart command goes out).
  let task_ctx = TaskContext::new(
    std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(100)),
    a,
    fx.pc.sender_for_tests(),
  );
  task_ctx.send(KernelCommand::TaskStopped(ExitInfo::code(1)));
  fx.flush().await;
  fx.assert_no_cmd();
  let tasks = fx.pc.list_tasks(TaskSelector::Id(a)).await;
  assert_eq!(tasks[0].state, TaskState::Exited(ExitInfo::code(1)));

  fx.pc.send(KernelCommand::Thaw);
  fx.quit(handle).await;
}

#[tokio::test]
async fn freeze_is_refused_while_shutting_down() {
  let mut fx = Fixture::new();
  let tx = fx.tx.clone();
  let a = fx
    .kernel
    .as_mut()
    .unwrap()
    .register_task(path_def("a"), move |ctx| {
      Box::new(StubbornTask { name: "a", tx, ctx })
    });
  let handle = fx.run();
  fx.pc.send(KernelCommand::Start(TaskSelector::Id(a), None));
  assert_eq!(fx.recv().await, ("a", RecordedCmd::Start));
  // The stubborn task keeps the quit in progress.
  fx.pc.send(KernelCommand::Quit);
  assert_eq!(fx.recv().await, ("a", RecordedCmd::Stop));

  let err = try_freeze(&fx).await.unwrap_err();
  assert!(err.contains("shutting down"), "{err}");

  // Nothing froze: dropping the task lets the quit finish.
  fx.pc.send(KernelCommand::Remove(TaskSelector::Id(a), None));
  tokio::time::timeout(Duration::from_secs(2), handle)
    .await
    .expect("timed out waiting for kernel to quit")
    .unwrap();
}

/// Counts the saves a quit makes.
fn count_saves(fx: &mut Fixture) -> Arc<AtomicUsize> {
  let saves = Arc::new(AtomicUsize::new(0));
  let counter = saves.clone();
  fx.kernel.as_mut().unwrap().save_on_quit(Box::new(move |_| {
    counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    Ok(())
  }));
  saves
}

#[tokio::test]
async fn quit_saves_and_quit_without_save_does_not() {
  for save in [true, false] {
    let mut fx = Fixture::new();
    let a = fx.add("a", path_def("a"));
    let saves = count_saves(&mut fx);
    let handle = fx.run();
    fx.pc.send(KernelCommand::Start(TaskSelector::Id(a), None));
    assert_eq!(fx.recv().await, ("a", RecordedCmd::Start));

    fx.pc.send(if save {
      KernelCommand::Quit
    } else {
      KernelCommand::QuitWithoutSave
    });
    assert_eq!(fx.recv().await, ("a", RecordedCmd::Stop));
    tokio::time::timeout(Duration::from_secs(2), handle)
      .await
      .expect("timed out waiting for kernel to quit")
      .unwrap();
    assert_eq!(
      saves.load(std::sync::atomic::Ordering::SeqCst),
      usize::from(save)
    );
  }
}

#[tokio::test]
async fn quit_without_save_drops_a_save_under_way() {
  let mut fx = Fixture::new();
  let tx = fx.tx.clone();
  // Never answers the freeze, so the quit's save waits on it.
  let a = fx
    .kernel
    .as_mut()
    .unwrap()
    .register_task(path_def("a"), move |_| {
      Box::new(SilentTask { name: "a", tx })
    });
  let saves = count_saves(&mut fx);
  let handle = fx.run();
  fx.pc.send(KernelCommand::Start(TaskSelector::Id(a), None));
  assert_eq!(fx.recv().await, ("a", RecordedCmd::Start));

  fx.pc.send(KernelCommand::Quit);
  fx.flush().await;
  fx.assert_no_cmd();

  fx.pc.send(KernelCommand::QuitWithoutSave);
  assert_eq!(fx.recv().await, ("a", RecordedCmd::Stop));
  tokio::time::timeout(Duration::from_secs(2), handle)
    .await
    .expect("timed out waiting for kernel to quit")
    .unwrap();
  assert_eq!(saves.load(std::sync::atomic::Ordering::SeqCst), 0);
}

#[tokio::test]
async fn freeze_names_a_task_whose_handler_stopped() {
  let mut fx = Fixture::new();
  fx.add("a", path_def("a"));
  fx.kernel
    .as_mut()
    .unwrap()
    .register_task(path_def("gone"), |_| {
      let (tx, _) = unbounded_channel();
      Box::new(crate::kernel::task::ChannelTask::new(tx))
    });
  let handle = fx.run();

  let err = try_freeze(&fx).await.unwrap_err();
  assert!(err.contains("gone"), "{err}");

  fx.pc.send(KernelCommand::Thaw);
  fx.quit(handle).await;
}

#[tokio::test]
async fn a_late_answer_to_an_earlier_freeze_is_ignored() {
  struct Mute;
  impl Task for Mute {
    fn handle_cmd(&mut self, _cmd: TaskCmd, _fx: &mut Effects) {}
  }
  let mut fx = Fixture::new();
  let a = fx
    .kernel
    .as_mut()
    .unwrap()
    .register_task(path_def("a"), |_| Box::new(Mute));
  let handle = fx.run();
  let task_ctx = TaskContext::new(
    std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(100)),
    a,
    fx.pc.sender_for_tests(),
  );

  // Freeze 1 never hears back (as if it timed out) and is thawed.
  let (tx, _first) = tokio::sync::oneshot::channel();
  fx.pc.send(KernelCommand::Freeze(tx));
  fx.pc.send(KernelCommand::Thaw);

  let (tx, mut second) = tokio::sync::oneshot::channel();
  fx.pc.send(KernelCommand::Freeze(tx));
  task_ctx.send(KernelCommand::TaskFrozen(
    1,
    TaskKindSnapshot::Console {
      groups: Default::default(),
    },
  ));
  fx.flush().await;
  assert!(
    second.try_recv().is_err(),
    "a stale answer completed freeze 2"
  );

  task_ctx.send(KernelCommand::TaskFrozen(
    2,
    TaskKindSnapshot::Console {
      groups: Default::default(),
    },
  ));
  let snapshot = tokio::time::timeout(Duration::from_secs(1), second)
    .await
    .expect("timed out waiting for the freeze")
    .unwrap()
    .unwrap();
  assert_eq!(snapshot.tasks.len(), 1);

  fx.pc.send(KernelCommand::Thaw);
  fx.quit(handle).await;
}

#[tokio::test]
async fn thawed_intent_is_handled_one_message_at_a_time() {
  let mut fx = Fixture::new();
  let a = fx.add("a", path_def("a"));
  let handle = fx.run();
  let _ = freeze(&fx).await;

  // Settled one message at a time, as without the freeze, the start
  // happens before the unpin undoes it.
  fx.pc.send(KernelCommand::Start(TaskSelector::Id(a), None));
  fx.pc.send(KernelCommand::Unpin(TaskSelector::Id(a), None));
  fx.flush().await;
  fx.assert_no_cmd();

  fx.pc.send(KernelCommand::Thaw);
  assert_eq!(fx.recv().await, ("a", RecordedCmd::Start));
  assert_eq!(fx.recv().await, ("a", RecordedCmd::Stop));

  fx.quit(handle).await;
}

#[tokio::test]
async fn stopping_task_keeps_its_deadline_in_the_snapshot() {
  let mut fx = Fixture::new();
  let tx = fx.tx.clone();
  let a = fx
    .kernel
    .as_mut()
    .unwrap()
    .register_task(path_def("a"), move |ctx| {
      Box::new(StubbornTask { name: "a", tx, ctx })
    });
  let handle = fx.run();
  fx.pc.send(KernelCommand::Start(TaskSelector::Id(a), None));
  assert_eq!(fx.recv().await, ("a", RecordedCmd::Start));
  fx.pc.send(KernelCommand::Unpin(TaskSelector::Id(a), None));
  assert_eq!(fx.recv().await, ("a", RecordedCmd::Stop));

  let snapshot = freeze(&fx).await;
  let task = &snapshot.tasks[0];
  assert_eq!(task.state, snap::TaskState::Stopping {});
  let remaining = task.timer_ms.expect("stop grace remaining");
  assert!(remaining > 0 && remaining <= STOP_TIMEOUT.as_millis() as u64);

  fx.pc.send(KernelCommand::Thaw);
  // The stubborn task would hold quit for the whole grace; drop it.
  fx.pc.send(KernelCommand::Remove(TaskSelector::Id(a), None));
  fx.quit(handle).await;
}

#[tokio::test]
async fn restore_rebuilds_graph_and_drives_only_what_changed() {
  let mut fx = Fixture::new();
  let a = fx.add("a", path_def("a"));
  let b = fx.add(
    "b",
    TaskDef {
      deps: vec![TaskSelector::Id(a)],
      ..path_def("b")
    },
  );
  let handle = fx.run();
  fx.pc.send(KernelCommand::Start(TaskSelector::Id(b), None));
  assert_eq!(fx.recv().await, ("a", RecordedCmd::Start));
  assert_eq!(fx.recv().await, ("b", RecordedCmd::Start));
  let snapshot = freeze(&fx).await;
  fx.pc.send(KernelCommand::Thaw);
  fx.quit(handle).await;

  // A new kernel from the snapshot: both tasks are Ready already, so
  // nothing is started; b is pinned and requires a.
  let mut restored = Fixture::new();
  let tasks = snapshot
    .tasks
    .iter()
    .map(|saved| {
      let name: &'static str = if saved.id == a.0 { "a" } else { "b" };
      let tx = restored.tx.clone();
      let def = TaskDef {
        path: saved.path.as_deref().map(|p| TaskPath::new(p).unwrap()),
        deps: saved
          .deps
          .iter()
          .map(|id| TaskSelector::Id(TaskId(*id)))
          .collect(),
        pinned: saved.pinned,
        ..Default::default()
      };
      (
        Some(saved),
        Registration::Task(TaskRegistration {
          task_id: TaskId(saved.id),
          def,
          factory: Box::new(move |ctx| {
            Box::new(RecordingTask { name, tx, ctx })
          }),
        }),
      )
    })
    .collect();
  restored
    .kernel
    .as_mut()
    .unwrap()
    .restore(snapshot.next_task_id, tasks)
    .unwrap();
  let handle = restored.run();
  restored.flush().await;
  restored.assert_no_cmd();

  let explain = restored.pc.explain(TaskSelector::Id(b)).await;
  assert_eq!(explain[0].state, TaskState::Ready);
  assert!(explain[0].pinned);
  assert_eq!(explain[0].deps.len(), 1);

  // Dropping the pin now stops b first, then a: the edges survived.
  restored
    .pc
    .send(KernelCommand::Unpin(TaskSelector::Id(b), None));
  assert_eq!(restored.recv().await, ("b", RecordedCmd::Stop));
  assert_eq!(restored.recv().await, ("a", RecordedCmd::Stop));
  restored.quit(handle).await;
}

fn ready_timeout_def(path: &str, restart: RestartMode) -> TaskDef {
  TaskDef {
    ready: ReadyMode::Reported {
      timeout: Some(Duration::from_secs(5)),
    },
    restart,
    ..path_def(path)
  }
}

#[tokio::test(start_paused = true)]
async fn not_ready_in_time_stops_and_stays_exited() {
  let mut fx = Fixture::new();
  let a = fx.add("a", ready_timeout_def("a", RestartMode::Never));
  let handle = fx.run();

  fx.pc.send(KernelCommand::Start(TaskSelector::Id(a), None));
  assert_eq!(fx.recv().await, ("a", RecordedCmd::Start));
  fx.flush().await;
  assert_eq!(state_of(&fx.pc, a).await, Some(TaskState::Running));

  tokio::time::advance(Duration::from_secs(5)).await;
  assert_eq!(fx.recv().await, ("a", RecordedCmd::Stop));
  fx.flush().await;
  // Still pinned, but a failed start with `never` is not retried.
  match state_of(&fx.pc, a).await {
    Some(TaskState::Exited(info)) => assert!(info.ready_timeout),
    other => panic!("expected a failed exit, got {other:?}"),
  }
  fx.assert_no_cmd();

  // A start runs it again, with a fresh timeout.
  fx.pc.send(KernelCommand::Start(TaskSelector::Id(a), None));
  assert_eq!(fx.recv().await, ("a", RecordedCmd::Start));
  fx.flush().await;
  assert_eq!(state_of(&fx.pc, a).await, Some(TaskState::Running));

  fx.quit(handle).await;
}

#[tokio::test(start_paused = true)]
async fn not_ready_in_time_backs_off_and_retries_on_failure() {
  let mut fx = Fixture::new();
  let a = fx.add("a", ready_timeout_def("a", RestartMode::OnFailure));
  let handle = fx.run();

  fx.pc.send(KernelCommand::Start(TaskSelector::Id(a), None));
  assert_eq!(fx.recv().await, ("a", RecordedCmd::Start));
  fx.flush().await;

  tokio::time::advance(Duration::from_secs(5)).await;
  assert_eq!(fx.recv().await, ("a", RecordedCmd::Stop));
  fx.flush().await;
  // The backoff says why: not ready in time.
  assert_eq!(
    state_of(&fx.pc, a).await,
    Some(TaskState::Backoff(ExitInfo {
      ready_timeout: true,
      ..ExitInfo::code(0)
    }))
  );
  assert_eq!(fx.recv().await, ("a", RecordedCmd::Start));

  // The uptime of a start that never got ready does not reset the count.
  tokio::time::advance(Duration::from_secs(5)).await;
  assert_eq!(fx.recv().await, ("a", RecordedCmd::Stop));
  fx.flush().await;
  let graph_attempts = fx.pc.explain(TaskSelector::Id(a)).await[0].attempts;
  assert_eq!(graph_attempts, 2);

  fx.quit(handle).await;
}

#[tokio::test(start_paused = true)]
async fn ready_in_time_is_not_failed() {
  let mut fx = Fixture::new();
  let a = fx.add("a", ready_timeout_def("a", RestartMode::Never));
  let handle = fx.run();

  fx.pc.send(KernelCommand::Start(TaskSelector::Id(a), None));
  assert_eq!(fx.recv().await, ("a", RecordedCmd::Start));
  tokio::time::advance(Duration::from_secs(4)).await;
  fx.pc.send_msg(a, Report::Ready);
  fx.flush().await;
  tokio::time::advance(Duration::from_secs(10)).await;
  fx.flush().await;
  fx.assert_no_cmd();
  assert_eq!(state_of(&fx.pc, a).await, Some(TaskState::Ready));

  fx.quit(handle).await;
}

#[tokio::test(start_paused = true)]
async fn start_during_a_failed_stop_brings_it_back() {
  let mut fx = Fixture::new();
  let tx = fx.tx.clone();
  let a = fx
    .kernel
    .as_mut()
    .unwrap()
    .register_task(ready_timeout_def("a", RestartMode::Never), move |ctx| {
      Box::new(StubbornTask { name: "a", tx, ctx })
    });
  let handle = fx.run();

  fx.pc.send(KernelCommand::Start(TaskSelector::Id(a), None));
  assert_eq!(fx.recv().await, ("a", RecordedCmd::Start));
  tokio::time::advance(Duration::from_secs(5)).await;
  assert_eq!(fx.recv().await, ("a", RecordedCmd::Stop));

  // Had the stop finished first, this start would revive the task; it
  // must do the same while the stop is still under way.
  fx.pc.send(KernelCommand::Start(TaskSelector::Id(a), None));
  fx.flush().await;
  fx.assert_no_cmd();
  tokio::time::advance(STOP_TIMEOUT + Duration::from_millis(1)).await;
  assert_eq!(fx.recv().await, ("a", RecordedCmd::Kill));
  tokio::time::advance(KILL_WAIT + Duration::from_millis(1)).await;
  assert_eq!(fx.recv().await, ("a", RecordedCmd::Start));

  fx.pc.send(KernelCommand::Remove(TaskSelector::Id(a), None));
  fx.quit(handle).await;
}

#[tokio::test(start_paused = true)]
async fn failed_stop_that_gives_up_still_lands_as_failed() {
  let mut fx = Fixture::new();
  let tx = fx.tx.clone();
  let a = fx
    .kernel
    .as_mut()
    .unwrap()
    .register_task(ready_timeout_def("a", RestartMode::Never), move |ctx| {
      Box::new(StubbornTask { name: "a", tx, ctx })
    });
  let handle = fx.run();

  fx.pc.send(KernelCommand::Start(TaskSelector::Id(a), None));
  assert_eq!(fx.recv().await, ("a", RecordedCmd::Start));
  tokio::time::advance(Duration::from_secs(5)).await;
  assert_eq!(fx.recv().await, ("a", RecordedCmd::Stop));
  tokio::time::advance(STOP_TIMEOUT + Duration::from_millis(1)).await;
  assert_eq!(fx.recv().await, ("a", RecordedCmd::Kill));
  tokio::time::advance(KILL_WAIT + Duration::from_millis(1)).await;
  fx.flush().await;
  match state_of(&fx.pc, a).await {
    Some(TaskState::Exited(info)) => assert!(info.ready_timeout),
    other => panic!("expected a failed exit, got {other:?}"),
  }
  fx.assert_no_cmd();

  fx.quit(handle).await;
}

#[tokio::test(start_paused = true)]
async fn stop_timeout_is_per_task() {
  let mut fx = Fixture::new();
  let tx = fx.tx.clone();
  let a = fx.kernel.as_mut().unwrap().register_task(
    TaskDef {
      stop_timeout: Duration::from_secs(2),
      ..path_def("a")
    },
    move |ctx| Box::new(StubbornTask { name: "a", tx, ctx }),
  );
  let handle = fx.run();

  fx.pc.send(KernelCommand::Start(TaskSelector::Id(a), None));
  assert_eq!(fx.recv().await, ("a", RecordedCmd::Start));
  fx.pc.send(KernelCommand::Stop(TaskSelector::Id(a), None));
  assert_eq!(fx.recv().await, ("a", RecordedCmd::Stop));
  fx.flush().await;
  tokio::time::advance(Duration::from_millis(1990)).await;
  fx.flush().await;
  fx.assert_no_cmd();
  tokio::time::advance(Duration::from_millis(20)).await;
  assert_eq!(fx.recv().await, ("a", RecordedCmd::Kill));

  fx.pc.send(KernelCommand::Remove(TaskSelector::Id(a), None));
  fx.quit(handle).await;
}

#[tokio::test(start_paused = true)]
async fn snapshot_carries_the_ready_timeout_and_a_failed_start() {
  let mut fx = Fixture::new();
  let tx = fx.tx.clone();
  let a = fx.kernel.as_mut().unwrap().register_task(
    TaskDef {
      stop_timeout: Duration::from_secs(3),
      ..ready_timeout_def("a", RestartMode::Never)
    },
    move |ctx| Box::new(StubbornTask { name: "a", tx, ctx }),
  );
  let job = fx.add(
    "job",
    TaskDef {
      kind: TaskKind::Job,
      ..path_def("job")
    },
  );
  let handle = fx.run();

  fx.pc.send(KernelCommand::Start(TaskSelector::Id(a), None));
  assert_eq!(fx.recv().await, ("a", RecordedCmd::Start));
  fx.flush().await;
  let snapshot = freeze(&fx).await;
  let task = &snapshot.tasks[0];
  assert_eq!(task.state, snap::TaskState::Running {});
  assert_eq!(task.ready_timeout_ms, Some(5000));
  assert_eq!(task.stop_timeout_ms, Some(3000));
  assert!(task.timer_ms.is_some_and(|ms| ms > 0 && ms <= 5000));
  assert!(!task.start_failed);
  assert_eq!(snapshot.tasks[1].id, job.0);
  assert!(snapshot.tasks[1].job);
  fx.pc.send(KernelCommand::Thaw);

  tokio::time::advance(Duration::from_secs(5)).await;
  assert_eq!(fx.recv().await, ("a", RecordedCmd::Stop));
  fx.flush().await;
  let snapshot = freeze(&fx).await;
  assert_eq!(snapshot.tasks[0].state, snap::TaskState::Stopping {});
  assert!(snapshot.tasks[0].start_failed);
  fx.pc.send(KernelCommand::Thaw);

  fx.pc.send(KernelCommand::Remove(TaskSelector::Id(a), None));
  fx.quit(handle).await;
}

// ---- Restore against a changed config ----

fn saved_task(state: snap::TaskState) -> snap::Task {
  snap::Task {
    id: 1,
    space: String::new(),
    path: Some("a".to_string()),
    label: None,
    tags: Vec::new(),
    pinned: false,
    deps: Vec::new(),
    restart: snap::Restart::Never,
    job: false,
    ready_timeout_ms: None,
    stop_timeout_ms: None,
    state,
    vetoed: false,
    killed: false,
    start_failed: false,
    saved_pin: false,
    attempts: 0,
    last_start_secs_ago: Some(40),
    timer_ms: Some(4000),
    spec: None,
    run_spec: None,
    origin: None,
    kind: TaskKindSnapshot::Console {
      groups: Default::default(),
    },
  }
}

/// A restored state is one the task, as now configured, can be in: an end
/// state is kept only where the config files that exit the same way, else
/// the task comes back idle (ORCHESTRATION.md "Restore").
#[test]
fn restore_keeps_only_states_the_config_can_be_in() {
  let ok = ExitInfo::code(0);
  let fail = ExitInfo::code(1);
  let late = ExitInfo {
    ready_timeout: true,
    ..fail
  };
  let service = |restart| TaskDef {
    restart,
    ..path_def("a")
  };
  let job = |restart| TaskDef {
    kind: TaskKind::Job,
    restart,
    ..path_def("a")
  };
  let timed = |restart| TaskDef {
    ready: ReadyMode::Reported {
      timeout: Some(Duration::from_secs(30)),
    },
    restart,
    ..path_def("a")
  };
  let saved = |state: TaskState, start_failed| snap::Task {
    start_failed,
    ..saved_task(match state {
      TaskState::Idle => snap::TaskState::Idle {},
      TaskState::Starting => snap::TaskState::Starting {},
      TaskState::Running => snap::TaskState::Running {},
      TaskState::Ready => snap::TaskState::Ready {},
      TaskState::Stopping => snap::TaskState::Stopping {},
      TaskState::Backoff(info) => snap::TaskState::Backoff(info.into()),
      TaskState::Done(info) => snap::TaskState::Done(info.into()),
      TaskState::Exited(info) => snap::TaskState::Exited(info.into()),
    })
  };
  let left = Some(Duration::from_secs(4));
  use RestartMode::{Always, Never, OnFailure};
  use TaskState::{Backoff, Done, Exited, Idle, Ready, Stopping};
  for (saved_state, start_failed, def, state, failed_after, timer) in [
    (Done(ok), false, job(Never), Done(ok), false, None),
    // The reported case: a done job that is now a service.
    (Done(ok), false, service(Never), Idle, false, None),
    (Exited(ok), false, service(Never), Exited(ok), false, None),
    (
      Exited(ok),
      false,
      service(OnFailure),
      Exited(ok),
      false,
      None,
    ),
    // A job's success would be done: it has not run as a job.
    (Exited(ok), false, job(Never), Idle, false, None),
    // Exits the restart mode now retries.
    (Exited(fail), false, service(OnFailure), Idle, false, None),
    (Exited(ok), false, service(Always), Idle, false, None),
    (
      Backoff(fail),
      false,
      service(OnFailure),
      Backoff(fail),
      false,
      left,
    ),
    // Retries the restart mode no longer makes.
    (Backoff(fail), false, service(Never), Idle, false, None),
    (Backoff(ok), false, service(OnFailure), Idle, false, None),
    (Backoff(ok), false, job(Always), Idle, false, None),
    // Not ready in time: only with a ready timeout.
    (Exited(late), false, timed(Never), Exited(late), false, None),
    (Exited(late), false, service(Never), Idle, false, None),
    (
      Backoff(late),
      false,
      timed(OnFailure),
      Backoff(late),
      false,
      left,
    ),
    (Backoff(late), false, service(OnFailure), Idle, false, None),
    (Stopping, true, timed(Never), Stopping, true, left),
    (Stopping, true, service(Never), Stopping, false, left),
    // A live job process is ready until it exits.
    (Ready, false, job(Never), Ready, false, None),
    // Only states with a timer arm one.
    (Idle, false, service(Never), Idle, false, None),
  ] {
    let case = format!("{saved_state:?} as {:?}/{:?}", def.kind, def.restart);
    let mut kernel = Kernel::new();
    kernel
      .graph
      .register_task_with_id(
        TaskId(1),
        def,
        Box::new(|_| Box::new(crate::kernel::task::TargetTask)),
        Some(&saved(saved_state, start_failed)),
      )
      .unwrap();
    let task = &kernel.graph.tasks[&TaskId(1)];
    assert_eq!(task.state, state, "{case}");
    assert_eq!(task.start_failed, failed_after, "{case}");
    let timer_left = kernel.graph.timer(TaskId(1)).map(|(_, left)| left);
    assert_eq!(timer_left, timer, "{case}");
  }
}

/// A job that was done and is now a service comes back idle, so the
/// reconciler starts it for its dependent instead of leaving it blocked.
#[tokio::test]
async fn restored_done_job_that_is_now_a_service_is_started() {
  let migrate = snap::Task {
    job: true,
    path: Some("migrate".to_string()),
    ..saved_task(snap::TaskState::Done(ExitInfo::code(0).into()))
  };
  let api = snap::Task {
    id: 2,
    path: Some("api".to_string()),
    pinned: true,
    deps: vec![1],
    ..saved_task(snap::TaskState::Ready {})
  };
  let mut fx = Fixture::new();
  let tasks = [&migrate, &api]
    .into_iter()
    .map(|saved| {
      let name: &'static str = if saved.id == 1 { "migrate" } else { "api" };
      let tx = fx.tx.clone();
      // `type: job` removed from migrate.
      let def = TaskDef {
        path: saved.path.as_deref().map(|p| TaskPath::new(p).unwrap()),
        deps: saved
          .deps
          .iter()
          .map(|id| TaskSelector::Id(TaskId(*id)))
          .collect(),
        pinned: saved.pinned,
        ..Default::default()
      };
      (
        Some(saved),
        Registration::Task(TaskRegistration {
          task_id: TaskId(saved.id),
          def,
          factory: Box::new(move |ctx| {
            Box::new(RecordingTask { name, tx, ctx })
          }),
        }),
      )
    })
    .collect();
  fx.kernel.as_mut().unwrap().restore(3, tasks).unwrap();
  let handle = fx.run();

  // api stops, as it waits on a service now; migrate starts for it; api
  // starts again once migrate is ready.
  let mut first = vec![fx.recv().await, fx.recv().await];
  first.sort();
  assert_eq!(
    first,
    [("api", RecordedCmd::Stop), ("migrate", RecordedCmd::Start)]
  );
  assert_eq!(fx.recv().await, ("api", RecordedCmd::Start));
  fx.flush().await;
  assert_eq!(state_of(&fx.pc, TaskId(1)).await, Some(TaskState::Ready));
  assert_eq!(state_of(&fx.pc, TaskId(2)).await, Some(TaskState::Ready));

  fx.quit(handle).await;
}

// ---- Quit bound ----

/// The bound is read in the dispatch that begins the quit: a task a
/// timeout starts just before it counts, and one it would start just
/// after never runs.
#[test]
fn quit_bound_is_read_from_the_graph_the_quit_begins_on() {
  let grace = Duration::from_secs(300);
  for timeout_first in [true, false] {
    let mut kernel = Kernel::new();
    let db = kernel.register_task(
      TaskDef {
        restart: RestartMode::OnFailure,
        stop_timeout: grace,
        pinned: true,
        ..path_def("db")
      },
      |_| Box::new(crate::kernel::task::TargetTask),
    );
    kernel.graph.settle();
    kernel.graph.on_task_stopped(db, ExitInfo::code(1));
    kernel.graph.settle();
    let (epoch, _) = kernel.graph.timer(db).expect("backing off");
    kernel.graph.sent.clear();

    let (reply, mut rx) = tokio::sync::oneshot::channel();
    let quit = KernelCommand::QuitWithin { save: true, reply };
    let timeout = KernelCommand::StateTimeout(db, epoch);
    if timeout_first {
      turn(&mut kernel, timeout);
      turn(&mut kernel, quit);
      assert_eq!(rx.try_recv().unwrap(), grace + KILL_WAIT);
    } else {
      turn(&mut kernel, quit);
      turn(&mut kernel, timeout);
      assert_eq!(rx.try_recv().unwrap(), Duration::ZERO);
      assert_eq!(kernel.graph.tasks[&db].state, TaskState::Idle);
      assert!(!kernel.graph.sent.contains(&(db, SentCmd::Start)));
    }
  }
}

/// A quit that arrives while a quit is saving is covered by it, and still
/// answers its bound.
#[tokio::test]
async fn a_quit_during_a_save_is_answered() {
  let mut fx = Fixture::new();
  let tx = fx.tx.clone();
  // Never answers the freeze, so the save waits on it; stays Starting.
  let a = fx
    .kernel
    .as_mut()
    .unwrap()
    .register_task(path_def("a"), move |_| {
      Box::new(SilentTask { name: "a", tx })
    });
  let _saves = count_saves(&mut fx);
  let handle = fx.run();
  fx.pc.send(KernelCommand::Start(TaskSelector::Id(a), None));
  assert_eq!(fx.recv().await, ("a", RecordedCmd::Start));

  let within = SAVE_TIMEOUT + STOP_TIMEOUT + KILL_WAIT;
  for _ in 0..2 {
    let (reply, rx) = tokio::sync::oneshot::channel();
    fx.pc.send(KernelCommand::QuitWithin { save: true, reply });
    let answer = tokio::time::timeout(Duration::from_secs(1), rx)
      .await
      .expect("quit bound not answered")
      .unwrap();
    assert_eq!(answer, within);
  }
  fx.assert_no_cmd();

  fx.pc.send(KernelCommand::QuitWithoutSave);
  assert_eq!(fx.recv().await, ("a", RecordedCmd::Stop));
  tokio::time::timeout(Duration::from_secs(2), handle)
    .await
    .expect("timed out waiting for kernel to quit")
    .unwrap();
}

// ---- State timers ----

/// The shell holds one timer per task: a task that keeps restarting
/// under a long ready timeout leaves none behind.
#[tokio::test(start_paused = true)]
async fn a_task_holds_at_most_one_timer() {
  let mut kernel = Kernel::new();
  let a = kernel.register_task(
    TaskDef {
      ready: ReadyMode::Reported {
        timeout: Some(Duration::from_secs(3600)),
      },
      restart: RestartMode::Always,
      pinned: true,
      ..path_def("a")
    },
    |_| Box::new(crate::kernel::task::TargetTask),
  );
  let step = |kernel: &mut Kernel, timers: &mut StateTimers, command| {
    let _ = kernel.dispatch(KernelMessage { from: a, command });
    kernel.after_dispatch(timers);
    (timers.queue.len(), timers.keys.len())
  };
  let mut timers = StateTimers::new();
  kernel.after_dispatch(&mut timers);
  for _ in 0..10 {
    // Running, under its ready timeout.
    assert_eq!(kernel.graph.tasks[&a].state, TaskState::Running);
    assert_eq!((timers.queue.len(), timers.keys.len()), (1, 1));
    // Ready ends the ready timeout.
    let ready = KernelCommand::TaskReady;
    assert_eq!(step(&mut kernel, &mut timers, ready), (0, 0));
    // The exit backs off; its timeout starts the task again.
    let exit = KernelCommand::TaskStopped(ExitInfo::code(0));
    assert_eq!(step(&mut kernel, &mut timers, exit), (1, 1));
    let (epoch, _) = kernel.graph.timer(a).unwrap();
    let timeout = KernelCommand::StateTimeout(a, epoch);
    assert_eq!(step(&mut kernel, &mut timers, timeout), (1, 1));
  }
}

// ---- Up ----

fn autostart() -> TaskSelector {
  TaskSelector::Tag(SpaceSelector::default_space(), "autostart".to_string())
}

async fn up(fx: &Fixture) -> usize {
  let (tx, rx) = tokio::sync::oneshot::channel();
  fx.pc.send(KernelCommand::Up(autostart(), Some(tx)));
  tokio::time::timeout(Duration::from_secs(1), rx)
    .await
    .expect("up not answered")
    .unwrap()
}

/// Finding #2: `up` on a project that is up leaves a done job and its
/// dependent alone.
#[tokio::test]
async fn up_twice_leaves_a_done_job_and_its_dependent_alone() {
  let mut fx = Fixture::new();
  let setup = fx.add(
    "setup",
    TaskDef {
      kind: TaskKind::Job,
      ..tagged_def("setup", "autostart")
    },
  );
  let db = fx.add(
    "db",
    TaskDef {
      deps: vec![TaskSelector::Id(setup)],
      ..tagged_def("db", "autostart")
    },
  );
  let handle = fx.run();

  assert_eq!(up(&fx).await, 2);
  assert_eq!(fx.recv().await, ("setup", RecordedCmd::Start));
  fx.pc.send_msg(setup, Report::Stopped(ExitInfo::code(0)));
  assert_eq!(fx.recv().await, ("db", RecordedCmd::Start));
  fx.flush().await;

  for _ in 0..2 {
    assert_eq!(up(&fx).await, 2);
    fx.flush().await;
    fx.assert_no_cmd();
  }
  assert_eq!(
    state_of(&fx.pc, setup).await,
    Some(TaskState::Done(ExitInfo::code(0)))
  );
  assert_eq!(state_of(&fx.pc, db).await, Some(TaskState::Ready));

  fx.quit(handle).await;
}

/// Unchanged: a start that targets a done job, by tag too, runs it again
/// (and bounces its dependent while it does).
#[tokio::test]
async fn start_by_tag_reruns_a_done_job() {
  let mut fx = Fixture::new();
  let setup = fx.add(
    "setup",
    TaskDef {
      kind: TaskKind::Job,
      ..tagged_def("setup", "autostart")
    },
  );
  fx.add(
    "db",
    TaskDef {
      deps: vec![TaskSelector::Id(setup)],
      ..tagged_def("db", "autostart")
    },
  );
  let handle = fx.run();
  up(&fx).await;
  assert_eq!(fx.recv().await, ("setup", RecordedCmd::Start));
  fx.pc.send_msg(setup, Report::Stopped(ExitInfo::code(0)));
  assert_eq!(fx.recv().await, ("db", RecordedCmd::Start));

  fx.pc.send(KernelCommand::Start(autostart(), None));
  let mut cmds = vec![fx.recv().await, fx.recv().await];
  cmds.sort();
  assert_eq!(
    cmds,
    [("db", RecordedCmd::Stop), ("setup", RecordedCmd::Start)]
  );

  fx.quit(handle).await;
}

/// `up` starts what crashed: a task backing off retries now, and an
/// exited one runs again.
#[test]
fn up_retries_a_crashed_autostart_task() {
  let mut kernel = Kernel::new();
  let add = |kernel: &mut Kernel, path, restart| {
    kernel.register_task(
      TaskDef {
        restart,
        ..tagged_def(path, "autostart")
      },
      |_| Box::new(crate::kernel::task::TargetTask),
    )
  };
  let backoff = add(&mut kernel, "backoff", RestartMode::OnFailure);
  let exited = add(&mut kernel, "exited", RestartMode::Never);
  turn(&mut kernel, KernelCommand::Up(autostart(), None));
  for id in [backoff, exited] {
    kernel.graph.on_task_stopped(id, ExitInfo::code(1));
  }
  kernel.graph.settle();
  assert_eq!(
    kernel.graph.tasks[&backoff].state,
    TaskState::Backoff(ExitInfo::code(1))
  );
  assert_eq!(
    kernel.graph.tasks[&exited].state,
    TaskState::Exited(ExitInfo::code(1))
  );
  kernel.graph.sent.clear();

  turn(&mut kernel, KernelCommand::Up(autostart(), None));
  for id in [backoff, exited] {
    assert!(kernel.graph.sent.contains(&(id, SentCmd::Start)), "{id:?}");
    assert_eq!(kernel.graph.tasks[&id].state, TaskState::Ready);
  }
}

/// `up` starts the tasks saved as started once: it consumes the saved
/// pins, so a task stopped afterwards stays stopped on the next `up`.
#[test]
fn up_starts_saved_pins_once() {
  let mut kernel = Kernel::new();
  let add = |kernel: &mut Kernel, def| {
    kernel.register_task(def, |_| Box::new(crate::kernel::task::TargetTask))
  };
  let saved = |path| TaskDef {
    saved_pin: true,
    ..path_def(path)
  };
  let a = add(&mut kernel, saved("a"));
  let b = add(&mut kernel, saved("b"));
  let auto = add(&mut kernel, tagged_def("auto", "autostart"));
  let other = add(&mut kernel, path_def("other"));
  kernel.graph.settle();
  // Restored idle: nothing starts before `up`.
  assert!(kernel.graph.sent.is_empty());
  assert!(kernel.graph.explain(a).unwrap().saved_pin);

  let up = |kernel: &mut Kernel| {
    turn_matching(kernel, |ack| KernelCommand::Up(autostart(), ack))
  };
  assert_eq!(up(&mut kernel), 3);
  for id in [a, b, auto] {
    assert!(pinned(&kernel, id), "{id:?}");
    assert_eq!(kernel.graph.tasks[&id].state, TaskState::Ready);
    assert!(!kernel.graph.tasks[&id].saved_pin);
  }
  assert_eq!(kernel.graph.tasks[&other].state, TaskState::Idle);

  turn(&mut kernel, KernelCommand::Stop(TaskSelector::Id(a), None));
  kernel.graph.sent.clear();
  assert_eq!(up(&mut kernel), 1);
  assert_eq!(kernel.graph.tasks[&a].state, TaskState::Idle);
  assert!(kernel.graph.sent.is_empty());
}

/// The latest wish wins: stopping a task before `up` drops its saved pin.
#[test]
fn a_stop_before_up_clears_the_saved_pin() {
  let mut kernel = Kernel::new();
  let a = kernel.register_task(
    TaskDef {
      saved_pin: true,
      ..path_def("a")
    },
    |_| Box::new(crate::kernel::task::TargetTask),
  );
  turn(&mut kernel, KernelCommand::Stop(TaskSelector::Id(a), None));
  assert!(!kernel.graph.tasks[&a].saved_pin);
  let matched =
    turn_matching(&mut kernel, |ack| KernelCommand::Up(autostart(), ack));
  assert_eq!(matched, 0);
  assert_eq!(kernel.graph.tasks[&a].state, TaskState::Idle);
  assert!(kernel.graph.sent.is_empty());
}

/// A live upgrade is the same session: the saved pin comes back from the
/// snapshot and is written into the next one.
#[test]
fn saved_pin_is_carried_through_a_snapshot() {
  let saved = snap::Task {
    saved_pin: true,
    ..saved_task(snap::TaskState::Idle {})
  };
  let mut kernel = Kernel::new();
  kernel
    .graph
    .register_task_with_id(
      TaskId(1),
      path_def("a"),
      Box::new(|_| Box::new(crate::kernel::task::TargetTask)),
      Some(&saved),
    )
    .unwrap();
  assert!(kernel.graph.tasks[&TaskId(1)].saved_pin);
  let snapshot = kernel.graph.snapshot(HashMap::from([(
    TaskId(1),
    TaskKindSnapshot::Console {
      groups: Default::default(),
    },
  )]));
  assert!(snapshot.tasks[0].saved_pin);
}

async fn described(pc: &TaskContext) -> Vec<TaskDescription> {
  pc.describe(TaskSelector::all()).await
}

fn update(deps: Vec<TaskSelector>, label: &str) -> Box<TaskUpdate> {
  let mut spec = crate::spec::TaskSpec::with_cmd(vec!["x".to_string()]);
  spec.set_label(Some(label));
  Box::new(TaskUpdate {
    next: NextRun {
      kind: TaskKind::Job,
      ready: ReadyMode::Immediate,
      stop_timeout: STOP_TIMEOUT,
      spec: Some(spec),
    },
    restart: RestartMode::Never,
    deps,
    tags: vec!["t".to_string()],
    label: Some(label.to_string()),
    was: None,
    restart_running: false,
  })
}

/// An update while a task is getting ready leaves that run getting ready
/// as it started to; the next run gets ready the new way.
#[tokio::test]
async fn an_update_while_getting_ready_applies_from_the_next_start() {
  let mut fx = Fixture::new();
  let tx = fx.tx.clone();
  let api = fx.kernel.as_mut().unwrap().register_task(
    TaskDef {
      spec: Some(crate::spec::TaskSpec::with_cmd(vec!["api".into()])),
      ..path_def("api")
    },
    move |_| Box::new(SilentTask { name: "api", tx }),
  );
  let handle = fx.run();
  fx.pc
    .send(KernelCommand::Start(TaskSelector::Id(api), None));
  assert_eq!(fx.recv().await, ("api", RecordedCmd::Start));

  let (tx, rx) = tokio::sync::oneshot::channel();
  let mut reported = update(Vec::new(), "api");
  reported.next.kind = TaskKind::Service;
  reported.next.ready = ReadyMode::Reported { timeout: None };
  fx.pc
    .send(KernelCommand::Update(TaskSelector::Id(api), reported, tx));
  assert_eq!(rx.await.unwrap(), Ok(1));
  fx.pc.send_msg(api, Report::Started);
  fx.flush().await;
  assert_eq!(state_of(&fx.pc, api).await, Some(TaskState::Ready));

  fx.pc
    .send(KernelCommand::Restart(TaskSelector::Id(api), None));
  assert_eq!(fx.recv().await, ("api", RecordedCmd::Stop));
  assert_eq!(fx.recv().await, ("api", RecordedCmd::Start));
  fx.pc.send_msg(api, Report::Started);
  fx.flush().await;
  assert_eq!(state_of(&fx.pc, api).await, Some(TaskState::Running));

  fx.quit(handle).await;
}

/// A run keeps what it started from: an update during it is the next
/// run's, its stop timeout too, and a snapshot carries both definitions.
#[tokio::test]
async fn a_run_keeps_its_definition_through_an_update() {
  let mut fx = Fixture::new();
  let api = fx.add(
    "api",
    TaskDef {
      spec: Some(crate::spec::TaskSpec::with_cmd(vec!["api".into()])),
      ..path_def("api")
    },
  );
  let handle = fx.run();
  fx.pc
    .send(KernelCommand::Start(TaskSelector::Id(api), None));
  assert_eq!(fx.recv().await, ("api", RecordedCmd::Start));
  fx.flush().await;

  let (tx, rx) = tokio::sync::oneshot::channel();
  let mut slow = update(Vec::new(), "v2");
  slow.next.kind = TaskKind::Service;
  slow.next.stop_timeout = Duration::from_secs(60);
  fx.pc
    .send(KernelCommand::Update(TaskSelector::Id(api), slow, tx));
  assert_eq!(rx.await.unwrap(), Ok(1));
  fx.flush().await;
  let snapshot = freeze(&fx).await;
  let task = &snapshot.tasks[0];
  assert_eq!(task.stop_timeout_ms, None, "the run's own");
  let said = |spec: &Option<crate::spec::TaskSpec>| {
    spec.as_ref().and_then(|s| s.label().map(String::from))
  };
  assert_eq!(said(&task.spec).as_deref(), Some("v2"));
  assert_eq!(
    task.run_spec.as_ref().and_then(|s| s.argv()),
    Some(vec!["api".into()])
  );
  fx.pc.send(KernelCommand::Thaw);

  fx.pc
    .send(KernelCommand::Restart(TaskSelector::Id(api), None));
  assert_eq!(fx.recv().await, ("api", RecordedCmd::Stop));
  assert_eq!(fx.recv().await, ("api", RecordedCmd::Start));
  fx.flush().await;
  let snapshot = freeze(&fx).await;
  let task = &snapshot.tasks[0];
  assert_eq!(task.stop_timeout_ms, Some(60_000));
  assert_eq!(task.run_spec, None, "the run is the definition now");
  fx.pc.send(KernelCommand::Thaw);

  fx.quit(handle).await;
}

/// A copy is made from the original's definition, after it, with its
/// deps; one that can't be registered is refused, not reported done.
#[tokio::test]
async fn a_copy_is_made_from_the_definition() {
  let mut fx = Fixture::new();
  let db = fx.add("db", path_def("db"));
  let make_copy: crate::kernel::kernel_message::MakeCopy =
    std::sync::Arc::new(|task_id, key, spec| {
      Ok(TaskRegistration {
        task_id,
        def: TaskDef {
          path: key.map(|key| key.path),
          label: spec.label().map(String::from),
          spec: Some(spec),
          ..TaskDef::default()
        },
        factory: Box::new(|_| Box::new(crate::kernel::task::TargetTask)),
      })
    });
  let api = fx.add(
    "api",
    TaskDef {
      deps: vec![TaskSelector::Id(db)],
      spec: Some(crate::spec::TaskSpec::with_cmd(vec!["api".into()])),
      make_copy: Some(make_copy),
      ..path_def("api")
    },
  );
  let handle = fx.run();
  let copy = |path: &str| {
    let (tx, rx) = tokio::sync::oneshot::channel();
    fx.pc.send(KernelCommand::Duplicate(
      TaskSelector::Id(api),
      DuplicateRequest {
        label: Some("API 2".into()),
        path: Some(TaskPath::new(path).unwrap()),
        start: false,
      },
      tx,
    ));
    rx
  };
  assert_eq!(copy("api2").await.unwrap(), Ok(1));
  let tasks = described(&fx.pc).await;
  let names: Vec<String> = tasks.iter().map(|t| t.info.name()).collect();
  assert_eq!(names, ["db", "api", "api2"]);
  assert_eq!(tasks[2].info.label.as_deref(), Some("API 2"));
  let copied = tasks[2].spec.as_ref().unwrap();
  assert_eq!(copied.argv(), Some(vec!["api".into()]));
  assert_eq!(copied.deps(), ["db"]);
  assert!(matches!(
    copy("db").await.unwrap(),
    Err(RegisterError::PathTaken(_))
  ));
  // A task with no definition has none to copy.
  let (tx, rx) = tokio::sync::oneshot::channel();
  fx.pc.send(KernelCommand::Duplicate(
    TaskSelector::Id(db),
    DuplicateRequest {
      label: None,
      path: None,
      start: false,
    },
    tx,
  ));
  assert_eq!(rx.await.unwrap(), Err(RegisterError::NoDefinition));

  fx.pc.send(KernelCommand::Remove(TaskSelector::all(), None));
  fx.quit(handle).await;
}

/// A running job edited into a service still ends as a job, done for
/// what needs it; it is a service from its next start.
#[tokio::test]
async fn a_running_job_edited_into_a_service_ends_as_a_job() {
  let mut fx = Fixture::new();
  let migrate = fx.add(
    "migrate",
    TaskDef {
      kind: TaskKind::Job,
      spec: Some(crate::spec::TaskSpec::with_cmd(vec!["migrate".into()])),
      ..path_def("migrate")
    },
  );
  let handle = fx.run();
  fx.pc
    .send(KernelCommand::Start(TaskSelector::Id(migrate), None));
  assert_eq!(fx.recv().await, ("migrate", RecordedCmd::Start));

  let (tx, rx) = tokio::sync::oneshot::channel();
  let mut service = update(Vec::new(), "migrate");
  service.next.kind = TaskKind::Service;
  fx.pc.send(KernelCommand::Update(
    TaskSelector::Id(migrate),
    service,
    tx,
  ));
  assert_eq!(rx.await.unwrap(), Ok(1));
  fx.pc.send_msg(migrate, Report::Stopped(ExitInfo::code(0)));
  fx.flush().await;
  assert!(matches!(
    state_of(&fx.pc, migrate).await,
    Some(TaskState::Done(_))
  ));

  fx.pc
    .send(KernelCommand::Start(TaskSelector::Id(migrate), None));
  assert_eq!(fx.recv().await, ("migrate", RecordedCmd::Start));
  fx.flush().await;
  let tasks = described(&fx.pc).await;
  assert_eq!(tasks[0].kind, TaskKind::Service);

  fx.quit(handle).await;
}

/// An update made from a definition the task no longer has is refused:
/// it would undo the change in between.
#[tokio::test]
async fn an_update_from_another_definition_is_refused() {
  let mut fx = Fixture::new();
  let was = crate::spec::TaskSpec::with_cmd(vec!["api".into()]);
  let api = fx.add(
    "api",
    TaskDef {
      spec: Some(was.clone()),
      ..path_def("api")
    },
  );
  let handle = fx.run();

  for (label, result) in [("A", Ok(1)), ("B", Err(RegisterError::Changed))] {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let mut redefined = update(Vec::new(), label);
    redefined.was = Some(was.clone());
    fx.pc
      .send(KernelCommand::Update(TaskSelector::Id(api), redefined, tx));
    assert_eq!(rx.await.unwrap(), result);
  }
  let tasks = described(&fx.pc).await;
  assert_eq!(tasks[0].info.label.as_deref(), Some("A"));
  // Nor one that is gone.
  let (tx, rx) = tokio::sync::oneshot::channel();
  let mut redefined = update(Vec::new(), "C");
  redefined.was = Some(was);
  fx.pc.send(KernelCommand::Update(
    TaskSelector::Id(TaskId(999)),
    redefined,
    tx,
  ));
  assert_eq!(rx.await.unwrap(), Err(RegisterError::Changed));

  fx.quit(handle).await;
}

/// A task redefined in place keeps its id and place; its deps are
/// replaced, and one that needs the task is refused with nothing changed,
/// as is one with no definition.
#[tokio::test]
async fn update_redefines_in_place_and_refuses_a_cycle() {
  let mut fx = Fixture::new();
  let db = fx.add("db", path_def("db"));
  let cache = fx.add(
    "cache",
    TaskDef {
      spec: Some(crate::spec::TaskSpec::with_cmd(vec!["cache".into()])),
      ..path_def("cache")
    },
  );
  let api = fx.add(
    "api",
    TaskDef {
      deps: vec![TaskSelector::Id(db)],
      spec: Some(crate::spec::TaskSpec::with_cmd(vec!["api".into()])),
      ..path_def("api")
    },
  );
  let handle = fx.run();

  let (tx, rx) = tokio::sync::oneshot::channel();
  let mut redefined = update(vec![TaskSelector::Id(cache)], "API");
  let spec = redefined.next.spec.as_mut().unwrap();
  spec.set_deps(vec!["cache".to_string()]);
  fx.pc
    .send(KernelCommand::Update(TaskSelector::Id(api), redefined, tx));
  assert_eq!(rx.await.unwrap(), Ok(1));
  let tasks = described(&fx.pc).await;
  let api_now = tasks.iter().find(|t| t.info.id == api).unwrap();
  assert_eq!(api_now.kind, TaskKind::Job);
  assert_eq!(api_now.info.label.as_deref(), Some("API"));
  let spec = api_now.spec.as_ref().unwrap();
  assert_eq!(spec.deps(), ["cache"]);
  assert_eq!(spec.label(), Some("API"));
  // A spec and no origin: the runner's own.
  let dynamic = fx
    .pc
    .list_tasks(TaskSelector::Tag(
      SpaceSelector::default_space(),
      "dynamic".to_string(),
    ))
    .await;
  assert!(dynamic.iter().any(|t| t.id == api));

  // cache -> api would close cache -> api -> cache.
  let (tx, rx) = tokio::sync::oneshot::channel();
  fx.pc.send(KernelCommand::Update(
    TaskSelector::Id(cache),
    update(vec![TaskSelector::Id(api)], "C"),
    tx,
  ));
  assert!(matches!(rx.await.unwrap(), Err(RegisterError::DepCycle(_))));
  let tasks = described(&fx.pc).await;
  let cache_now = tasks.iter().find(|t| t.info.id == cache).unwrap();
  assert_eq!(cache_now.kind, TaskKind::Service, "nothing changed");
  let names: Vec<String> = tasks.iter().map(|t| t.info.name()).collect();
  assert_eq!(names, ["db", "cache", "api"]);

  let (tx, rx) = tokio::sync::oneshot::channel();
  fx.pc.send(KernelCommand::Update(
    TaskSelector::Id(db),
    update(Vec::new(), "D"),
    tx,
  ));
  assert_eq!(rx.await.unwrap(), Err(RegisterError::NoDefinition));
  let tasks = described(&fx.pc).await;
  assert!(tasks[0].spec.is_none(), "nothing changed");

  fx.quit(handle).await;
}

/// A task's deps are its edges: its definition names each by where it
/// is, after a move too, and not once it is gone.
#[tokio::test]
async fn deps_are_the_tasks_edges() {
  let mut fx = Fixture::new();
  let db = fx.add("db", path_def("db"));
  let cache = fx.add("cache", path_def("cache"));
  let mut spec = crate::spec::TaskSpec::with_cmd(vec!["api".into()]);
  // What the definition said is not kept: the edges are.
  spec.set_deps(vec!["db".to_string(), "gone".to_string()]);
  let api = fx.add(
    "api",
    TaskDef {
      deps: vec![TaskSelector::Id(db), TaskSelector::Id(cache)],
      spec: Some(spec),
      ..path_def("api")
    },
  );
  let handle = fx.run();
  let deps = |tasks: Vec<TaskDescription>| {
    let api = tasks.into_iter().find(|t| t.info.id == api).unwrap();
    api.spec.unwrap().deps()
  };
  assert_eq!(deps(described(&fx.pc).await), ["cache", "db"]);

  let (reply, rx) = tokio::sync::oneshot::channel();
  fx.pc.send(KernelCommand::Move(
    MoveTask {
      space: TaskSpaceId::default_space(),
      from: TaskPath::new("db").unwrap(),
      to: TaskPath::new("data/db").unwrap(),
      place: Place::Last,
    },
    reply,
  ));
  assert_eq!(rx.await.unwrap(), Ok(1));
  assert_eq!(deps(described(&fx.pc).await), ["cache", "data/db"]);

  fx.pc
    .send(KernelCommand::Remove(TaskSelector::Id(db), None));
  assert_eq!(deps(described(&fx.pc).await), ["cache"]);

  fx.pc.send(KernelCommand::Remove(TaskSelector::all(), None));
  fx.quit(handle).await;
}

/// A move tells the console where each moved task is now, in its new
/// list order.
#[tokio::test]
async fn move_relists_a_group_and_notifies_in_order() {
  let mut fx = Fixture::new();
  let handle = fx.run();
  let pc = fx.pc.clone();
  let (tx, mut moved) = unbounded_channel();
  let (subscribed_tx, subscribed_rx) = tokio::sync::oneshot::channel();
  let ack = pc.spawn_async_with_id(
    pc.alloc_id(),
    TaskDef::default(),
    move |pc, mut cmds| async move {
      pc.subscribe_path(
        TaskKey::default_space(TaskPath::root()),
        SubMode::Subtree,
      );
      subscribed_tx.send(()).unwrap();
      while let Some(cmd) = cmds.recv().await {
        if let TaskCmd::Msg(msg) = cmd
          && let Ok(n) = msg.downcast::<TaskNotification>()
          && let TaskNotify::Moved { path, after } = n.notify
        {
          tx.send((n.from, path.unwrap().to_string(), after)).unwrap();
        }
      }
    },
  );
  assert!(ack.await.unwrap().is_ok());
  subscribed_rx.await.unwrap();
  let mut ids = Vec::new();
  for path in ["db", "web/api", "web/ui"] {
    let id = pc.alloc_id();
    let ack = pc.register_task(TaskRegistration {
      task_id: id,
      def: path_def(path),
      factory: Box::new(|_| Box::new(crate::kernel::task::TargetTask)),
    });
    assert!(ack.await.unwrap().is_ok());
    ids.push(id);
  }
  let [db, api, ui] = ids[..] else {
    unreachable!()
  };

  let (reply, rx) = tokio::sync::oneshot::channel();
  pc.send(KernelCommand::Move(
    MoveTask {
      space: TaskSpaceId::default_space(),
      from: TaskPath::new("web").unwrap(),
      to: TaskPath::new("apps/web").unwrap(),
      place: Place::First,
    },
    reply,
  ));
  assert_eq!(rx.await.unwrap(), Ok(2));
  // `apps` is new: listed last, after `db`.
  assert_eq!(
    moved.recv().await.unwrap(),
    (api, "apps/web/api".to_string(), Some(db))
  );
  assert_eq!(
    moved.recv().await.unwrap(),
    (ui, "apps/web/ui".to_string(), Some(api))
  );
  let names: Vec<String> =
    described(&pc).await.iter().map(|t| t.info.name()).collect();
  assert_eq!(names, ["db", "apps/web/api", "apps/web/ui"]);

  let (reply, rx) = tokio::sync::oneshot::channel();
  pc.send(KernelCommand::Move(
    MoveTask {
      space: TaskSpaceId::default_space(),
      from: TaskPath::new("db").unwrap(),
      to: TaskPath::new("apps/web/api/db").unwrap(),
      place: Place::Last,
    },
    reply,
  ));
  assert!(matches!(rx.await.unwrap(), Err(MoveError::UnderTask(_))));

  pc.send(KernelCommand::Remove(TaskSelector::all(), None));
  fx.quit(handle).await;
}

/// A run starts from the definition as it is then, and where the task
/// is listed then: a move since names its log.
#[tokio::test]
async fn a_run_starts_where_the_task_is() {
  struct Told(UnboundedSender<Option<String>>);
  impl Task for Told {
    fn handle_cmd(&mut self, cmd: TaskCmd, fx: &mut Effects) {
      match cmd {
        TaskCmd::Start(run) => {
          let path = run.and_then(|run| run.path).map(|p| p.to_string());
          self.0.send(path).unwrap();
          fx.started();
        }
        TaskCmd::Stop | TaskCmd::Kill => fx.stopped(ExitInfo::code(0)),
        TaskCmd::Msg(_) | TaskCmd::Freeze(_) | TaskCmd::Thaw => (),
      }
    }
  }
  let mut fx = Fixture::new();
  let (tx, mut told) = unbounded_channel();
  let kernel = fx.kernel.as_mut().unwrap();
  let api = kernel.register_task(
    TaskDef {
      spec: Some(crate::spec::TaskSpec::with_cmd(vec!["api".into()])),
      ..path_def("web/api")
    },
    move |_| Box::new(Told(tx)),
  );
  let handle = fx.run();
  let (reply, rx) = tokio::sync::oneshot::channel();
  fx.pc.send(KernelCommand::Move(
    MoveTask {
      space: TaskSpaceId::default_space(),
      from: TaskPath::new("web").unwrap(),
      to: TaskPath::new("apps/web").unwrap(),
      place: Place::Last,
    },
    reply,
  ));
  assert_eq!(rx.await.unwrap(), Ok(1));
  fx.pc
    .send(KernelCommand::Start(TaskSelector::Id(api), None));
  assert_eq!(told.recv().await.unwrap().as_deref(), Some("apps/web/api"));
  fx.quit(handle).await;
}

/// A task given another origin tells the console, which marks the
/// runner's own; the same one again tells nothing.
#[tokio::test]
async fn a_new_origin_is_told() {
  let mut fx = Fixture::new();
  let handle = fx.run();
  let pc = fx.pc.clone();
  let (tx, mut told) = unbounded_channel();
  let (subscribed_tx, subscribed_rx) = tokio::sync::oneshot::channel();
  let ack = pc.spawn_async_with_id(
    pc.alloc_id(),
    TaskDef::default(),
    move |pc, mut cmds| async move {
      pc.subscribe_path(
        TaskKey::default_space(TaskPath::root()),
        SubMode::Subtree,
      );
      subscribed_tx.send(()).unwrap();
      while let Some(cmd) = cmds.recv().await {
        if let TaskCmd::Msg(msg) = cmd
          && let Ok(n) = msg.downcast::<TaskNotification>()
        {
          match n.notify {
            TaskNotify::Changed { .. } => tx.send("changed").unwrap(),
            TaskNotify::Removed => tx.send("removed").unwrap(),
            _ => (),
          }
        }
      }
    },
  );
  assert!(ack.await.unwrap().is_ok());
  subscribed_rx.await.unwrap();
  let id = pc.alloc_id();
  let ack = pc.register_task(TaskRegistration {
    task_id: id,
    def: path_def("api"),
    factory: Box::new(|_| Box::new(crate::kernel::task::TargetTask)),
  });
  assert!(ack.await.unwrap().is_ok());
  let origin = crate::spec::Source {
    file: "dekit.yaml".into(),
    key: "api".into(),
    mount: String::new(),
  };
  for origin in [Some(origin.clone()), Some(origin), None] {
    let (ack, rx) = tokio::sync::oneshot::channel();
    pc.send(KernelCommand::SetOrigin(
      TaskSelector::Id(id),
      origin,
      Some(ack),
    ));
    assert_eq!(rx.await.unwrap(), 1);
  }
  pc.send(KernelCommand::Remove(TaskSelector::Id(id), None));
  let mut seen = Vec::new();
  while seen.last() != Some(&"removed") {
    seen.push(told.recv().await.unwrap());
  }
  assert_eq!(seen, ["changed", "changed", "removed"]);

  pc.send(KernelCommand::Remove(TaskSelector::all(), None));
  fx.quit(handle).await;
}

// ---- Folders ----

/// What a subscriber hears of the tasks and folders.
#[derive(Debug, Eq, PartialEq)]
enum Heard {
  Task(TaskId, Option<TaskId>),
  Folder(TaskId, String, Option<TaskId>, bool),
  Removed(TaskId),
  Moved(TaskId, String, Option<TaskId>),
}

/// A task subscribed to the default space, and what it hears.
async fn listen(pc: &TaskContext) -> UnboundedReceiver<Heard> {
  let (tx, rx) = unbounded_channel();
  let (subscribed_tx, subscribed_rx) = tokio::sync::oneshot::channel();
  let ack = pc.spawn_async_with_id(
    pc.alloc_id(),
    TaskDef::default(),
    move |pc, mut cmds| async move {
      pc.subscribe_path(
        TaskKey::default_space(TaskPath::root()),
        SubMode::Subtree,
      );
      subscribed_tx.send(()).unwrap();
      while let Some(cmd) = cmds.recv().await {
        let msg = match cmd {
          TaskCmd::Msg(msg) => msg,
          TaskCmd::Freeze(number) => {
            let kind = TaskKindSnapshot::Console {
              groups: Default::default(),
            };
            pc.send(KernelCommand::TaskFrozen(number, kind));
            continue;
          }
          TaskCmd::Start(_) | TaskCmd::Stop | TaskCmd::Kill | TaskCmd::Thaw => {
            continue;
          }
        };
        let Ok(n) = msg.downcast::<TaskNotification>() else {
          continue;
        };
        let heard = match n.notify {
          TaskNotify::Added { after, .. } => Heard::Task(n.from, after),
          TaskNotify::FolderAdded {
            path,
            after,
            collapsed,
          } => Heard::Folder(n.from, path.to_string(), after, collapsed),
          TaskNotify::Removed => Heard::Removed(n.from),
          TaskNotify::Moved { path, after } => {
            let path = path.map_or(String::new(), |p| p.to_string());
            Heard::Moved(n.from, path, after)
          }
          TaskNotify::StateChanged(_) | TaskNotify::Changed { .. } => continue,
        };
        tx.send(heard).unwrap();
      }
    },
  );
  assert!(ack.await.unwrap().is_ok());
  subscribed_rx.await.unwrap();
  rx
}

fn folder(id: TaskId, path: &str, collapsed: bool) -> Registration {
  Registration::Folder(FolderDef {
    id,
    space: TaskSpaceId::default_space(),
    path: TaskPath::new(path).unwrap(),
    place: Place::Last,
    settings: FolderSettings {
      collapsed,
      origin: None,
    },
  })
}

impl Fixture {
  fn task(&self, id: TaskId, path: &str) -> Registration {
    let tx = self.tx.clone();
    Registration::Task(TaskRegistration {
      task_id: id,
      def: path_def(path),
      factory: Box::new(move |ctx| {
        Box::new(RecordingTask {
          name: "task",
          tx,
          ctx,
        })
      }),
    })
  }

  async fn folder_paths(&self) -> Vec<String> {
    let folders = self.pc.folders(TaskSpaceId::default_space()).await;
    folders.iter().map(|f| f.path.to_string()).collect()
  }

  async fn task_paths(&self) -> Vec<String> {
    let tasks = self.pc.list_tasks(TaskSelector::all()).await;
    tasks.iter().map(|task| task.name()).collect()
  }
}

async fn heard(rx: &mut UnboundedReceiver<Heard>, n: usize) -> Vec<Heard> {
  let mut heard = Vec::new();
  for _ in 0..n {
    let next = tokio::time::timeout(Duration::from_secs(1), rx.recv());
    heard.push(next.await.unwrap().unwrap());
  }
  heard
}

/// A batch lists its folders among its tasks in the order given, and
/// registers them first; a subscriber that comes later hears them all
/// in list order, each after the one before it.
#[tokio::test]
async fn folders_are_listed_in_place() {
  let mut fx = Fixture::new();
  let handle = fx.run();
  let pc = fx.pc.clone();
  let mut early = listen(&pc).await;
  let [db, apps, zz, tools, web] = [(); 5].map(|_| pc.alloc_id());
  let results = pc
    .register_tasks(vec![
      fx.task(db, "db"),
      folder(apps, "apps", true),
      fx.task(zz, "zz"),
      folder(tools, "tools", false),
      fx.task(web, "apps/web"),
    ])
    .await
    .unwrap();
  assert!(results.iter().all(Result::is_ok), "{results:?}");
  // The folders first, so a task under one is listed in it; each after
  // what is there before it.
  assert_eq!(
    heard(&mut early, 5).await,
    [
      Heard::Folder(apps, "apps".into(), None, true),
      Heard::Folder(tools, "tools".into(), Some(apps), false),
      Heard::Task(db, None),
      Heard::Task(zz, Some(apps)),
      Heard::Task(web, Some(apps)),
    ]
  );
  assert_eq!(fx.folder_paths().await, ["apps", "tools"]);
  assert_eq!(fx.task_paths().await, ["db", "apps/web", "zz"]);

  let mut late = listen(&pc).await;
  assert_eq!(
    heard(&mut late, 5).await,
    [
      Heard::Task(db, None),
      Heard::Folder(apps, "apps".into(), Some(db), true),
      Heard::Task(web, Some(apps)),
      Heard::Task(zz, Some(web)),
      Heard::Folder(tools, "tools".into(), Some(zz), false),
    ]
  );

  // Saved in list order, each with what it goes back after.
  let snapshot = freeze(&fx).await;
  let saved: Vec<(&str, Option<usize>, bool)> = snapshot
    .folders
    .iter()
    .map(|f| (f.path.as_str(), f.after, f.collapsed))
    .collect();
  assert_eq!(
    saved,
    [("apps", Some(db.0), true), ("tools", Some(zz.0), false)]
  );
  pc.send(KernelCommand::Thaw);
  fx.quit(handle).await;
}

/// `rm` takes a group or folder with every task and folder under it,
/// whether a path, a glob, or a folder's id names it.
#[tokio::test]
async fn rm_takes_a_group_or_folder_whole() {
  let mut fx = Fixture::new();
  let handle = fx.run();
  let pc = fx.pc.clone();
  let ids: Vec<TaskId> = (0..8).map(|_| pc.alloc_id()).collect();
  let [apps, web, x, empty, db, lint, svc, api] = ids[..] else {
    unreachable!()
  };
  let results = pc
    .register_tasks(vec![
      folder(apps, "apps", false),
      fx.task(web, "apps/web"),
      fx.task(x, "apps/api/x"),
      folder(empty, "apps/empty", false),
      fx.task(db, "db"),
      fx.task(lint, "tools/lint"),
      folder(svc, "svc", false),
      fx.task(api, "svc/api"),
    ])
    .await
    .unwrap();
  assert!(results.iter().all(Result::is_ok), "{results:?}");
  let mut rx = listen(&pc).await;
  heard(&mut rx, 8).await;

  let remove = |selector| {
    let (tx, rx) = tokio::sync::oneshot::channel();
    pc.send(KernelCommand::Remove(selector, Some(tx)));
    async move { rx.await.unwrap() }
  };
  let glob = |pattern: &str| {
    TaskSelector::Glob(SpaceSelector::default_space(), pattern.to_string())
  };
  assert_eq!(remove(glob("apps")).await, 4);
  let mut gone = heard(&mut rx, 4).await;
  gone.sort_by_key(|heard| format!("{heard:?}"));
  let mut expected = [web, x, empty, apps].map(Heard::Removed);
  expected.sort_by_key(|heard| format!("{heard:?}"));
  assert_eq!(gone, expected);
  // A group made by the paths under it.
  assert_eq!(remove(glob("tools")).await, 1);
  assert_eq!(remove(TaskSelector::Id(svc)).await, 2);
  assert_eq!(fx.folder_paths().await, Vec::<String>::new());
  assert_eq!(fx.task_paths().await, ["db"]);
  assert_eq!(remove(glob("nope")).await, 0);
  assert_eq!(remove(glob("**")).await, 1);
  fx.quit(handle).await;
}

/// A dropped folder leaves what is under it; nothing goes where a folder
/// is; a folder's entry changes what it is, not what is listed.
#[tokio::test]
async fn a_folder_is_dropped_or_set_on_its_own() {
  let mut fx = Fixture::new();
  let handle = fx.run();
  let pc = fx.pc.clone();
  let [apps, web, empty] = [(); 3].map(|_| pc.alloc_id());
  pc.register_tasks(vec![
    folder(apps, "apps", true),
    fx.task(web, "apps/web"),
    folder(empty, "empty", false),
  ])
  .await
  .unwrap();

  let refused = pc
    .register_tasks(vec![
      fx.task(pc.alloc_id(), "apps"),
      folder(pc.alloc_id(), "apps/web", false),
      folder(pc.alloc_id(), "apps/web/x", false),
    ])
    .await
    .unwrap();
  let refused: Vec<String> = refused
    .into_iter()
    .map(|result| result.unwrap_err().to_string())
    .collect();
  assert_eq!(
    refused,
    [
      "'apps' is a folder",
      "a task already exists at 'apps/web'",
      "a task can't have tasks under it: 'apps/web/x' is under 'apps/web'",
    ]
  );

  let ack = |command: fn(TaskId, Ack) -> KernelCommand, id| {
    let (tx, rx) = tokio::sync::oneshot::channel();
    pc.send(command(id, Some(tx)));
    async move { rx.await.unwrap() }
  };
  let settings = FolderSettings {
    collapsed: false,
    origin: Some(crate::spec::Source {
      file: "dekit.yaml".into(),
      key: "apps/".into(),
      mount: String::new(),
    }),
  };
  let set = settings.clone();
  let (tx, rx) = tokio::sync::oneshot::channel();
  pc.send(KernelCommand::SetFolder(apps, set, Some(tx)));
  assert_eq!(rx.await.unwrap(), 1);
  let folders = pc.folders(TaskSpaceId::default_space()).await;
  assert_eq!(folders[0].settings, settings);

  assert_eq!(ack(KernelCommand::DropFolder, apps).await, 1);
  assert_eq!(ack(KernelCommand::DropFolder, apps).await, 0);
  assert_eq!(ack(KernelCommand::DropFolder, web).await, 0, "a task");
  assert_eq!(ack(KernelCommand::DropFolder, empty).await, 1);
  assert_eq!(fx.folder_paths().await, Vec::<String>::new());
  assert_eq!(fx.task_paths().await, ["apps/web"]);
  fx.quit(handle).await;
}

/// A folder moves with the group it is in, and its subscribers hear it
/// first, in the new list order.
#[tokio::test]
async fn a_folder_moves_with_its_group() {
  let mut fx = Fixture::new();
  let handle = fx.run();
  let pc = fx.pc.clone();
  let [db, apps, web] = [(); 3].map(|_| pc.alloc_id());
  pc.register_tasks(vec![
    fx.task(db, "db"),
    folder(apps, "g/apps", false),
    fx.task(web, "g/apps/web"),
  ])
  .await
  .unwrap();
  let mut rx = listen(&pc).await;
  heard(&mut rx, 3).await;
  let (tx, reply) = tokio::sync::oneshot::channel();
  pc.send(KernelCommand::Move(
    MoveTask {
      space: TaskSpaceId::default_space(),
      from: TaskPath::new("g").unwrap(),
      to: TaskPath::new("h").unwrap(),
      place: Place::First,
    },
    tx,
  ));
  assert_eq!(reply.await.unwrap(), Ok(2));
  assert_eq!(
    heard(&mut rx, 2).await,
    [
      Heard::Moved(apps, "h/apps".into(), None),
      Heard::Moved(web, "h/apps/web".into(), Some(apps)),
    ]
  );
  assert_eq!(fx.folder_paths().await, ["h/apps"]);
  fx.quit(handle).await;
}
