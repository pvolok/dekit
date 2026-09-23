use std::sync::Arc;

use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

use crate::{
  dekit::server::{
    ConnCtl, ConnReg, ServerCtx, carried, finish, resolve_screen,
  },
  kernel::{
    copy_mode::CopyMove,
    kernel_message::{
      KernelCommand, KernelQuery, KernelQueryResponse, SharedVt, TaskContext,
      TaskSelector,
    },
    task::TaskId,
    task_screen::{ObserverId, ScreenNotify, ScrollUnit, TaskScreenCmd},
  },
  protocol::{
    Bye, ConnReceiver, ConnSender, CtlMsg, Msg, ScreenCommand, codes,
    ctl::{EVENT_INPUT, EVENT_SCREEN, Hello},
    ok_result, screen,
  },
  target::Target,
  term::{ScreenDiffer, Size, TermEvent, Winsize, vt::emit},
  upgrade::snapshot as snap,
};

pub async fn attach_session(
  ctx: &Arc<ServerCtx>,
  mut reg: ConnReg,
  fd: Option<i32>,
  hello: &Hello,
  request_id: u64,
  target: Target,
  size: Size,
  until_exit: bool,
  mut sender: ConnSender,
  receiver: ConnReceiver,
) {
  let pc = &ctx.pc;
  let (task, vt) = match resolve_screen(pc, &target).await {
    Ok(found) => found,
    Err(error) => {
      if sender.queue_ctl(CtlMsg::err(request_id, error)).is_ok() {
        finish(
          &mut sender,
          &mut reg.ctl,
          fd,
          Some(hello),
          receiver.buffered(),
        )
        .await;
      }
      return;
    }
  };
  if sender
    .queue_ctl(CtlMsg::ok(request_id, ok_result()))
    .is_err()
  {
    return;
  }
  run_attached(
    ctx, reg, fd, hello, task.id, vt, size, until_exit, sender, receiver,
  )
  .await;
}

/// An attach session inherited across an upgrade: the task is re-observed
/// and the client gets a full repaint.
pub async fn resume_session(
  ctx: Arc<ServerCtx>,
  mut reg: ConnReg,
  fd: Option<i32>,
  hello: Hello,
  task: TaskId,
  size: Size,
  until_exit: bool,
  mut sender: ConnSender,
  receiver: ConnReceiver,
) {
  let Ok((_, vt)) = resolve_screen(&ctx.pc, &Target::Id(task)).await else {
    let bye = CtlMsg::Bye(Bye {
      code: codes::QUIT.to_string(),
      message: "task did not survive the upgrade".to_string(),
      state: None,
      screen: None,
    });
    if sender.queue_ctl(bye).is_ok() {
      finish(
        &mut sender,
        &mut reg.ctl,
        fd,
        Some(&hello),
        receiver.buffered(),
      )
      .await;
    }
    return;
  };
  // The client's terminal still has whatever state the old image's
  // painting left.
  let mut reset = Vec::new();
  ScreenDiffer::reset(&mut reset);
  if sender.queue_out(reset.into()).is_err() {
    return;
  }
  run_attached(
    &ctx, reg, fd, &hello, task, vt, size, until_exit, sender, receiver,
  )
  .await;
}

async fn run_attached(
  ctx: &Arc<ServerCtx>,
  mut reg: ConnReg,
  fd: Option<i32>,
  hello: &Hello,
  task: TaskId,
  vt: SharedVt,
  size: Size,
  until_exit: bool,
  mut sender: ConnSender,
  mut receiver: ConnReceiver,
) {
  let pc = &ctx.pc;
  let observer = ObserverId::new();
  let (sink, notifies) = unbounded_channel();
  pc.send_msg(
    task,
    TaskScreenCmd::Attach {
      observer,
      size: Winsize {
        x: size.width,
        y: size.height,
        x_px: 0,
        y_px: 0,
      },
      sink,
    },
  );
  let mut until = None;
  let mut ended = false;
  if until_exit {
    // The watch is registered before the state query and both go
    // through the one kernel queue, so an exit is never missed:
    // either the query already sees it or the watch reports it.
    until = Some(pc.watch_active(TaskSelector::Id(task)));
    ended = !kernel_task_state(pc, task)
      .await
      .is_some_and(|state| state.is_active());
  }
  let freeze = FreezeInfo {
    fd,
    hello,
    size,
    until_exit,
  };
  let end = session(
    pc,
    task,
    observer,
    &vt,
    notifies,
    until,
    ended,
    &mut sender,
    &mut receiver,
    &mut reg.ctl,
    freeze,
  )
  .await;
  // Whatever ended the session, the screen must not keep our geometry.
  pc.send_msg(task, TaskScreenCmd::Detach { observer });
  let bye = match end {
    SessionEnd::TaskExited => {
      // Capture the final state and screen before reaping the task.
      let state = kernel_task_state(pc, task)
        .await
        .map(crate::dekit::server::task_state);
      let screen = final_screen_text(&vt);
      // A foreground `run` (the only `until_exit` caller) owns its
      // task; the runner reaps it here so a client that dies before
      // it could ask can never leak the task.
      pc.send(KernelCommand::Remove(TaskSelector::Id(task), None));
      Bye {
        code: codes::TASK_EXITED.to_string(),
        message: String::new(),
        state,
        screen,
      }
    }
    SessionEnd::Closed => Bye {
      code: codes::QUIT.to_string(),
      message: String::new(),
      state: None,
      screen: None,
    },
  };
  if sender.queue_ctl(CtlMsg::Bye(bye)).is_ok() {
    finish(
      &mut sender,
      &mut reg.ctl,
      fd,
      Some(hello),
      receiver.buffered(),
    )
    .await;
  }
}

/// What the session reports when frozen for an upgrade.
struct FreezeInfo<'a> {
  fd: Option<i32>,
  hello: &'a Hello,
  size: Size,
  until_exit: bool,
}

/// The task's final screen as ANSI text, trailing blank space trimmed,
/// or `None` if it is empty.
fn final_screen_text(vt: &SharedVt) -> Option<String> {
  let screen = vt.read().ok()?;
  let text = crate::term::ansi::render_screen_ansi(&screen);
  let trimmed = text.trim_end_matches(['\n', '\r', ' ']);
  if trimmed.is_empty() {
    None
  } else {
    Some(trimmed.to_string())
  }
}

enum SessionEnd {
  /// The client went away or closed the session.
  Closed,
  /// `until_exit`: the attached task's execution finished.
  TaskExited,
}

async fn kernel_task_state(
  pc: &TaskContext,
  id: TaskId,
) -> Option<crate::kernel::task::TaskState> {
  match pc.query(KernelQuery::ListTasks(TaskSelector::Id(id))).await {
    Ok(KernelQueryResponse::TaskList(tasks)) => {
      tasks.into_iter().next().map(|task| task.state)
    }
    Ok(KernelQueryResponse::Explain(_)) | Err(_) => None,
  }
}

async fn session(
  pc: &TaskContext,
  task: TaskId,
  observer: ObserverId,
  vt: &SharedVt,
  mut notifies: UnboundedReceiver<ScreenNotify>,
  mut until: Option<UnboundedReceiver<bool>>,
  mut ended: bool,
  sender: &mut ConnSender,
  receiver: &mut ConnReceiver,
  ctl: &mut UnboundedReceiver<ConnCtl>,
  mut freeze: FreezeInfo<'_>,
) -> SessionEnd {
  let mut differ = ScreenDiffer::new();
  // Copy-mode surface, painted instead of `vt` while set.
  let mut present: Option<SharedVt> = None;
  let mut title = String::new();
  let mut batch = Vec::new();
  // Frozen for an upgrade: nothing read or painted until thawed.
  let mut frozen = false;
  loop {
    if ended && !frozen {
      // The task reports its exit only after its output reached the
      // vt, so painting now shows the complete final screen even if
      // render notifies are still queued.
      let mut out = Vec::new();
      render(&mut differ, vt, &present, &mut title, &mut out);
      if !out.is_empty() {
        let _ = sender.queue_out(out.into());
      }
      return SessionEnd::TaskExited;
    }
    let exit_watch = async {
      match until.as_mut() {
        Some(watch) => watch.recv().await,
        None => std::future::pending().await,
      }
    };
    tokio::select! {
      msg = ctl.recv() => match msg {
        Some(ConnCtl::Freeze(reply)) => {
          frozen = true;
          let kind = snap::ConnectionKind::Attach {
            task: task.0,
            width: freeze.size.width,
            height: freeze.size.height,
            until_exit: freeze.until_exit,
          };
          let _ = reply.send(carried(
            freeze.fd,
            Some(freeze.hello),
            receiver.buffered(),
            sender,
            kind,
          ));
        }
        Some(ConnCtl::Thaw) => frozen = false,
        None => return SessionEnd::Closed,
      },
      // Painted only once the last paint is written: a client that stops
      // reading gets one catch-up paint when it reads again.
      n = notifies.recv_many(&mut batch, 256), if !frozen && sender.pending().is_empty() => {
        if n == 0 {
          return SessionEnd::Closed;
        }
        let mut paint = false;
        let mut out = Vec::new();
        for notify in batch.drain(..) {
          match notify {
            ScreenNotify::Attached | ScreenNotify::Render => paint = true,
            ScreenNotify::Bell => out.push(0x07),
            ScreenNotify::CopyPresent { vt, .. } => {
              present = vt;
              paint = true;
            }
            ScreenNotify::Yank { text } => {
              emit::osc52_copy(&mut out, &text);
              // For terminals without OSC 52, while the runner is local.
              tokio::task::spawn_blocking(move || crate::clipboard::copy(&text));
            }
          }
        }
        if paint {
          render(&mut differ, vt, &present, &mut title, &mut out);
        }
        if !out.is_empty() && sender.queue_out(out.into()).is_err() {
          return SessionEnd::Closed;
        }
      }
      written = sender.flush(), if !frozen && !sender.pending().is_empty() => {
        if written.is_err() {
          return SessionEnd::Closed;
        }
      }
      active = exit_watch, if !frozen => match active {
        Some(true) => {}
        Some(false) => ended = true,
        // The watch channel closes only when the kernel is shutting
        // down — that is a session close, not a task exit.
        None => return SessionEnd::Closed,
      },
      msg = receiver.recv(), if !frozen => match msg {
        Some(Ok(Msg::Ctl(CtlMsg::Event(event)))) if event.name == EVENT_INPUT => {
          match serde_json::from_value::<TermEvent>(event.params) {
            Ok(event) => {
              if let TermEvent::Resize(width, height) = event {
                freeze.size = Size { width, height };
              }
              pc.send_msg(task, TaskScreenCmd::Input { observer, event });
            }
            Err(err) => log::debug!("attach: dropping input event: {err}"),
          }
        }
        Some(Ok(Msg::Ctl(CtlMsg::Event(event)))) if event.name == EVENT_SCREEN => {
          match serde_json::from_value::<ScreenCommand>(event.params) {
            Ok(command) => pc.send_msg(task, screen_cmd(command)),
            Err(err) => log::debug!("attach: dropping screen event: {err}"),
          }
        }
        Some(Ok(msg)) => log::debug!("attach: ignoring {msg:?}"),
        Some(Err(err)) => {
          log::debug!("attach: closing: {err}");
          return SessionEnd::Closed;
        }
        None => return SessionEnd::Closed,
      },
    }
  }
}

fn render(
  differ: &mut ScreenDiffer,
  vt: &SharedVt,
  present: &Option<SharedVt>,
  title: &mut String,
  out: &mut Vec<u8>,
) {
  if let Ok(screen) = vt.read()
    && screen.title() != *title
  {
    *title = screen.title().to_string();
    emit::osc_title(out, title);
  }
  if let Ok(screen) = present.as_ref().unwrap_or(vt).read() {
    differ.diff(out, &*screen);
  }
}

fn screen_cmd(command: ScreenCommand) -> TaskScreenCmd {
  match command {
    ScreenCommand::Scroll { delta, unit } => TaskScreenCmd::Scroll {
      delta,
      unit: match unit {
        screen::ScrollUnit::Line => ScrollUnit::Line,
        screen::ScrollUnit::HalfScreen => ScrollUnit::HalfScreen,
        screen::ScrollUnit::Screen => ScrollUnit::Screen,
      },
    },
    ScreenCommand::CopyEnter => TaskScreenCmd::CopyEnter,
    ScreenCommand::CopyLeave => TaskScreenCmd::CopyLeave,
    ScreenCommand::CopyMove { dir } => TaskScreenCmd::CopyMove {
      dir: match dir {
        screen::CopyMove::Up => CopyMove::Up,
        screen::CopyMove::Down => CopyMove::Down,
        screen::CopyMove::Left => CopyMove::Left,
        screen::CopyMove::Right => CopyMove::Right,
      },
    },
    ScreenCommand::CopySelect => TaskScreenCmd::CopyBeginSelection,
    ScreenCommand::CopyYank => TaskScreenCmd::CopyYank,
  }
}

#[cfg(test)]
mod tests {
  use std::{sync::Arc, time::Duration};

  use tokio::{io::duplex, time::timeout};

  use crate::{
    config::config::Config,
    console::app::console_task_registration,
    dekit::server::{Connections, ServerCtx, dispatch_connection},
    kernel::{
      kernel::Kernel,
      kernel_message::{KernelCommand, TaskContext},
      task::{TaskDef, TaskId},
      task_key::{TaskKey, TaskSpaceId},
      task_path::TaskPath,
    },
    protocol::{
      ConnReceiver, ConnSender, CtlMsg, Event, Msg, Request, RpcRequest,
      ScreenCommand, client_handshake,
      ctl::{EVENT_INPUT, EVENT_SCREEN},
      screen,
    },
    term::{
      TermEvent,
      key::{Key, KeyCode, KeyMods},
    },
  };

  /// The next `Out` frame, or None if the session ended first.
  async fn next_out(receiver: &mut ConnReceiver) -> Option<Vec<u8>> {
    loop {
      match receiver.recv().await {
        Some(Ok(Msg::Out(bytes))) => return Some(bytes.to_vec()),
        Some(Ok(Msg::Ctl(CtlMsg::Response(response)))) => {
          assert!(response.error.is_none(), "{:?}", response.error);
        }
        Some(Ok(Msg::Ctl(_))) => (),
        Some(Err(_)) | None => return None,
      }
    }
  }

  /// Reads `Out` frames until one contains `needle`.
  async fn wait_for(receiver: &mut ConnReceiver, needle: &[u8]) -> bool {
    timeout(Duration::from_secs(2), async {
      let mut out = Vec::new();
      while let Some(frame) = next_out(receiver).await {
        out.extend(frame);
        if out.windows(needle.len()).any(|w| w == needle) {
          return true;
        }
      }
      false
    })
    .await
    .unwrap()
  }

  /// A client attached to `target` through a served connection.
  async fn attach(
    pc: &TaskContext,
    config: &Arc<Config>,
    target: &str,
    until_exit: bool,
  ) -> (ConnSender, ConnReceiver, tokio::task::JoinHandle<()>) {
    let ctx = ServerCtx::local(pc.clone(), config.clone());
    attach_through(&ctx, target, until_exit, 64 * 1024, None).await
  }

  /// `attach` over a pipe of `buffer` bytes, with `fd` standing in for
  /// the socket fd.
  async fn attach_through(
    ctx: &Arc<ServerCtx>,
    target: &str,
    until_exit: bool,
    buffer: usize,
    fd: Option<i32>,
  ) -> (ConnSender, ConnReceiver, tokio::task::JoinHandle<()>) {
    let (client, server) = duplex(buffer);
    let (client_read, client_write) = tokio::io::split(client);
    let (server_read, server_write) = tokio::io::split(server);
    let session = tokio::spawn(dispatch_connection(
      ctx.clone(),
      Connections::register(ctx),
      ConnSender::new(server_write),
      ConnReceiver::new(server_read),
      fd,
      None,
    ));
    let mut sender = ConnSender::new(client_write);
    let mut receiver = ConnReceiver::new(client_read);
    client_handshake(&mut sender, &mut receiver).await.unwrap();
    let (method, params) = RpcRequest::Attach {
      target: target.parse().unwrap(),
      width: 80,
      height: 24,
      until_exit,
    }
    .to_wire();
    sender
      .send_ctl(CtlMsg::Request(Request {
        id: 1,
        method,
        params,
      }))
      .await
      .unwrap();
    (sender, receiver, session)
  }

  async fn finish(
    pc: TaskContext,
    sender: ConnSender,
    receiver: ConnReceiver,
    session: tokio::task::JoinHandle<()>,
    kernel: tokio::task::JoinHandle<()>,
  ) {
    // Dropping the client ends the session.
    drop(sender);
    drop(receiver);
    timeout(Duration::from_secs(2), session)
      .await
      .unwrap()
      .unwrap();
    pc.send(KernelCommand::Quit);
    timeout(Duration::from_secs(2), kernel)
      .await
      .unwrap()
      .unwrap();
  }

  #[tokio::test]
  async fn until_exit_ends_the_attach_when_the_task_is_not_active() {
    use crate::{
      process::process_spec::ProcessSpec,
      protocol::codes,
      task::process_task::{ProcessTaskConfig, process_task_registration},
    };

    let config = Arc::new(Config::make_default());
    let mut kernel = Kernel::new();
    let pc = kernel.context();
    let spec = ProcessSpec::from_argv(vec![
      "sh".to_string(),
      "-c".to_string(),
      "true".to_string(),
    ]);
    // Never started: the task sits Idle, which `until_exit` treats as
    // already finished.
    kernel
      .register_task_registration(process_task_registration(
        pc.alloc_id(),
        Some(TaskKey::default_space(TaskPath::new("oneshot").unwrap())),
        ProcessTaskConfig {
          pinned: false,
          ..ProcessTaskConfig::new(spec)
        },
      ))
      .unwrap();
    let kernel_handle = tokio::spawn(kernel.run());

    let (sender, mut receiver, session) =
      attach(&pc, &config, "oneshot", true).await;

    let bye = timeout(Duration::from_secs(2), async {
      loop {
        match receiver.recv().await {
          Some(Ok(Msg::Ctl(CtlMsg::Bye(bye)))) => return Some(bye.code),
          Some(Ok(_)) => (),
          Some(Err(_)) | None => return None,
        }
      }
    })
    .await
    .unwrap();
    assert_eq!(bye.as_deref(), Some(codes::TASK_EXITED));

    finish(pc, sender, receiver, session, kernel_handle).await;
  }

  #[tokio::test]
  async fn attaches_to_the_console_and_forwards_input() {
    let config = Arc::new(Config::make_default());
    let (pc, _, kernel_handle) = console_kernel(&config);

    let (mut sender, mut receiver, session) =
      attach(&pc, &config, "@dekit/console", false).await;

    // The console paints its sidebar once attached.
    assert!(wait_for(&mut receiver, b"Tasks").await);

    // Input reaches the console: `?` toggles the help window.
    let key = Key::new(KeyCode::Char('?'), KeyMods::NONE);
    sender
      .send_ctl(CtlMsg::Event(Event {
        name: EVENT_INPUT.to_string(),
        params: serde_json::to_value(TermEvent::Key(key)).unwrap(),
      }))
      .await
      .unwrap();
    let repainted = timeout(Duration::from_secs(2), next_out(&mut receiver))
      .await
      .unwrap();
    assert!(repainted.is_some());

    finish(pc, sender, receiver, session, kernel_handle).await;
  }

  #[tokio::test]
  async fn console_quit_key_detaches_without_stopping_runner() {
    let config = Arc::new(Config::make_default());
    let (pc, _, kernel_handle) = console_kernel(&config);

    let (mut sender, mut receiver, session) =
      attach(&pc, &config, "@dekit/console", false).await;
    assert!(wait_for(&mut receiver, b"Tasks").await);

    sender
      .send_ctl(CtlMsg::Event(Event {
        name: EVENT_INPUT.to_string(),
        params: serde_json::to_value(TermEvent::Key(Key::new(
          KeyCode::Char('q'),
          KeyMods::NONE,
        )))
        .unwrap(),
      }))
      .await
      .unwrap();

    // The attachment closes, but the runner remains available for another.
    assert!(
      timeout(Duration::from_secs(2), next_out(&mut receiver))
        .await
        .unwrap()
        .is_none()
    );
    timeout(Duration::from_secs(2), session)
      .await
      .unwrap()
      .unwrap();
    drop(sender);
    drop(receiver);

    let (sender, mut receiver, session) =
      attach(&pc, &config, "@dekit/console", false).await;
    assert!(wait_for(&mut receiver, b"Tasks").await);
    finish(pc, sender, receiver, session, kernel_handle).await;
  }

  /// A kernel running just the console, as `@dekit/console`.
  fn console_kernel(
    config: &Arc<Config>,
  ) -> (TaskContext, TaskId, tokio::task::JoinHandle<()>) {
    let mut kernel = Kernel::new();
    let pc = kernel.context();
    let console_id = pc.alloc_id();
    kernel
      .register_task_registration(console_task_registration(
        console_id,
        TaskDef {
          space: TaskSpaceId::dekit(),
          path: Some(TaskPath::new("console").unwrap()),
          ..TaskDef::default()
        },
        config.clone(),
        config.keymap.build(),
      ))
      .unwrap();
    (pc, console_id, tokio::spawn(kernel.run()))
  }

  #[tokio::test]
  async fn a_client_that_stops_reading_still_freezes() {
    let config = Arc::new(Config::make_default());
    let (pc, _, kernel_handle) = console_kernel(&config);
    let ctx = ServerCtx::local(pc.clone(), config.clone());
    // A pipe far smaller than a paint of the console.
    let (_sender, _receiver, session) =
      attach_through(&ctx, "@dekit/console", false, 256, Some(7)).await;
    // From here on the client reads nothing. Once the paint is stuck in
    // the pipe, the freeze still comes back, carrying the rest.
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    let frozen = loop {
      let reply = ctx.connections.freeze_all().await.pop().unwrap();
      let frozen = timeout(Duration::from_secs(1), reply)
        .await
        .expect("froze in time")
        .unwrap()
        .expect("carried");
      if !frozen.buffered_output.is_empty() {
        break frozen;
      }
      ctx.connections.thaw_all();
      assert!(
        std::time::Instant::now() < deadline,
        "the paint never stuck"
      );
      tokio::time::sleep(Duration::from_millis(10)).await;
    };
    match frozen.kind {
      crate::upgrade::snapshot::ConnectionKind::Attach { width, .. } => {
        assert_eq!(width, 80)
      }
      kind => panic!("expected an attach session, got {kind:?}"),
    }

    session.abort();
    pc.send(KernelCommand::Quit);
    timeout(Duration::from_secs(2), kernel_handle)
      .await
      .unwrap()
      .unwrap();
  }

  #[tokio::test]
  async fn a_resumed_session_resets_the_terminal_before_painting() {
    let config = Arc::new(Config::make_default());
    let (pc, console_id, kernel_handle) = console_kernel(&config);
    let (client, server) = duplex(64 * 1024);
    let (client_read, client_write) = tokio::io::split(client);
    let (server_read, server_write) = tokio::io::split(server);
    let ctx = ServerCtx::local(pc.clone(), config.clone());
    let session = tokio::spawn(super::resume_session(
      ctx.clone(),
      Connections::register(&ctx),
      None,
      crate::protocol::ctl::local_hello(),
      console_id,
      crate::term::Size {
        width: 80,
        height: 24,
      },
      false,
      ConnSender::new(server_write),
      ConnReceiver::new(server_read),
    ));
    let sender = ConnSender::new(client_write);
    let mut receiver = ConnReceiver::new(client_read);

    let first = timeout(Duration::from_secs(2), next_out(&mut receiver))
      .await
      .unwrap()
      .expect("painted");
    assert!(
      first.starts_with(b"\x1b[0m\x1b[?25h\x1b[0 q"),
      "{:?}",
      String::from_utf8_lossy(&first)
    );
    assert!(wait_for(&mut receiver, b"Tasks").await);

    finish(pc, sender, receiver, session, kernel_handle).await;
  }

  #[cfg(not(windows))]
  #[tokio::test]
  async fn screen_commands_drive_copy_mode_on_a_process() {
    use crate::{
      process::process_spec::ProcessSpec,
      task::process_task::{ProcessTaskConfig, process_task_registration},
    };

    let config = Arc::new(Config::make_default());
    let mut kernel = Kernel::new();
    let pc = kernel.context();
    let spec = ProcessSpec::from_argv(vec![
      "sh".to_string(),
      "-c".to_string(),
      "echo hello-copy; sleep 30".to_string(),
    ]);
    kernel
      .register_task_registration(process_task_registration(
        pc.alloc_id(),
        Some(TaskKey::default_space(TaskPath::new("echo").unwrap())),
        ProcessTaskConfig {
          pinned: true,
          ..ProcessTaskConfig::new(spec)
        },
      ))
      .unwrap();
    let kernel_handle = tokio::spawn(kernel.run());

    let (mut sender, mut receiver, session) =
      attach(&pc, &config, "echo", false).await;
    assert!(wait_for(&mut receiver, b"hello-copy").await);

    // Select the first cell of the top row and yank it: OSC 52 comes back.
    for command in [
      ScreenCommand::CopyEnter,
      ScreenCommand::Scroll {
        delta: 1,
        unit: screen::ScrollUnit::Screen,
      },
      ScreenCommand::CopySelect,
      ScreenCommand::CopyMove {
        dir: screen::CopyMove::Right,
      },
      ScreenCommand::CopyYank,
    ] {
      sender
        .send_ctl(CtlMsg::Event(Event {
          name: EVENT_SCREEN.to_string(),
          params: serde_json::to_value(command).unwrap(),
        }))
        .await
        .unwrap();
    }
    assert!(wait_for(&mut receiver, b"\x1b]52;;").await);

    // No SIGCHLD waiter in unit tests: remove the task so quit can finish.
    pc.send(KernelCommand::Remove(
      crate::kernel::kernel_message::TaskSelector::all(),
      None,
    ));
    finish(pc, sender, receiver, session, kernel_handle).await;
  }
}
