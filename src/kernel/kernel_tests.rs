use std::time::Duration;

use tokio::sync::mpsc::{
  UnboundedReceiver, UnboundedSender, error::TryRecvError, unbounded_channel,
};

use super::*;
use crate::kernel::kernel_message::KernelSnapshot;
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
      TaskCmd::Start => {
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
      TaskCmd::Duplicate(_) | TaskCmd::Thaw => (),
      TaskCmd::Freeze(number) => self.ctx.send(KernelCommand::TaskFrozen(
        number,
        TaskKindSnapshot::Console {},
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
      TaskCmd::Start => {
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
      TaskCmd::Duplicate(_) | TaskCmd::Freeze(_) | TaskCmd::Thaw => (),
      TaskCmd::Msg(_) => fx.stopped(ExitInfo::code(0)),
    }
  }
}

/// Records commands like `RecordingTask` but never reports starting:
/// stays in Starting until commanded down.
struct SilentTask {
  name: &'static str,
  tx: UnboundedSender<(&'static str, RecordedCmd)>,
}

impl Task for SilentTask {
  fn handle_cmd(&mut self, cmd: TaskCmd, fx: &mut Effects) {
    match cmd {
      TaskCmd::Start => {
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
      TaskCmd::Duplicate(_) | TaskCmd::Freeze(_) | TaskCmd::Thaw => (),
      TaskCmd::Msg(_) => (),
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
      TaskCmd::Start => {
        self.tx.send((self.name, RecordedCmd::Start)).unwrap();
        fx.started();
      }
      TaskCmd::Stop => {
        self.tx.send((self.name, RecordedCmd::Stop)).unwrap();
      }
      TaskCmd::Kill => {
        self.tx.send((self.name, RecordedCmd::Kill)).unwrap();
      }
      TaskCmd::Duplicate(_) | TaskCmd::Thaw => (),
      TaskCmd::Freeze(number) => self.ctx.send(KernelCommand::TaskFrozen(
        number,
        TaskKindSnapshot::Console {},
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
    let rx = self.pc.query(KernelQuery::ListTasks(TaskSelector::all()));
    tokio::time::timeout(Duration::from_secs(1), rx)
      .await
      .expect("timed out waiting for kernel query response")
      .expect("kernel query response channel closed");
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
      ready: ReadyMode::Reported,
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

async fn label_of(pc: &TaskContext, id: TaskId) -> Option<String> {
  let rx = pc.query(KernelQuery::ListTasks(TaskSelector::all()));
  let resp = tokio::time::timeout(Duration::from_secs(1), rx)
    .await
    .expect("timed out listing tasks")
    .expect("kernel query channel closed");
  match resp {
    KernelQueryResponse::TaskList(list) => {
      list.into_iter().find(|t| t.id == id).and_then(|t| t.label)
    }
    _ => panic!("unexpected query response"),
  }
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
  let rx = pc.query(KernelQuery::ListTasks(TaskSelector::all()));
  let resp = tokio::time::timeout(Duration::from_secs(1), rx)
    .await
    .expect("timed out listing tasks")
    .expect("kernel query channel closed");
  match resp {
    KernelQueryResponse::TaskList(list) => {
      list.into_iter().find(|t| t.id == id).map(|t| t.state)
    }
    _ => panic!("unexpected query response"),
  }
}

async fn resolve(pc: &TaskContext, path: &str) -> Option<TaskId> {
  resolve_in(pc, TaskSpaceId::default_space(), path).await
}

async fn resolve_in(
  pc: &TaskContext,
  space: TaskSpaceId,
  path: &str,
) -> Option<TaskId> {
  let rx = pc.query(KernelQuery::ListTasks(TaskSelector::Glob(
    SpaceSelector::One(space),
    path.to_string(),
  )));
  let resp = tokio::time::timeout(Duration::from_secs(1), rx)
    .await
    .expect("timed out resolving path")
    .expect("kernel query channel closed");
  match resp {
    KernelQueryResponse::TaskList(tasks) => tasks.first().map(|t| t.id),
    _ => panic!("unexpected query response"),
  }
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
  tokio::time::advance(STOP_GRACE + Duration::from_millis(1)).await;
  assert_eq!(fx.recv().await, ("a", RecordedCmd::Kill));
  fx.flush().await;

  // The kill is also ignored: the kernel gives up so the graph (and
  // quit) can make progress. Nothing wants the task, so it stays down.
  tokio::time::advance(STOP_GRACE + Duration::from_millis(1)).await;
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
  tokio::time::advance(STOP_GRACE + Duration::from_millis(1)).await;
  assert_eq!(fx.recv().await, ("a", RecordedCmd::Kill));
  tokio::time::advance(STOP_GRACE + Duration::from_millis(1)).await;
  assert_eq!(fx.recv().await, ("a", RecordedCmd::Start));

  // Quit must wind the stubborn task down through both graces again.
  fx.pc.send(KernelCommand::Quit);
  assert_eq!(fx.recv().await, ("a", RecordedCmd::Stop));
  tokio::time::advance(STOP_GRACE + Duration::from_millis(1)).await;
  assert_eq!(fx.recv().await, ("a", RecordedCmd::Kill));
  tokio::time::advance(STOP_GRACE + Duration::from_millis(1)).await;
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
      ready: ReadyMode::Reported,
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

  let rx = fx.pc.query(KernelQuery::Explain(TaskSelector::Id(app)));
  let resp = tokio::time::timeout(Duration::from_secs(1), rx)
    .await
    .unwrap()
    .unwrap();
  let explain = match resp {
    KernelQueryResponse::Explain(mut explains) if explains.len() == 1 => {
      explains.pop().unwrap()
    }
    _ => panic!("missing explain response"),
  };
  assert_eq!(explain.name, "app");
  assert_eq!(explain.state, TaskState::Idle);
  assert!(explain.wanted);
  // Wanted but blocked: the dep has not reported ready yet.
  assert!(!explain.supported);
  assert!(explain.pinned);
  assert!(!explain.vetoed);
  assert_eq!(explain.deps.len(), 1);
  assert_eq!(explain.deps[0].name, "dep");
  assert_eq!(explain.deps[0].state, TaskState::Running);
  assert!(explain.deps[0].wanted);
  assert!(!explain.deps[0].satisfied);

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

  let default = pc
    .query(KernelQuery::ListTasks(TaskSelector::all()))
    .await
    .unwrap();
  let KernelQueryResponse::TaskList(default) = default else {
    panic!("unexpected response");
  };
  assert_eq!(default.len(), 1);
  assert_eq!(default[0].id, default_id);

  let dekit = pc
    .query(KernelQuery::ListTasks(TaskSelector::Glob(
      SpaceSelector::One(TaskSpaceId::dekit()),
      "**".to_string(),
    )))
    .await
    .unwrap();
  let KernelQueryResponse::TaskList(dekit) = dekit else {
    panic!("unexpected response");
  };
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
  let flush = pc.query(KernelQuery::ListTasks(TaskSelector::all()));
  flush.await.unwrap();

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
  match fx
    .pc
    .query(KernelQuery::ListTasks(TaskSelector::Id(a)))
    .await
  {
    Ok(KernelQueryResponse::TaskList(tasks)) => {
      assert_eq!(tasks[0].state, TaskState::Exited(ExitInfo::code(1)));
    }
    _ => panic!("query failed"),
  }

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
  task_ctx.send(KernelCommand::TaskFrozen(1, TaskKindSnapshot::Console {}));
  fx.flush().await;
  assert!(
    second.try_recv().is_err(),
    "a stale answer completed freeze 2"
  );

  task_ctx.send(KernelCommand::TaskFrozen(2, TaskKindSnapshot::Console {}));
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
  assert!(remaining > 0 && remaining <= STOP_GRACE.as_millis() as u64);

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
        TaskRegistration {
          task_id: TaskId(saved.id),
          def,
          factory: Box::new(move |ctx| {
            Box::new(RecordingTask { name, tx, ctx })
          }),
        },
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

  match restored
    .pc
    .query(KernelQuery::Explain(TaskSelector::Id(b)))
    .await
  {
    Ok(KernelQueryResponse::Explain(explain)) => {
      assert_eq!(explain[0].state, TaskState::Ready);
      assert!(explain[0].pinned);
      assert_eq!(explain[0].deps.len(), 1);
    }
    _ => panic!("explain failed"),
  }

  // Dropping the pin now stops b first, then a: the edges survived.
  restored
    .pc
    .send(KernelCommand::Unpin(TaskSelector::Id(b), None));
  assert_eq!(restored.recv().await, ("b", RecordedCmd::Stop));
  assert_eq!(restored.recv().await, ("a", RecordedCmd::Stop));
  restored.quit(handle).await;
}
