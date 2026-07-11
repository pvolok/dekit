use std::path::PathBuf;

use serde_json::Value;

use crate::{
  config::config::TASK_ROOT,
  console::{
    app::create_app_task, app_client::client_session, server_message::ClientId,
  },
  daemon::{lockfile, socket::bind_server_socket},
  kernel::{
    kernel::Kernel,
    kernel_message::{
      KernelCommand, KernelQuery, KernelQueryResponse, TaskContext, TaskInfo,
      TaskSelector,
    },
    task::TaskState,
    task_path::TaskPath,
  },
  protocol::{
    ActResult, ConnReceiver, ConnSender, CtlMsg, RpcError, RpcRequest,
    RpcTaskInfo, RpcWhy, RpcWhyDep, TaskListResult, codes, ok_result,
    server_handshake,
  },
  term::Size,
};

pub async fn run_server(
  working_dir: PathBuf,
  log_level: Option<&str>,
) -> anyhow::Result<()> {
  let (config, keymap, load_err) =
    match crate::config::config::Config::load_dir(&working_dir) {
      Ok(config) => {
        let keymap = config.keymap.build();
        (config, keymap, None)
      }
      Err(err) => {
        let config = crate::config::config::Config::make_default();
        let keymap = config.keymap.build();
        (config, keymap, Some(err))
      }
    };

  let _logger = crate::logging::init(crate::logging::Config {
    binary: "dekit",
    cli_level: log_level,
    log_env: "DEKIT_LOG",
    file_env: "DEKIT_LOG_FILE",
    config_level: config.log.level.as_deref(),
    config_file: config.log.file.as_deref(),
    default_dir: Some(&working_dir),
  })?;

  if let Some(err) = load_err {
    log::warn!("Failed to load dekit config: {}", err);
  }

  // Create lock file and acquire exclusive flock.
  let lock_guard = lockfile::create_lock_file(&working_dir)?;
  log::info!("Lock file created for directory: {}", working_dir.display());

  #[cfg(unix)]
  crate::process::unix_processes_waiter::UnixProcessesWaiter::init()?;
  let kernel = Kernel::new();
  let pc = kernel.context();

  let socket_path = lock_guard.socket_path().to_path_buf();
  let app_task_id = create_app_task(config, keymap, &pc);
  let app_sender = pc.get_task_sender(app_task_id);

  tokio::spawn(async move {
    let mut last_client_id = 0;

    let mut server_socket = match bind_server_socket(&socket_path).await {
      Ok(server_socket) => {
        log::info!("Server is listening.");
        server_socket
      }
      Err(err) => {
        log::error!("Failed to bind the server: {:?}", err);
        pc.send(KernelCommand::Quit);
        return;
      }
    };
    log::debug!("Waiting for clients...");
    loop {
      match server_socket.accept().await {
        Ok((sender, receiver)) => {
          last_client_id += 1;
          let client_id = ClientId(last_client_id);
          let app_sender = app_sender.clone();
          let pc = pc.clone();
          tokio::spawn(async move {
            dispatch_connection(client_id, app_sender, pc, sender, receiver)
              .await;
          });
        }
        Err(err) => {
          log::debug!("Server socket accept error: {}", err);
          break;
        }
      }
    }
  });

  kernel.run().await;

  // lock_guard is dropped here, removing lock + socket files.
  drop(lock_guard);

  #[cfg(unix)]
  crate::process::unix_processes_waiter::UnixProcessesWaiter::uninit()?;

  Ok(())
}

/// Dispatch an accepted connection: handshake, then one RPC request or an
/// attach session.
async fn dispatch_connection(
  client_id: ClientId,
  app_sender: crate::kernel::kernel_message::TaskSender,
  pc: TaskContext,
  mut sender: ConnSender,
  mut receiver: ConnReceiver,
) {
  if let Err(err) = server_handshake(&mut sender, &mut receiver).await {
    log::debug!("Client handshake failed: {err}");
    return;
  }

  let request = match receiver.recv_ctl().await {
    Ok(CtlMsg::Request(request)) => request,
    Ok(msg) => {
      log::warn!("Expected a request from client, got {msg:?}");
      return;
    }
    Err(err) => {
      log::debug!("Client connection closed: {err}");
      return;
    }
  };

  match RpcRequest::from_wire(&request.method, request.params) {
    Ok(RpcRequest::Attach { width, height }) => {
      client_session(
        client_id,
        app_sender,
        Size { width, height },
        request.id,
        sender,
        receiver,
      )
      .await;
    }
    Ok(req) => {
      let msg = match handle_rpc(&pc, req).await {
        Ok(result) => CtlMsg::ok(request.id, result),
        Err(error) => CtlMsg::err(request.id, error),
      };
      let _ = sender.send_ctl(msg).await;
    }
    Err(error) => {
      let _ = sender.send_ctl(CtlMsg::err(request.id, error)).await;
    }
  }
}

fn state_str(state: TaskState) -> String {
  match state {
    TaskState::Idle => "idle".to_string(),
    TaskState::Starting => "starting".to_string(),
    TaskState::Running => "running".to_string(),
    TaskState::Ready => "ready".to_string(),
    TaskState::Stopping => "stopping".to_string(),
    TaskState::Backoff => "backoff".to_string(),
    TaskState::Done(info) => format!("done ({})", info),
    TaskState::Exited(info) => info.to_string(),
  }
}

async fn query(
  pc: &TaskContext,
  query: KernelQuery,
) -> Result<KernelQueryResponse, RpcError> {
  pc.query(query).await.map_err(RpcError::internal)
}

async fn list_tasks(
  pc: &TaskContext,
  glob: Option<String>,
) -> Result<Vec<TaskInfo>, RpcError> {
  match query(pc, KernelQuery::ListTasks(glob)).await? {
    KernelQueryResponse::TaskList(tasks) => Ok(tasks),
    _ => Err(RpcError::internal("unexpected query response")),
  }
}

/// Send a selector command and return how many tasks it matched. The
/// count comes from the same kernel dispatch that acted, so it reflects
/// membership at act time.
async fn act_matching(
  pc: &TaskContext,
  make: impl FnOnce(Option<tokio::sync::oneshot::Sender<usize>>) -> KernelCommand,
) -> Result<usize, RpcError> {
  let (tx, rx) = tokio::sync::oneshot::channel();
  pc.send(make(Some(tx)));
  rx.await.map_err(RpcError::internal)
}

fn acted(matched: usize) -> Result<Value, RpcError> {
  serde_json::to_value(ActResult { matched }).map_err(RpcError::internal)
}

fn parse_path(path: &str) -> Result<TaskPath, RpcError> {
  TaskPath::resolve(TASK_ROOT, path)
    .map_err(|err| RpcError::new(codes::BAD_PATH, err.to_string()))
}

async fn handle_rpc(
  pc: &TaskContext,
  req: RpcRequest,
) -> Result<Value, RpcError> {
  match req {
    RpcRequest::Attach { .. } => Err(RpcError::internal(
      "attach must be the first request on a connection",
    )),

    RpcRequest::Ls { glob } => {
      let glob = glob.map(|g| TaskPath::resolve_spec(TASK_ROOT, &g));
      let tasks = list_tasks(pc, glob)
        .await?
        .into_iter()
        .map(|t| RpcTaskInfo {
          path: t
            .path
            .map(|p| p.to_string())
            .unwrap_or_else(|| format!("<task:{}>", t.id.0)),
          state: state_str(t.state),
        })
        .collect();
      serde_json::to_value(TaskListResult { tasks }).map_err(RpcError::internal)
    }

    RpcRequest::Up {} => {
      let tag = crate::config::proc::AUTOSTART_TAG.to_string();
      let matched = act_matching(pc, |ack| {
        KernelCommand::Start(TaskSelector::Tag(tag), ack)
      })
      .await?;
      acted(matched)
    }

    RpcRequest::Start { pattern } => {
      let glob =
        TaskSelector::Glob(TaskPath::resolve_spec(TASK_ROOT, &pattern));
      let matched =
        act_matching(pc, |ack| KernelCommand::Start(glob, ack)).await?;
      acted(matched)
    }

    RpcRequest::Stop { pattern } => {
      let glob =
        TaskSelector::Glob(TaskPath::resolve_spec(TASK_ROOT, &pattern));
      let matched =
        act_matching(pc, |ack| KernelCommand::Stop(glob, ack)).await?;
      acted(matched)
    }

    RpcRequest::Veto { pattern } => {
      let glob =
        TaskSelector::Glob(TaskPath::resolve_spec(TASK_ROOT, &pattern));
      let matched =
        act_matching(pc, |ack| KernelCommand::Veto(glob, ack)).await?;
      acted(matched)
    }

    RpcRequest::Down { pattern } => {
      let selector = match pattern {
        Some(pattern) => {
          TaskSelector::Glob(TaskPath::resolve_spec(TASK_ROOT, &pattern))
        }
        None => TaskSelector::All,
      };
      let matched =
        act_matching(pc, |ack| KernelCommand::Down(selector, ack)).await?;
      acted(matched)
    }

    RpcRequest::Kill { pattern } => {
      let glob =
        TaskSelector::Glob(TaskPath::resolve_spec(TASK_ROOT, &pattern));
      let matched =
        act_matching(pc, |ack| KernelCommand::Kill(glob, ack)).await?;
      acted(matched)
    }

    RpcRequest::Restart { pattern } => {
      let glob =
        TaskSelector::Glob(TaskPath::resolve_spec(TASK_ROOT, &pattern));
      let matched =
        act_matching(pc, |ack| KernelCommand::Restart(glob, ack)).await?;
      acted(matched)
    }

    RpcRequest::Why { path } => {
      let task_path = parse_path(&path)?;
      match query(pc, KernelQuery::Explain(task_path)).await? {
        KernelQueryResponse::Explain(Some(explain)) => {
          let why = RpcWhy {
            path,
            state: state_str(explain.state),
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
                state: state_str(d.state),
                wanted: d.wanted,
                satisfied: d.satisfied,
              })
              .collect(),
            attempts: explain.attempts,
          };
          serde_json::to_value(why).map_err(RpcError::internal)
        }
        KernelQueryResponse::Explain(None) => Err(RpcError::new(
          codes::NO_MATCH,
          format!("no task at '{}'", path),
        )),
        _ => Err(RpcError::internal("unexpected query response")),
      }
    }

    RpcRequest::Screen { path } => {
      let task_path = parse_path(&path)?;
      match query(pc, KernelQuery::GetScreen(task_path)).await? {
        KernelQueryResponse::Screen(Some(content)) => {
          serde_json::to_value(crate::protocol::ScreenResult {
            screen: Some(content),
          })
          .map_err(RpcError::internal)
        }
        KernelQueryResponse::Screen(None) => Err(RpcError::new(
          codes::NO_SCREEN,
          format!("no screen content for '{}'", path),
        )),
        _ => Err(RpcError::internal("unexpected query response")),
      }
    }

    RpcRequest::Shutdown {} => {
      pc.send(KernelCommand::Quit);
      Ok(ok_result())
    }

    RpcRequest::Spawn { path, cmd, cwd } => {
      let task_path = parse_path(&path)?;
      if cmd.is_empty() {
        return Err(RpcError::new(
          codes::INVALID_PARAMS,
          "cmd must not be empty",
        ));
      }
      let mut spec = crate::process::process_spec::ProcessSpec::from_argv(cmd);
      if let Some(cwd) = cwd {
        spec.cwd(cwd);
      } else if let Ok(cwd) = std::env::current_dir() {
        spec.cwd(cwd.to_string_lossy());
      }
      let mut cfg = crate::task::proc_task::ProcTaskConfig::new(spec);
      cfg.tags = vec![crate::config::proc::USER_TAG.to_string()];
      cfg.pinned = true;
      let (_id, ack) =
        crate::task::proc_task::spawn_proc_task(pc, Some(task_path), cfg);
      match ack.await {
        Ok(true) => Ok(ok_result()),
        Ok(false) => Err(RpcError::new(
          codes::PATH_TAKEN,
          format!("a task already exists at '{}'", path),
        )),
        Err(_) => Err(RpcError::internal("kernel dropped the registration")),
      }
    }
  }
}
