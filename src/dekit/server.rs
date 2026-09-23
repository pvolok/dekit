use std::{
  collections::HashMap,
  sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU64, Ordering},
  },
};

use futures::future::BoxFuture;
use serde_json::Value;
use tokio::{
  sync::{
    mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel},
    oneshot,
  },
  task::JoinSet,
};

use crate::{
  command::{CommandError, CommandResult, execute},
  config::{
    config::Config,
    hook::{Hook, watch_idle},
  },
  console::app::console_task_registration,
  dekit::attach::attach_session,
  kernel::{
    kernel::Kernel,
    kernel_message::{
      KernelCommand, KernelQuery, KernelQueryResponse, RegisterError, SharedVt,
      TaskContext, TaskInfo, TaskSelector,
    },
    task::{TaskDef, TaskState},
    task_key::TaskSpaceId,
    task_path::TaskPath,
  },
  protocol::{
    ActResult, ConnReceiver, ConnSender, CtlMsg, RpcError, RpcRequest,
    RpcState, RpcTaskInfo, RpcWhy, RpcWhyDep, ScreenResult, TaskListResult,
    codes, ctl::Hello, ok_result, server_hello,
  },
  runner::{
    RunnerSpec,
    lockfile::{self, RunnerPaths},
    socket::{ServerSocket, bind_server_socket},
  },
  target::Target,
  task::config_tasks::register_config_tasks,
  term::Size,
  upgrade::snapshot as snap,
};

pub struct RunnerHandle {
  pub spec: RunnerSpec,
  pub paths: RunnerPaths,
  pub lock_fd: i32,
  pub live_fd: i32,
  pub listener_fd: i32,
  pub started_at: u64,
  /// The `--log-level` it was started with, passed on to the next image.
  pub log_level: Option<String>,
  pub upgrading: AtomicBool,
}

/// What every served connection shares.
pub struct ServerCtx {
  pub pc: TaskContext,
  pub config: Arc<Config>,
  pub connections: Connections,
  /// None when serving in-process (tests): no upgrade possible.
  pub runner: Option<RunnerHandle>,
}

impl ServerCtx {
  /// Serving in-process with no runner identity (tests, legacy `--ctl`).
  pub fn local(pc: TaskContext, config: Arc<Config>) -> Arc<Self> {
    Arc::new(ServerCtx {
      pc,
      config,
      connections: Connections::default(),
      runner: None,
    })
  }
}

pub enum ConnCtl {
  /// Stop reading and writing, answer with the connection's snapshot
  /// (None: it cannot be carried across), then wait for `Thaw` or the
  /// exec. Answered at once: a connection never waits on its client
  /// outside its select loop.
  Freeze(oneshot::Sender<Option<snap::Connection>>),
  Thaw,
}

/// Paused while connections are frozen, so none starts unfrozen; new
/// clients wait in the listener backlog.
enum AcceptCtl {
  /// Acknowledged once every connection accepted so far is registered.
  Pause(oneshot::Sender<()>),
  Resume,
}

/// Every live connection, so an upgrade can freeze them all.
#[derive(Default)]
pub struct Connections {
  inner: Mutex<HashMap<u64, UnboundedSender<ConnCtl>>>,
  next: AtomicU64,
  /// The accept loop, while one runs.
  accept: Mutex<Option<UnboundedSender<AcceptCtl>>>,
}

pub struct ConnReg {
  pub ctl: UnboundedReceiver<ConnCtl>,
  id: u64,
  ctx: Arc<ServerCtx>,
}

impl Drop for ConnReg {
  fn drop(&mut self) {
    if let Ok(mut map) = self.ctx.connections.inner.lock() {
      map.remove(&self.id);
    }
  }
}

impl Connections {
  pub fn register(ctx: &Arc<ServerCtx>) -> ConnReg {
    let (tx, rx) = unbounded_channel();
    let id = ctx.connections.next.fetch_add(1, Ordering::Relaxed);
    if let Ok(mut map) = ctx.connections.inner.lock() {
      map.insert(id, tx);
    }
    ConnReg {
      ctl: rx,
      id,
      ctx: ctx.clone(),
    }
  }

  /// Pauses accepting, then asks every connection to freeze.
  pub async fn freeze_all(
    &self,
  ) -> Vec<oneshot::Receiver<Option<snap::Connection>>> {
    let accept = self.accept.lock().ok().and_then(|accept| accept.clone());
    if let Some(accept) = accept {
      let (tx, rx) = oneshot::channel();
      if accept.send(AcceptCtl::Pause(tx)).is_ok() {
        let _ = rx.await;
      }
    }
    let mut replies = Vec::new();
    if let Ok(map) = self.inner.lock() {
      for sender in map.values() {
        let (tx, rx) = oneshot::channel();
        if sender.send(ConnCtl::Freeze(tx)).is_ok() {
          replies.push(rx);
        }
      }
    }
    replies
  }

  pub fn thaw_all(&self) {
    if let Ok(map) = self.inner.lock() {
      for sender in map.values() {
        let _ = sender.send(ConnCtl::Thaw);
      }
    }
    if let Ok(accept) = self.accept.lock()
      && let Some(accept) = accept.as_ref()
    {
      let _ = accept.send(AcceptCtl::Resume);
    }
  }
}

pub fn snapshot_hello(hello: &Hello) -> snap::Hello {
  snap::Hello {
    protocol: hello.protocol,
    version: hello.version.clone(),
    app: hello.app.clone(),
    features: hello.features.clone(),
  }
}

pub fn hello_from_snapshot(hello: &snap::Hello) -> Hello {
  Hello {
    protocol: hello.protocol,
    version: hello.version.clone(),
    app: hello.app.clone(),
    features: hello.features.clone(),
  }
}

/// What a frozen connection hands the snapshot: nothing, when it has no
/// fd to inherit.
pub fn carried(
  fd: Option<i32>,
  hello: Option<&Hello>,
  buffered_input: &[u8],
  sender: &ConnSender,
  kind: snap::ConnectionKind,
) -> Option<snap::Connection> {
  Some(snap::Connection {
    fd: fd?,
    hello: hello.map(snapshot_hello),
    buffered_input: snap::to_base64(buffered_input),
    buffered_output: snap::to_base64(sender.pending()),
    kind,
  })
}

/// A connection inherited across an upgrade.
pub struct Resumed {
  /// None: frozen before the client's hello arrived.
  pub hello: Option<Hello>,
  pub pending_upgrade: Option<u64>,
}

pub async fn run_server(
  runner: RunnerSpec,
  log_level: Option<&str>,
) -> anyhow::Result<()> {
  let lock_guard = lockfile::create_lock_file(&runner)?;
  let result = run_locked(&runner, log_level, &lock_guard).await;
  if let Err(error) = &result {
    lock_guard.publish_error(error);
  }
  result
}

async fn run_locked(
  runner: &RunnerSpec,
  log_level: Option<&str>,
  lock_guard: &lockfile::LockFileGuard,
) -> anyhow::Result<()> {
  let working_dir = runner.root.clone();
  let mut config = Config::load_dir(&working_dir)?;
  config.runner = Some(runner.clone());
  let keymap = config.keymap.build();
  let config = Arc::new(config);

  let _logger = init_logging(&config, log_level, &working_dir)?;

  // Visible when running in the foreground; detached runners surface
  // these through the published record instead.
  crate::dekit::main::print_warnings(&config.warnings);

  log::info!("Lock file created for directory: {}", working_dir.display());

  #[cfg(unix)]
  crate::process::unix_processes_waiter::UnixProcessesWaiter::init()?;
  let mut kernel = Kernel::new();
  let pc = kernel.context();
  let socket_path = lock_guard.socket_path().to_path_buf();
  let console_id = pc.alloc_id();
  let console = console_task_registration(
    console_id,
    TaskDef {
      space: TaskSpaceId::dekit(),
      path: Some(TaskPath::new("console").expect("valid console path")),
      pinned: true,
      ..TaskDef::default()
    },
    config.clone(),
    keymap,
  );
  if let Err(err) = kernel.register_task_registration(console) {
    #[cfg(unix)]
    crate::process::unix_processes_waiter::UnixProcessesWaiter::uninit()?;
    anyhow::bail!("Failed to register console task: {err}")
  }
  let console = pc.get_task_sender(console_id);
  let kernel_handle = tokio::spawn(kernel.run());

  // Watch before any task exists so the first start→exit fires the hook.
  if let Some(hook) = config.on_idle.clone() {
    watch_idle(&pc, &config, TaskSelector::all(), hook, console);
  }

  let bootstrap = async {
    register_config_tasks(&config, &pc).await?;
    if let Some(hook) = &config.on_init {
      let Hook::Command(command) = hook else {
        anyhow::bail!("dekit on_init hook is not a command")
      };
      execute(&pc, &config, command).await?;
    }
    let socket = bind_server_socket(&socket_path).await?;
    lock_guard.publish(runner, &config.warnings)?;
    log::info!("Server is listening.");
    anyhow::Ok(socket)
  }
  .await;
  let server_socket = match bootstrap {
    Ok(socket) => socket,
    Err(err) => {
      pc.send(KernelCommand::Quit);
      let _ = kernel_handle.await;
      #[cfg(unix)]
      crate::process::unix_processes_waiter::UnixProcessesWaiter::uninit()?;
      return Err(err);
    }
  };

  let ctx = Arc::new(ServerCtx {
    pc,
    config,
    connections: Connections::default(),
    runner: Some(runner_handle(runner, lock_guard, &server_socket, log_level)),
  });
  serve(ctx, server_socket, kernel_handle, Vec::new()).await
}

pub fn init_logging(
  config: &Config,
  log_level: Option<&str>,
  working_dir: &std::path::Path,
) -> anyhow::Result<Option<crate::logging::LoggerHandle>> {
  crate::logging::init(crate::logging::Config {
    binary: "dekit",
    cli_level: log_level,
    log_env: "DEKIT_LOG",
    file_env: "DEKIT_LOG_FILE",
    config_level: config.log.level.as_deref(),
    config_file: config.log.file.as_deref(),
    default_dir: Some(working_dir),
  })
}

pub fn runner_handle(
  runner: &RunnerSpec,
  lock_guard: &lockfile::LockFileGuard,
  socket: &ServerSocket,
  log_level: Option<&str>,
) -> RunnerHandle {
  #[cfg(unix)]
  let (lock_fd, live_fd) = lock_guard.fds();
  #[cfg(not(unix))]
  let (lock_fd, live_fd) = (-1, -1);
  RunnerHandle {
    spec: runner.clone(),
    paths: lock_guard.paths().clone(),
    lock_fd,
    live_fd,
    listener_fd: socket.raw_fd().unwrap_or(-1),
    started_at: lock_guard.started_at(),
    log_level: log_level.map(str::to_string),
    upgrading: AtomicBool::new(false),
  }
}

/// Accepts clients until the kernel stops. `carried` are the connections
/// an upgrade carried across, already registered: they start once the
/// acceptor can be paused, so a freeze one of them asks for at once still
/// holds new clients back.
pub async fn serve(
  ctx: Arc<ServerCtx>,
  mut server_socket: ServerSocket,
  kernel_handle: tokio::task::JoinHandle<()>,
  carried: Vec<BoxFuture<'static, ()>>,
) -> anyhow::Result<()> {
  let (accept_tx, mut accept_rx) = unbounded_channel();
  if let Ok(mut accept) = ctx.connections.accept.lock() {
    *accept = Some(accept_tx);
  }
  let accept_ctx = ctx.clone();
  tokio::spawn(async move {
    log::debug!("Waiting for clients...");
    let mut paused = false;
    loop {
      tokio::select! {
        accepted = server_socket.accept(), if !paused => match accepted {
          Ok(accepted) => {
            // Registered before its task runs, so a freeze from here on
            // reaches it.
            let reg = Connections::register(&accept_ctx);
            tokio::spawn(dispatch_connection(
              accept_ctx.clone(),
              reg,
              accepted.sender,
              accepted.receiver,
              accepted.fd,
              None,
            ));
          }
          Err(err) => {
            log::debug!("Server socket accept error: {}", err);
            break;
          }
        },
        ctl = accept_rx.recv() => match ctl {
          Some(AcceptCtl::Pause(ack)) => {
            paused = true;
            let _ = ack.send(());
          }
          Some(AcceptCtl::Resume) => paused = false,
          None => break,
        },
      }
    }
  });
  for connection in carried {
    tokio::spawn(connection);
  }

  kernel_handle.await?;

  #[cfg(unix)]
  crate::process::unix_processes_waiter::UnixProcessesWaiter::uninit()?;

  Ok(())
}

/// Serves a registered connection: the hello exchange, then any number
/// of concurrent requests answered as they finish, until the client hangs
/// up or an `attach` takes the connection over. Nothing waits on the
/// client outside the select, so a freeze is answered at any point, the
/// hello exchange included: what was read but not handled and what was
/// queued but not written travel in the snapshot. A resumed connection
/// first answers the upgrade it was waiting on.
pub async fn dispatch_connection(
  ctx: Arc<ServerCtx>,
  mut reg: ConnReg,
  mut sender: ConnSender,
  mut receiver: ConnReceiver,
  fd: Option<i32>,
  resumed: Option<Resumed>,
) {
  let mut hello = None;
  if let Some(resumed) = resumed {
    hello = resumed.hello;
    if let Some(id) = resumed.pending_upgrade
      && sender.queue_ctl(CtlMsg::ok(id, upgrade_result())).is_err()
    {
      return;
    }
  }

  let mut replies: JoinSet<CtlMsg> = JoinSet::new();
  let mut frozen = false;
  // The client left or was refused: only what is owed remains.
  let mut closing = false;
  // An `upgrade` in flight: its request id and where its failure lands.
  let mut upgrade: Option<(u64, oneshot::Receiver<RpcError>)> = None;
  while !(closing && replies.is_empty()) {
    let upgrade_failed = async {
      match upgrade.as_mut() {
        Some((_, rx)) => rx.await,
        None => std::future::pending().await,
      }
    };
    tokio::select! {
      // Nothing is read while output is owed: a client that stops
      // reading is not read either.
      msg = receiver.recv_ctl(), if !frozen && !closing && sender.pending().is_empty() => {
        let msg = match msg {
          Ok(msg) => msg,
          Err(err) => {
            log::debug!("Client connection closed: {err}");
            closing = true;
            continue;
          }
        };
        if hello.is_none() {
          match msg {
            CtlMsg::Hello(client) => match server_hello(&client) {
              Ok(ours) => {
                if sender.queue_ctl(CtlMsg::Hello(ours)).is_err() {
                  return;
                }
                hello = Some(client);
              }
              Err(bye) => {
                log::debug!("Refusing client: {}", bye.message);
                if sender.queue_ctl(CtlMsg::Bye(bye)).is_err() {
                  return;
                }
                closing = true;
              }
            },
            msg => {
              log::debug!("Expected hello from client, got {msg:?}");
              closing = true;
            }
          }
          continue;
        }
        let request = match msg {
          CtlMsg::Request(request) => request,
          msg => {
            log::debug!("Ignoring client message {msg:?}");
            continue;
          }
        };
        match RpcRequest::from_wire(&request.method, request.params) {
          Ok(RpcRequest::Attach {
            target,
            width,
            height,
            until_exit,
          }) => {
            // Earlier requests are answered before the screen stream
            // starts; handlers never wait on the client.
            while let Some(reply) = replies.join_next().await {
              if let Ok(reply) = reply && sender.queue_ctl(reply).is_err() {
                return;
              }
            }
            let Some(client) = hello else {
              return;
            };
            attach_session(
              &ctx,
              reg,
              fd,
              &client,
              request.id,
              target,
              Size { width, height },
              until_exit,
              sender,
              receiver,
            )
            .await;
            return;
          }
          Ok(RpcRequest::Upgrade { binary }) => {
            if upgrade.is_some() {
              let error = RpcError::new(codes::BUSY, "an upgrade is in progress");
              if sender.queue_ctl(CtlMsg::err(request.id, error)).is_err() {
                return;
              }
              continue;
            }
            let (tx, rx) = oneshot::channel();
            upgrade = Some((request.id, rx));
            let ctx = ctx.clone();
            tokio::spawn(async move {
              let error = crate::upgrade::upgrade(ctx, binary).await;
              let _ = tx.send(error);
            });
          }
          Ok(req) => {
            let (ctx, id) = (ctx.clone(), request.id);
            replies.spawn(async move {
              match handle_rpc(&ctx.pc, &ctx.config, req).await {
                Ok(result) => CtlMsg::ok(id, result),
                Err(error) => CtlMsg::err(id, error),
              }
            });
          }
          Err(error) => {
            if sender.queue_ctl(CtlMsg::err(request.id, error)).is_err() {
              return;
            }
          }
        }
      }
      Some(reply) = replies.join_next(), if !replies.is_empty() => {
        if let Ok(reply) = reply && sender.queue_ctl(reply).is_err() {
          return;
        }
      }
      error = upgrade_failed, if !frozen => {
        let (id, _) = upgrade.take().expect("upgrade in flight");
        let error = error.unwrap_or_else(|_| RpcError::internal("upgrade task ended"));
        if sender.queue_ctl(CtlMsg::err(id, error)).is_err() {
          return;
        }
      }
      written = sender.flush(), if !frozen && !sender.pending().is_empty() => {
        if let Err(err) = written {
          log::debug!("Client connection closed: {err}");
          return;
        }
      }
      ctl = reg.ctl.recv() => match ctl {
        Some(ConnCtl::Freeze(reply)) => {
          // Handlers never wait on the client, so this ends promptly.
          while let Some(done) = replies.join_next().await {
            if let Ok(done) = done && sender.queue_ctl(done).is_err() {
              return;
            }
          }
          frozen = true;
          let kind = snap::ConnectionKind::Rpc {
            pending_upgrade: upgrade.as_ref().map(|(id, _)| *id),
          };
          let _ = reply.send(carried(fd, hello.as_ref(), receiver.buffered(), &sender, kind));
        }
        Some(ConnCtl::Thaw) => frozen = false,
        None => break,
      }
    }
  }
  finish(
    &mut sender,
    &mut reg.ctl,
    fd,
    hello.as_ref(),
    receiver.buffered(),
  )
  .await;
}

/// Writes what a connection still owes before it ends, still answering
/// a freeze: the rest is then carried and written by the next image.
pub async fn finish(
  sender: &mut ConnSender,
  ctl: &mut UnboundedReceiver<ConnCtl>,
  fd: Option<i32>,
  hello: Option<&Hello>,
  buffered_input: &[u8],
) {
  let mut frozen = false;
  while !sender.pending().is_empty() {
    tokio::select! {
      written = sender.flush(), if !frozen => {
        if written.is_err() {
          return;
        }
      }
      msg = ctl.recv() => match msg {
        Some(ConnCtl::Freeze(reply)) => {
          frozen = true;
          let kind = snap::ConnectionKind::Rpc { pending_upgrade: None };
          let _ = reply.send(carried(fd, hello, buffered_input, sender, kind));
        }
        Some(ConnCtl::Thaw) => frozen = false,
        None => return,
      },
    }
  }
}

pub fn upgrade_result() -> Value {
  serde_json::json!({ "version": env!("CARGO_PKG_VERSION") })
}

pub(crate) fn task_state(state: TaskState) -> RpcState {
  let (token, info) = match state {
    TaskState::Idle => ("idle", None),
    TaskState::Starting => ("starting", None),
    TaskState::Running => ("running", None),
    TaskState::Ready => ("ready", None),
    TaskState::Stopping => ("stopping", None),
    TaskState::Backoff => ("backoff", None),
    TaskState::Done(info) => ("done", Some(info)),
    TaskState::Exited(info) => ("exited", Some(info)),
  };
  RpcState {
    state: token.to_string(),
    exit_code: info.and_then(|i| i.code),
    signal: info.and_then(|i| i.signal),
  }
}

fn bad_target(err: impl ToString) -> RpcError {
  RpcError::new(codes::BAD_TARGET, err.to_string())
}

async fn list(
  pc: &TaskContext,
  target: &Target,
) -> Result<Vec<TaskInfo>, RpcError> {
  let selector = target.selector().map_err(bad_target)?;
  match pc.query(KernelQuery::ListTasks(selector)).await {
    Ok(KernelQueryResponse::TaskList(tasks)) => Ok(tasks),
    Ok(KernelQueryResponse::Explain(_)) | Err(_) => {
      Err(RpcError::internal("unexpected query response"))
    }
  }
}

/// The single match a one-task request needs.
fn one<T>(matches: Vec<T>, target: &Target) -> Result<T, RpcError> {
  let mut matches = matches.into_iter();
  match (matches.next(), matches.next()) {
    (Some(task), None) => Ok(task),
    (None, _) => Err(RpcError::new(
      codes::NO_MATCH,
      format!("no task matches '{}'", target),
    )),
    (Some(_), Some(_)) => Err(RpcError::new(
      codes::AMBIGUOUS,
      format!("'{}' matches more than one task", target),
    )),
  }
}

/// Exactly one task must match, and it must have a screen.
pub async fn resolve_screen(
  pc: &TaskContext,
  target: &Target,
) -> Result<(TaskInfo, SharedVt), RpcError> {
  let task = one(list(pc, target).await?, target)?;
  match task.vt.clone() {
    Some(vt) => Ok((task, vt)),
    None => Err(RpcError::new(
      codes::NO_SCREEN,
      format!("'{}' has no screen", task.name()),
    )),
  }
}

async fn handle_rpc(
  pc: &TaskContext,
  config: &Config,
  req: RpcRequest,
) -> Result<Value, RpcError> {
  match req {
    RpcRequest::Attach { .. } | RpcRequest::Upgrade { .. } => {
      Err(RpcError::internal("handled by the connection loop"))
    }

    RpcRequest::Command(command) => {
      let result = execute(pc, config, &command).await.map_err(|err| {
        let code = match &err {
          CommandError::InvalidTarget(_) => codes::BAD_TARGET,
          CommandError::InvalidCommand(_) => codes::INVALID_PARAMS,
          CommandError::Register(RegisterError::MissingDep(_)) => {
            codes::NO_MATCH
          }
          CommandError::Register(RegisterError::PathTaken(_)) => {
            codes::PATH_TAKEN
          }
          CommandError::Register(RegisterError::ReservedSpace(_)) => {
            codes::BAD_TARGET
          }
          CommandError::Register(RegisterError::IdTaken)
          | CommandError::KernelClosed => codes::INTERNAL,
        };
        RpcError::new(code, err.to_string())
      })?;
      match result {
        CommandResult::Matched(matched) => {
          serde_json::to_value(ActResult { matched })
            .map_err(RpcError::internal)
        }
        CommandResult::None => Ok(ok_result()),
      }
    }

    RpcRequest::Ls { target } => {
      let target = target.unwrap_or_else(|| Target::glob("**"));
      let tasks = list(pc, &target)
        .await?
        .into_iter()
        .map(|t| RpcTaskInfo {
          id: t.id,
          path: t.name(),
          label: t.label,
          state: task_state(t.state),
        })
        .collect();
      serde_json::to_value(TaskListResult { tasks }).map_err(RpcError::internal)
    }

    RpcRequest::Why { target } => {
      let selector = target.selector().map_err(bad_target)?;
      let explains = match pc.query(KernelQuery::Explain(selector)).await {
        Ok(KernelQueryResponse::Explain(explains)) => explains,
        Ok(KernelQueryResponse::TaskList(_)) | Err(_) => {
          return Err(RpcError::internal("unexpected query response"));
        }
      };
      let explain = one(explains, &target)?;
      let why = RpcWhy {
        id: explain.id,
        path: explain.name,
        state: task_state(explain.state),
        wanted: explain.wanted,
        supported: explain.supported,
        vetoed: explain.vetoed,
        pinned: explain.pinned,
        required_by: explain.required_by,
        deps: explain
          .deps
          .into_iter()
          .map(|d| RpcWhyDep {
            path: d.name,
            state: task_state(d.state),
            wanted: d.wanted,
            satisfied: d.satisfied,
          })
          .collect(),
        attempts: explain.attempts,
      };
      serde_json::to_value(why).map_err(RpcError::internal)
    }

    RpcRequest::Screen { target } => {
      let (_, vt) = resolve_screen(pc, &target).await?;
      let screen = vt
        .read()
        .map(|screen| crate::term::ansi::render_screen_ansi(&screen))
        .map_err(|_| RpcError::internal("screen lock poisoned"))?;
      serde_json::to_value(ScreenResult { screen }).map_err(RpcError::internal)
    }
  }
}

#[cfg(test)]
mod tests {
  use std::{sync::Arc, time::Duration};

  use tokio::{io::duplex, time::timeout};

  use super::*;
  use crate::{
    kernel::kernel::Kernel,
    protocol::{Request, client_handshake},
  };

  #[tokio::test]
  async fn answers_concurrent_requests_by_id() {
    let config = Arc::new(Config::make_default());
    let kernel = Kernel::new();
    let pc = kernel.context();
    let kernel_handle = tokio::spawn(kernel.run());

    let ctx = ServerCtx::local(pc.clone(), config);
    let (mut sender, mut receiver, connection) = serve_duplex(&ctx, None);
    let hello = client_handshake(&mut sender, &mut receiver).await.unwrap();
    assert_eq!(hello.version, env!("CARGO_PKG_VERSION"));

    let requests = [
      (7, RpcRequest::Ls { target: None }),
      (
        8,
        RpcRequest::Why {
          target: Target::glob("nope"),
        },
      ),
      (9, RpcRequest::Ls { target: None }),
    ];
    for (id, request) in requests {
      let (method, params) = request.to_wire();
      sender
        .send_ctl(CtlMsg::Request(Request { id, method, params }))
        .await
        .unwrap();
    }
    let mut seen = Vec::new();
    for _ in 0..3 {
      match timeout(Duration::from_secs(2), receiver.recv_ctl())
        .await
        .unwrap()
        .unwrap()
      {
        CtlMsg::Response(response) => {
          match response.id {
            8 => assert_eq!(response.error.unwrap().code, codes::NO_MATCH),
            _ => assert!(response.error.is_none()),
          }
          seen.push(response.id);
        }
        msg => panic!("unexpected {msg:?}"),
      }
    }
    seen.sort();
    assert_eq!(seen, vec![7, 8, 9]);

    drop(sender);
    drop(receiver);
    timeout(Duration::from_secs(2), connection)
      .await
      .unwrap()
      .unwrap();
    pc.send(KernelCommand::Quit);
    timeout(Duration::from_secs(2), kernel_handle)
      .await
      .unwrap()
      .unwrap();
  }

  fn serve_duplex(
    ctx: &Arc<ServerCtx>,
    resumed: Option<Resumed>,
  ) -> (ConnSender, ConnReceiver, tokio::task::JoinHandle<()>) {
    let (client, server) = duplex(64 * 1024);
    let (client_read, client_write) = tokio::io::split(client);
    let (server_read, server_write) = tokio::io::split(server);
    let connection = tokio::spawn(dispatch_connection(
      ctx.clone(),
      Connections::register(ctx),
      ConnSender::new(server_write),
      ConnReceiver::new(server_read),
      // Stands in for a socket fd so the connection can be carried.
      Some(7),
      resumed,
    ));
    (
      ConnSender::new(client_write),
      ConnReceiver::new(client_read),
      connection,
    )
  }

  #[tokio::test]
  async fn a_connection_freezes_before_its_hello_and_resumes() {
    let kernel = Kernel::new();
    let pc = kernel.context();
    let kernel_handle = tokio::spawn(kernel.run());
    let ctx = ServerCtx::local(pc.clone(), Arc::new(Config::make_default()));

    // Connected, but no hello yet: the freeze must not wait for one.
    let (_sender, _receiver, connection) = serve_duplex(&ctx, None);
    let reply = ctx.connections.freeze_all().await.pop().unwrap();
    let frozen = timeout(Duration::from_secs(2), reply)
      .await
      .expect("froze in time")
      .unwrap()
      .expect("carried");
    assert_eq!(frozen.hello, None);
    connection.abort();

    // The next image answers the hello when it comes.
    let resumed = Resumed {
      hello: None,
      pending_upgrade: None,
    };
    let (mut sender, mut receiver, _connection) =
      serve_duplex(&ctx, Some(resumed));
    client_handshake(&mut sender, &mut receiver).await.unwrap();

    pc.send(KernelCommand::Quit);
    timeout(Duration::from_secs(2), kernel_handle)
      .await
      .unwrap()
      .unwrap();
  }

  #[tokio::test]
  async fn another_protocol_gets_a_bye() {
    let kernel = Kernel::new();
    let pc = kernel.context();
    let kernel_handle = tokio::spawn(kernel.run());
    let ctx = ServerCtx::local(pc.clone(), Arc::new(Config::make_default()));

    let (mut sender, mut receiver, connection) = serve_duplex(&ctx, None);
    sender
      .send_ctl(CtlMsg::Hello(Hello {
        protocol: 999,
        version: "99.0.0".to_string(),
        app: "dekit future".to_string(),
        features: vec![],
      }))
      .await
      .unwrap();
    match receiver.recv_ctl().await.unwrap() {
      CtlMsg::Bye(bye) => assert_eq!(bye.code, codes::UNSUPPORTED_PROTOCOL),
      msg => panic!("expected bye, got {msg:?}"),
    }
    // The runner hangs up after the bye.
    timeout(Duration::from_secs(2), connection)
      .await
      .unwrap()
      .unwrap();

    pc.send(KernelCommand::Quit);
    timeout(Duration::from_secs(2), kernel_handle)
      .await
      .unwrap()
      .unwrap();
  }

  #[cfg(unix)]
  #[tokio::test]
  async fn a_client_arriving_during_a_freeze_is_served_after_it() {
    let dir =
      std::env::temp_dir().join(format!("dk-accept-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("s.sock");
    let socket = bind_server_socket(&path).await.unwrap();
    let kernel = Kernel::new();
    let pc = kernel.context();
    let kernel_handle = tokio::spawn(kernel.run());
    let ctx = ServerCtx::local(pc.clone(), Arc::new(Config::make_default()));
    let server =
      tokio::spawn(serve(ctx.clone(), socket, kernel_handle, Vec::new()));
    let path = path.to_str().unwrap();

    // A first client proves the accept loop runs.
    let (mut sender, mut receiver) =
      crate::runner::socket::connect_socket(path).await.unwrap();
    client_handshake(&mut sender, &mut receiver).await.unwrap();

    let replies = ctx.connections.freeze_all().await;
    assert_eq!(replies.len(), 1);
    // Waits in the backlog while connections are frozen.
    let (mut late_sender, mut late_receiver) =
      crate::runner::socket::connect_socket(path).await.unwrap();
    ctx.connections.thaw_all();
    timeout(
      Duration::from_secs(2),
      client_handshake(&mut late_sender, &mut late_receiver),
    )
    .await
    .expect("served after the thaw")
    .unwrap();

    pc.send(KernelCommand::Quit);
    let _ = timeout(Duration::from_secs(2), server).await;
    let _ = std::fs::remove_dir_all(&dir);
  }
}
