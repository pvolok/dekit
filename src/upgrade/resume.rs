//! `dekit runner resume --snapshot PATH`: the entry point a new image
//! starts from after an exec, and with `--check` the test the runner puts
//! a target binary through first. The arguments, the check's answer, and
//! the snapshot header are frozen.

use std::{path::PathBuf, sync::Arc};

use anyhow::Context;
use futures::future::BoxFuture;

use crate::project::Project;
use crate::{
  config::{config::Config, hook::watch_idle},
  console::app::console_task_registration,
  dekit::{
    attach::{Attachment, resume_session},
    main::print_warnings,
    server::{
      Connections, Resumed, ServerCtx, dispatch_connection,
      hello_from_snapshot, init_logging, runner_handle, serve,
    },
  },
  kernel::{
    kernel::Kernel,
    kernel_message::{
      Registration, TaskContext, TaskRegistration, TaskSelector,
    },
    task::{Effects, Task, TaskCmd, TaskDef, TaskId},
    task_key::TaskSpaceId,
  },
  process::unix_processes_waiter::UnixProcessesWaiter,
  protocol::ctl::Hello,
  runner::{
    RunnerKind, RunnerSpec,
    lockfile::{LockFileGuard, runner_paths},
    socket::{ServerSocket, adopt_connection},
  },
  task::{
    config_tasks::{saved_base, saved_task_registration},
    process_task::resumed_instance,
  },
  term::{Geometry, Size},
  upgrade::{set_cloexec, snapshot as snap},
};

pub struct ResumeArgs {
  pub snapshot: PathBuf,
  pub check: bool,
  pub log_level: Option<String>,
}

pub async fn resume(args: ResumeArgs) -> anyhow::Result<()> {
  let bytes = std::fs::read(&args.snapshot)
    .with_context(|| format!("reading {}", args.snapshot.display()))?;
  let snapshot = snap::decode(&bytes)?;
  let kind = RunnerKind::from_name(&snapshot.runner.kind).ok_or_else(|| {
    anyhow::anyhow!("unknown runner kind {}", snapshot.runner.kind)
  })?;
  // Carried, not derived again: the lock files are named after it.
  let runner = RunnerSpec {
    kind,
    root: PathBuf::from(&snapshot.runner.root),
  };
  let paths = runner_paths(&runner)?;
  let log_level = args.log_level.as_deref();
  if args.check {
    return check(&snapshot, &runner, log_level);
  }
  // Only the process that wrote the snapshot holds what it names.
  if std::process::id() != snapshot.pid {
    anyhow::bail!("the snapshot belongs to pid {}", snapshot.pid);
  }

  // From here this process is the runner.
  let (config, _logger) = resumed_config(&runner, log_level);
  let project = Project::new(config.clone());
  print_warnings(&config.warnings);
  log::info!(
    "Resuming from dekit {} snapshot v{}",
    snapshot.source_version,
    snapshot.version
  );

  let lock_guard = LockFileGuard::adopt(
    paths,
    snapshot.lock_fd,
    snapshot.live_fd,
    snapshot.started_at,
  );
  let taken = take_over(&snapshot, &runner, &project, &lock_guard);
  let _ = std::fs::remove_file(&args.snapshot);
  let (kernel, console, server_socket, connections) = match taken {
    Ok(taken) => taken,
    Err(err) => return Err(fail(&snapshot, &lock_guard, err)),
  };

  // Taken over: nothing below fails the resume.
  let pc = kernel.context();
  let ctx = Arc::new(ServerCtx {
    pc: pc.clone(),
    project,
    connections: Connections::default(),
    runner: Some(runner_handle(
      &runner,
      &lock_guard,
      &server_socket,
      log_level,
    )),
  });
  if let Some(hook) = config.on_idle.clone() {
    let console = pc.get_task_sender(console);
    watch_idle(&pc, &config, TaskSelector::all(), hook, console);
  }
  let kernel_handle = tokio::spawn(kernel.run());
  UnixProcessesWaiter::resume();

  // Registered before any of them runs: one could ask for another upgrade
  // at once, and its freeze must reach every connection.
  let mut carried: Vec<BoxFuture<'static, ()>> = Vec::new();
  for conn in connections {
    let (sender, receiver) =
      match adopt_connection(conn.fd, &conn.input, &conn.output) {
        Ok(adopted) => adopted,
        Err(err) => {
          log::warn!("Dropping connection {}: {err}", conn.fd);
          continue;
        }
      };
    let reg = Connections::register(&ctx);
    let ctx = ctx.clone();
    let fd = Some(conn.fd);
    carried.push(match conn.kind {
      Carried::Rpc(resumed) => Box::pin(dispatch_connection(
        ctx,
        reg,
        sender,
        receiver,
        fd,
        Some(resumed),
      )),
      Carried::Attach {
        hello,
        task,
        attach,
      } => Box::pin(resume_session(
        ctx, reg, fd, hello, task, attach, sender, receiver,
      )),
    });
  }

  log::info!("Upgrade complete: now dekit {}", env!("CARGO_PKG_VERSION"));
  let result = serve(ctx, server_socket, kernel_handle, carried).await;
  // As `run_server` does: `runner status` shows why it ended.
  if let Err(err) = &result {
    lock_guard.publish_error(err);
  }
  drop(lock_guard);
  result
}

/// `--check`, run by the target binary as the runner's own child before
/// the switch: it loads what resume loads, builds what resume builds, and
/// restores the graph with tasks that never start. Nothing inherited is
/// touched.
fn check(
  snapshot: &snap::Snapshot,
  runner: &RunnerSpec,
  log_level: Option<&str>,
) -> anyhow::Result<()> {
  // A wrapper (npm's `dekit` script) would run the next image as its own
  // child, which neither inherits the runner's fds nor can reap its tasks.
  if std::os::unix::process::parent_id() != snapshot.pid {
    anyhow::bail!(
      "not run by the runner itself (pid {}); pass the dekit binary, not a wrapper",
      snapshot.pid
    );
  }
  let (config, _logger) = resumed_config(runner, log_level);
  check_graph(snapshot, &Project::new(config))?;
  println!("ok {}", snapshot.pid);
  Ok(())
}

/// The config a resume runs with, and its logging. The tasks are the
/// snapshot's, so a dekit.yaml that does not load, or log settings that
/// don't work, fall back to the defaults with a warning rather than keep
/// the runner from going on: the check finds what the resume will.
fn resumed_config(
  runner: &RunnerSpec,
  log_level: Option<&str>,
) -> (Arc<Config>, Option<crate::logging::LoggerHandle>) {
  let mut config = Config::load_dir(&runner.root).unwrap_or_else(|err| {
    let mut config = Config::make_default();
    config
      .warnings
      .push(format!("ignoring dekit.yaml: {err:#}"));
    config
  });
  config.runner = Some(runner.clone());
  let logger = match init_logging(&config, log_level, &runner.root) {
    Ok(logger) => logger,
    Err(err) => {
      config
        .warnings
        .push(format!("ignoring the log settings: {err:#}"));
      init_logging(&Config::make_default(), log_level, &runner.root)
        .unwrap_or(None)
    }
  };
  (Arc::new(config), logger)
}

fn check_graph(
  snapshot: &snap::Snapshot,
  project: &Arc<Project>,
) -> anyhow::Result<()> {
  let prepared = prepare(snapshot, project)?;
  let tasks = prepared
    .tasks
    .into_iter()
    .map(|(saved, registration)| match registration {
      Registration::Task(registration) => {
        let factory =
          Box::new(|_: TaskContext| -> Box<dyn Task> { Box::new(Unstarted) });
        let registration = TaskRegistration {
          factory,
          ..registration
        };
        (saved, Registration::Task(registration))
      }
      folder @ Registration::Folder(_) => (saved, folder),
    })
    .collect();
  Kernel::new().restore(prepared.next_task_id, tasks)
}

/// Holds a task's place in the graph `--check` restores and drops.
struct Unstarted;

impl Task for Unstarted {
  fn handle_cmd(&mut self, _cmd: TaskCmd, _fx: &mut Effects) {}
}

/// Everything a resume builds from the snapshot before it touches an
/// inherited fd; `--check` builds exactly this.
///
/// The graph is the snapshot's, every task with its saved definition
/// around its child and screen: a restart or upgrade changes nothing, and
/// a reload takes an edited `dekit.yaml`. Only a task an older runner
/// saved without a definition is built from the config, once.
struct Prepared<'a> {
  next_task_id: usize,
  /// The tasks and folders, in list order.
  tasks: Vec<(Option<&'a snap::Task>, Registration)>,
  console: TaskId,
  connections: Vec<Connection>,
}

struct Connection {
  fd: i32,
  input: Vec<u8>,
  output: Vec<u8>,
  kind: Carried,
}

enum Carried {
  Rpc(Resumed),
  Attach {
    hello: Hello,
    task: TaskId,
    attach: Attachment,
  },
}

fn prepare<'a>(
  snapshot: &'a snap::Snapshot,
  project: &Arc<Project>,
) -> anyhow::Result<Prepared<'a>> {
  let config = &project.config();
  let mut console = None;
  let mut tasks = Vec::with_capacity(snapshot.tasks.len());
  // Every task and folder as it was, in its order: the config is not
  // read for them. A reload brings in what changed there.
  for node in snapshot.listed() {
    let task = match node {
      snap::Listed::Task(task) => task,
      snap::Listed::Folder(folder) => {
        let def = folder
          .def(TaskId(folder.id))
          .with_context(|| format!("saved folder {}/", folder.path))?;
        tasks.push((None, Registration::Folder(def)));
        continue;
      }
    };
    let task_id = TaskId(task.id);
    let registration = match &task.kind {
      snap::TaskKind::Process(process) => {
        let deps = task
          .deps
          .iter()
          .map(|id| TaskSelector::Id(TaskId(*id)))
          .collect();
        // Refused whole: a task left out would leave its child behind.
        saved_task_registration(
          config,
          task_id,
          task,
          process,
          deps,
          resumed_instance(task, process),
        )
        .with_context(|| {
          let name = task.path.clone().unwrap_or(task.id.to_string());
          format!("saved task {name}")
        })?
      }
      snap::TaskKind::Console { groups } => {
        console = Some(task_id);
        let (space, path) = match task.key()? {
          Some(key) => (key.space, Some(key.path)),
          None => (TaskSpaceId::default_space(), None),
        };
        console_task_registration(
          task_id,
          TaskDef {
            space,
            path,
            label: task.label.clone(),
            tags: task.tags.clone(),
            pinned: task.pinned,
            ..TaskDef::default()
          },
          project.clone(),
          config.keymap.build(),
          groups.clone(),
        )
      }
    };
    tasks.push((Some(task), Registration::Task(registration)));
  }
  let next_task_id = snapshot.next_task_id;
  let console =
    console.ok_or_else(|| anyhow::anyhow!("snapshot has no console"))?;

  let mut connections = Vec::with_capacity(snapshot.connections.len());
  for conn in &snapshot.connections {
    let hello = conn.hello.as_ref().map(hello_from_snapshot);
    let kind = match &conn.kind {
      snap::ConnectionKind::Rpc { pending_upgrade } => Carried::Rpc(Resumed {
        hello,
        pending_upgrade: *pending_upgrade,
      }),
      snap::ConnectionKind::Attach {
        task,
        width,
        height,
        until_exit,
        caps,
        cell_pixels,
      } => Carried::Attach {
        hello: hello.ok_or_else(|| {
          anyhow::anyhow!("attach connection {} has no hello", conn.fd)
        })?,
        task: TaskId(*task),
        attach: Attachment {
          geometry: Geometry {
            size: Size {
              width: *width,
              height: *height,
            },
            cell_pixels: *cell_pixels,
          },
          until_exit: *until_exit,
          caps: *caps,
        },
      },
    };
    connections.push(Connection {
      fd: conn.fd,
      input: snap::from_base64(&conn.buffered_input)?,
      output: snap::from_base64(&conn.buffered_output)?,
      kind,
    });
  }
  Ok(Prepared {
    next_task_id,
    tasks,
    console,
    connections,
  })
}

/// Takes over what the snapshot names and restores the graph. Everything
/// that can fail a resume is here: the kernel is not running yet and
/// nothing is reaped until it returns, so on failure every task's process
/// group is still this runner's to kill.
fn take_over(
  snapshot: &snap::Snapshot,
  runner: &RunnerSpec,
  project: &Arc<Project>,
  lock_guard: &LockFileGuard,
) -> anyhow::Result<(Kernel, TaskId, ServerSocket, Vec<Connection>)> {
  let prepared = prepare(snapshot, project)?;
  project.set_base(saved_base(
    snapshot,
    prepared
      .tasks
      .iter()
      .filter_map(|(_, registration)| match registration {
        Registration::Task(registration) => Some(registration),
        Registration::Folder(_) => None,
      }),
  ));
  // Children spawned by this image must not inherit what it adopts.
  set_cloexec(&snap::fds(snapshot), true)
    .map_err(|err| anyhow::anyhow!("inherited fd is not open: {err}"))?;
  let server_socket = ServerSocket::adopt(snapshot.listener_fd)?;
  UnixProcessesWaiter::init_paused()?;
  let mut kernel = Kernel::new();
  kernel
    .save_on_quit(crate::dekit::server::save_on_quit(runner, project.clone()));
  kernel.restore(prepared.next_task_id, prepared.tasks)?;
  let warnings = project.config().warnings.clone();
  // Before the upgrade is answered: its requester reads the record next.
  lock_guard.publish(runner, &warnings)?;
  Ok((
    kernel,
    prepared.console,
    server_socket,
    prepared.connections,
  ))
}

/// This image owns the runner but cannot run it. Nothing is left running
/// without an owner, and the error goes where `runner status` and the
/// waiting `runner upgrade` look.
fn fail(
  snapshot: &snap::Snapshot,
  lock_guard: &LockFileGuard,
  err: anyhow::Error,
) -> anyhow::Error {
  let err = err.context(format!(
    "dekit {} could not resume the runner and killed its tasks",
    env!("CARGO_PKG_VERSION")
  ));
  log::error!("{err:#}");
  for task in &snapshot.tasks {
    if let snap::TaskKind::Process(process) = &task.kind
      && let Some(instance) = &process.instance
      // Collected by the previous image: its pid may be reused.
      && instance.exit.is_none()
      // 0 and 1 would signal our own group, or everything.
      && let Ok(pid @ 2..) = i32::try_from(instance.pid)
    {
      // Unreaped (see `take_over`), so the group is still ours.
      unsafe { libc::kill(-pid, libc::SIGKILL) };
    }
  }
  lock_guard.publish_error(&err);
  err
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn golden_v1_passes_the_check() {
    let snapshot = snap::decode(include_bytes!("fixtures/v1.json")).unwrap();
    check_graph(&snapshot, &Project::new(Arc::new(Config::make_default())))
      .unwrap();
  }

  /// A saved definition this binary refuses refuses the upgrade, by name.
  #[test]
  fn a_saved_task_this_binary_refuses_is_named() {
    let mut snapshot =
      snap::decode(include_bytes!("fixtures/v1.json")).unwrap();
    let web = snapshot.tasks.iter_mut().find(|t| t.id == 2).unwrap();
    web.spec =
      Some(serde_yaml::from_str("{cmd: [web], no_such_key: 1}").unwrap());
    let err =
      check_graph(&snapshot, &Project::new(Arc::new(Config::make_default())))
        .unwrap_err();
    assert!(
      format!("{err:#}").starts_with("saved task web: "),
      "{err:#}"
    );
  }
}
