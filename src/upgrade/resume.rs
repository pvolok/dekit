//! `dekit runner resume --snapshot PATH`: the entry point a new image
//! starts from after an exec, and with `--check` the test the runner puts
//! a target binary through first. The arguments, the check's answer, and
//! the snapshot header are frozen.

use std::{
  collections::{HashMap, HashSet},
  path::PathBuf,
  sync::Arc,
};

use anyhow::Context;
use futures::future::BoxFuture;

use crate::{
  config::{config::Config, hook::watch_idle},
  console::app::console_task_registration,
  dekit::{
    attach::resume_session,
    main::print_warnings,
    server::{
      Connections, Resumed, ServerCtx, dispatch_connection,
      hello_from_snapshot, init_logging, runner_handle, serve,
    },
  },
  kernel::{
    kernel::Kernel,
    kernel_message::{TaskContext, TaskRegistration, TaskSelector},
    task::{Effects, Task, TaskCmd, TaskDef, TaskId},
    task_key::{TaskKey, TaskSpaceId},
    task_path::TaskPath,
  },
  process::unix_processes_waiter::UnixProcessesWaiter,
  protocol::ctl::Hello,
  runner::{
    RunnerKind, RunnerSpec,
    lockfile::{LockFileGuard, runner_paths},
    socket::{ServerSocket, adopt_connection},
  },
  task::{
    config_tasks::{
      config_task_registration, config_task_resumed, resolve_task_deps,
    },
    process_task::process_task_from_snapshot,
  },
  term::Size,
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

  // From here this process is the runner. The check loaded the config and
  // the log settings moments ago; an edit since then falls back to the
  // defaults rather than take the tasks down.
  let mut config = Config::load_dir(&runner.root).unwrap_or_else(|err| {
    let mut config = Config::make_default();
    config
      .warnings
      .push(format!("ignoring dekit.yaml: {err:#}"));
    config
  });
  config.runner = Some(runner.clone());
  let _logger = match init_logging(&config, log_level, &runner.root) {
    Ok(logger) => logger,
    Err(err) => {
      config
        .warnings
        .push(format!("ignoring the log settings: {err:#}"));
      init_logging(&Config::make_default(), log_level, &runner.root)
        .unwrap_or(None)
    }
  };
  let config = Arc::new(config);
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
  let taken = take_over(&snapshot, &runner, &config, &lock_guard);
  let _ = std::fs::remove_file(&args.snapshot);
  let (kernel, console, server_socket, connections) = match taken {
    Ok(taken) => taken,
    Err(err) => return Err(fail(&snapshot, &lock_guard, err)),
  };

  // Taken over: nothing below fails the resume.
  let pc = kernel.context();
  let ctx = Arc::new(ServerCtx {
    pc: pc.clone(),
    config: config.clone(),
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
        size,
        until_exit,
      } => Box::pin(resume_session(
        ctx, reg, fd, hello, task, size, until_exit, sender, receiver,
      )),
    });
  }

  log::info!("Upgrade complete: now dekit {}", env!("CARGO_PKG_VERSION"));
  let result = serve(ctx, server_socket, kernel_handle, carried).await;
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
  let mut config = Config::load_dir(&runner.root)?;
  config.runner = Some(runner.clone());
  let config = Arc::new(config);
  let _logger = init_logging(&config, log_level, &runner.root)?;
  check_graph(snapshot, &config)?;
  println!("ok {}", snapshot.pid);
  Ok(())
}

fn check_graph(
  snapshot: &snap::Snapshot,
  config: &Arc<Config>,
) -> anyhow::Result<()> {
  let prepared = prepare(snapshot, config)?;
  let tasks = prepared
    .tasks
    .into_iter()
    .map(|(saved, registration)| {
      let factory =
        Box::new(|_: TaskContext| -> Box<dyn Task> { Box::new(Unstarted) });
      (
        saved,
        TaskRegistration {
          factory,
          ..registration
        },
      )
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
/// The graph is the snapshot's reconciled with the config just loaded: a
/// config task the snapshot also has is built from the config's spec
/// around the saved child and screen, a config task the snapshot lacks
/// is added idle, and a saved task the config lacks stays as saved. So a
/// restart or upgrade applies an edited `dekit.yaml` without touching a
/// running child; its new command is used at the next start.
struct Prepared<'a> {
  next_task_id: usize,
  tasks: Vec<(Option<&'a snap::Task>, TaskRegistration)>,
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
    size: Size,
    until_exit: bool,
  },
}

fn prepare<'a>(
  snapshot: &'a snap::Snapshot,
  config: &Arc<Config>,
) -> anyhow::Result<Prepared<'a>> {
  let mut tasks = Vec::with_capacity(snapshot.tasks.len());
  let mut console = None;

  // Config tasks first: a saved one keeps its id, a new one takes the
  // next, so the config's own dependencies resolve among them.
  let saved_by_path: HashMap<&str, &snap::Task> = snapshot
    .tasks
    .iter()
    .filter(|task| task.space.is_empty())
    .filter_map(|task| Some((task.path.as_deref()?, task)))
    .collect();
  let mut next_task_id = snapshot.next_task_id;
  let ids: Vec<TaskId> = config
    .tasks
    .iter()
    .map(|cfg| match saved_by_path.get(cfg.path.as_str()) {
      Some(saved) => TaskId(saved.id),
      None => {
        next_task_id += 1;
        TaskId(next_task_id - 1)
      }
    })
    .collect();
  let deps_by_task = resolve_task_deps(&config.tasks, &ids)?;
  let mut from_config = HashSet::new();
  for (i, cfg) in config.tasks.iter().enumerate() {
    let deps = deps_by_task[i]
      .iter()
      .copied()
      .map(TaskSelector::Id)
      .collect();
    let saved = saved_by_path.get(cfg.path.as_str()).copied();
    let registration = match saved {
      Some(saved) => {
        let snap::TaskKind::Process(process) = &saved.kind else {
          anyhow::bail!("task {} is not a process task", cfg.path);
        };
        from_config.insert(saved.id);
        config_task_resumed(
          config,
          cfg.clone(),
          ids[i],
          deps,
          saved.pinned,
          &process.screen,
          process.instance.clone(),
        )?
      }
      None => config_task_registration(
        config,
        TaskSpaceId::default_space(),
        cfg.clone(),
        ids[i],
        deps,
        cfg.autostart(),
      ),
    };
    tasks.push((saved, registration));
  }

  for task in &snapshot.tasks {
    if from_config.contains(&task.id) {
      continue;
    }
    let space = if task.space.is_empty() {
      TaskSpaceId::default_space()
    } else {
      TaskSpaceId::new(task.space.clone())
        .map_err(|err| anyhow::anyhow!("task {}: {err}", task.id))?
    };
    let path = task
      .path
      .as_deref()
      .map(TaskPath::new)
      .transpose()
      .map_err(|err| anyhow::anyhow!("task {}: {err}", task.id))?;
    let task_id = TaskId(task.id);
    let registration = match &task.kind {
      snap::TaskKind::Process(process) => process_task_from_snapshot(
        task_id,
        path.map(|path| TaskKey::new(space, path)),
        task,
        process,
      )?,
      snap::TaskKind::Console {} => {
        console = Some(task_id);
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
          config.clone(),
          config.keymap.build(),
        )
      }
    };
    tasks.push((Some(task), registration));
  }
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
      } => Carried::Attach {
        hello: hello.ok_or_else(|| {
          anyhow::anyhow!("attach connection {} has no hello", conn.fd)
        })?,
        task: TaskId(*task),
        size: Size {
          width: *width,
          height: *height,
        },
        until_exit: *until_exit,
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
  config: &Arc<Config>,
  lock_guard: &LockFileGuard,
) -> anyhow::Result<(Kernel, TaskId, ServerSocket, Vec<Connection>)> {
  let prepared = prepare(snapshot, config)?;
  // Children spawned by this image must not inherit what it adopts.
  set_cloexec(&snap::fds(snapshot), true)
    .map_err(|err| anyhow::anyhow!("inherited fd is not open: {err}"))?;
  let server_socket = ServerSocket::adopt(snapshot.listener_fd)?;
  UnixProcessesWaiter::init_paused()?;
  let mut kernel = Kernel::new();
  kernel.restore(prepared.next_task_id, prepared.tasks)?;
  // Before the upgrade is answered: its requester reads the record next.
  lock_guard.publish(runner, &config.warnings)?;
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
    check_graph(&snapshot, &Arc::new(Config::make_default())).unwrap();
  }
}
