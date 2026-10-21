use std::{collections::VecDeque, pin::Pin, sync::Arc, time::Duration};

use tokio::{
  sync::mpsc::{UnboundedReceiver, unbounded_channel},
  time::Sleep,
};

use crate::{
  dekit::server::{
    ConnCtl, ConnReg, ServerCtx, carried, finish, resolve_screen,
  },
  kernel::{
    copy_mode::CopyMove,
    kernel_message::{KernelCommand, ScreenHandle, TaskContext, TaskSelector},
    task::TaskId,
    task_screen::{ObserverId, ScreenNotify, ScrollUnit, TaskScreenCmd},
  },
  protocol::{
    Bye, ByeSwitch, ConnReceiver, ConnSender, CtlMsg, Event, Feature, Msg,
    ScreenCommand, TermCaps, codes,
    ctl::{
      CopyEvent, EVENT_COPY, EVENT_SCREEN, Hello, input_of, is_input_event,
    },
    ok_result, screen,
  },
  target::Target,
  term::{Frame, Geometry, Painter, TermEvent, reveal, vt::emit},
  upgrade::snapshot as snap,
};

/// What an attach request asked for, kept for the session's life: the
/// geometry follows the client's resizes, and all of it goes into the
/// snapshot when the session is carried across an upgrade.
pub struct Attachment {
  pub geometry: Geometry,
  /// End the session when the task's execution finishes.
  pub until_exit: bool,
  /// The client's terminal, as it said; `None` from a client that did
  /// not say, which is painted as a plain true-color terminal.
  pub caps: Option<TermCaps>,
}

impl Attachment {
  fn snapshot(&self, task: TaskId) -> snap::ConnectionKind {
    snap::ConnectionKind::Attach {
      task: task.0,
      width: self.geometry.size.width,
      height: self.geometry.size.height,
      until_exit: self.until_exit,
      caps: self.caps,
      cell_pixels: self.geometry.cell_pixels,
    }
  }
}

pub async fn attach_session(
  ctx: &Arc<ServerCtx>,
  mut reg: ConnReg,
  fd: Option<i32>,
  hello: &Hello,
  request_id: u64,
  target: Target,
  attach: Attachment,
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
    ctx, reg, fd, hello, task.id, vt, attach, false, sender, receiver,
  )
  .await;
}

/// An attach session inherited across an upgrade: the task is re-observed
/// and the client gets a full repaint, swept in over what the old image
/// left on its terminal once the screen has settled.
pub async fn resume_session(
  ctx: Arc<ServerCtx>,
  mut reg: ConnReg,
  fd: Option<i32>,
  hello: Hello,
  task: TaskId,
  attach: Attachment,
  mut sender: ConnSender,
  receiver: ConnReceiver,
) {
  let Ok((_, vt)) = resolve_screen(&ctx.pc, &Target::Id(task)).await else {
    let bye =
      CtlMsg::Bye(Bye::new(codes::QUIT, "task did not survive the upgrade"));
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
  run_attached(
    &ctx, reg, fd, &hello, task, vt, attach, true, sender, receiver,
  )
  .await;
}

async fn run_attached(
  ctx: &Arc<ServerCtx>,
  mut reg: ConnReg,
  fd: Option<i32>,
  hello: &Hello,
  task: TaskId,
  vt: ScreenHandle,
  attach: Attachment,
  resumed: bool,
  mut sender: ConnSender,
  mut receiver: ConnReceiver,
) {
  let pc = &ctx.pc;
  let caps = attach.caps.unwrap_or_default();
  if resumed {
    // The client's terminal still has whatever state the old image's
    // painting left.
    let mut reset = Vec::new();
    Painter::reset(&mut reset, &caps);
    if sender.queue_out(reset.into()).is_err() {
      return;
    }
  }
  let observer = ObserverId::new();
  let (sink, notifies) = unbounded_channel();
  pc.send_msg(
    task,
    TaskScreenCmd::Attach {
      observer,
      geometry: attach.geometry,
      sink,
      client_version: Some(hello.version.clone()),
      background: caps.background,
    },
  );
  let mut until = None;
  let mut ended = false;
  if attach.until_exit {
    // The watch is registered before the state query and both go
    // through the one kernel queue, so an exit is never missed:
    // either the query already sees it or the watch reports it.
    until = Some(pc.watch_active(TaskSelector::Id(task)));
    ended = !kernel_task_state(pc, task)
      .await
      .is_some_and(|state| state.is_active());
  }
  let freeze = FreezeInfo { fd, hello, attach };
  let mut painter = Painter::new(caps);
  let end = session(
    pc,
    task,
    observer,
    &vt,
    notifies,
    until,
    ended,
    resumed,
    &mut sender,
    &mut receiver,
    &mut reg.ctl,
    freeze,
    &mut painter,
  )
  .await;
  // Whatever ended the session, the screen must not keep our geometry.
  pc.send_msg(task, TaskScreenCmd::Detach { observer });
  // Images placed on the client's terminal go with the session.
  let mut cleanup = Vec::new();
  painter.cleanup(&mut cleanup);
  if !cleanup.is_empty() {
    let _ = sender.queue_out(cleanup.into());
  }
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
        state,
        screen,
        ..Bye::new(codes::TASK_EXITED, "")
      }
    }
    SessionEnd::Closed => Bye::new(codes::QUIT, ""),
    SessionEnd::Switch { kind, root } => Bye {
      switch: Some(ByeSwitch { kind, root }),
      ..Bye::new(codes::QUIT, "")
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

/// How long a resumed session waits for the screen to stop changing
/// before sweeping it in, and the most it waits in all.
const REVEAL_SETTLE: Duration = Duration::from_millis(80);
const REVEAL_SETTLE_MAX: Duration = Duration::from_millis(300);

/// The sweep of a resumed session's first paint (`term::reveal`).
enum Reveal {
  Off,
  /// Waiting for the screen to settle; `deadline` caps the wait.
  Settling {
    deadline: tokio::time::Instant,
  },
  /// Frames still to write, one per `FRAME_INTERVAL`, painting `shown`.
  Sweeping {
    frames: VecDeque<Vec<u8>>,
    shown: Arc<Frame>,
  },
}

/// What the session reports when frozen for an upgrade.
struct FreezeInfo<'a> {
  fd: Option<i32>,
  hello: &'a Hello,
  attach: Attachment,
}

/// The task's final screen as ANSI text, trailing blank space trimmed,
/// or `None` if it is empty.
fn final_screen_text(vt: &ScreenHandle) -> Option<String> {
  let text = crate::term::ansi::render_frame_ansi(&vt.load());
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
  /// The console sends the client on to another runner.
  Switch { kind: String, root: String },
}

async fn kernel_task_state(
  pc: &TaskContext,
  id: TaskId,
) -> Option<crate::kernel::task::TaskState> {
  let tasks = pc.list_tasks(TaskSelector::Id(id)).await;
  tasks.into_iter().next().map(|task| task.state)
}

async fn session(
  pc: &TaskContext,
  task: TaskId,
  observer: ObserverId,
  vt: &ScreenHandle,
  mut notifies: UnboundedReceiver<ScreenNotify>,
  mut until: Option<UnboundedReceiver<bool>>,
  mut ended: bool,
  resumed: bool,
  sender: &mut ConnSender,
  receiver: &mut ConnReceiver,
  ctl: &mut UnboundedReceiver<ConnCtl>,
  mut freeze: FreezeInfo<'_>,
  painter: &mut Painter,
) -> SessionEnd {
  // Copy-mode surface, painted instead of `vt` while set.
  let mut present: Option<ScreenHandle> = None;
  let mut title = String::new();
  let mut batch = Vec::new();
  // Frozen for an upgrade: nothing read or painted until thawed.
  let mut frozen = false;
  // After an upgrade the client still shows the old kernel's paint, and
  // the first paint of the new one is swept in over it. Paints are held
  // until the screen has been quiet for a moment (the console redraws on
  // re-attach), at most `REVEAL_SETTLE_MAX`, so the sweep reveals the
  // task's settled screen. The sweep is paced by the loop, never awaited
  // inline: a client that stops reading stalls it, not its own input.
  let mut reveal = if resumed {
    Reveal::Settling {
      deadline: tokio::time::Instant::now() + REVEAL_SETTLE_MAX,
    }
  } else {
    Reveal::Off
  };
  let mut reveal_timer: Pin<Box<Sleep>> =
    Box::pin(tokio::time::sleep(REVEAL_SETTLE));
  loop {
    if ended && !frozen {
      // The task reports its exit only after its output reached the
      // vt, so painting now shows the complete final screen even if
      // render notifies are still queued.
      let mut out = Vec::new();
      render(painter, vt, &present, &mut title, &mut out);
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
          let kind = freeze.attach.snapshot(task);
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
            ScreenNotify::Attached | ScreenNotify::Render { .. } => {
              paint = true
            }
            ScreenNotify::Bell => out.push(0x07),
            ScreenNotify::CopyPresent { handle, .. } => {
              present = handle;
              paint = true;
            }
            // The client owns the user's clipboard: it knows which
            // terminal and machine the user is at, the runner does not.
            ScreenNotify::Yank { text } => {
              let event = CtlMsg::Event(Event {
                name: EVENT_COPY.to_string(),
                params: serde_json::to_value(CopyEvent { text })
                  .expect("a string serializes"),
              });
              if sender.queue_ctl(event).is_err() {
                return SessionEnd::Closed;
              }
            }
            ScreenNotify::Switch { kind, root } => {
              return SessionEnd::Switch { kind, root };
            }
          }
        }
        match &reveal {
          Reveal::Off if paint => {
            render(painter, vt, &present, &mut title, &mut out);
          }
          Reveal::Settling { deadline, .. } if paint => {
            let quiet = tokio::time::Instant::now() + REVEAL_SETTLE;
            reveal_timer.as_mut().reset(quiet.min(*deadline));
          }
          // The paint after the sweep catches up on anything drawn
          // meanwhile.
          Reveal::Off | Reveal::Settling { .. } | Reveal::Sweeping { .. } => (),
        }
        if !out.is_empty() && sender.queue_out(out.into()).is_err() {
          return SessionEnd::Closed;
        }
      }
      _ = reveal_timer.as_mut(), if !matches!(reveal, Reveal::Off) && !frozen && sender.pending().is_empty() => {
        let (mut frames, shown) = match std::mem::replace(&mut reveal, Reveal::Off) {
          Reveal::Off => unreachable!("guarded"),
          Reveal::Settling { .. } => {
            let shown = present.as_ref().unwrap_or(vt).load();
            let links = painter.caps().has(Feature::Hyperlinks);
            (VecDeque::from(reveal::frames(&shown, links)), shown)
          }
          Reveal::Sweeping { frames, shown } => (frames, shown),
        };
        let mut out = Vec::new();
        let swept = match frames.pop_front() {
          Some(frame) => {
            out = frame;
            true
          }
          None => false,
        };
        if frames.is_empty() {
          // The sweep painted `shown` whole with the cursor hidden; this
          // paint places the cursor and brings in what changed since.
          if swept {
            painter.assume(Some(shown));
          }
          render(painter, vt, &present, &mut title, &mut out);
        } else {
          reveal = Reveal::Sweeping { frames, shown };
          reveal_timer
            .as_mut()
            .reset(tokio::time::Instant::now() + reveal::FRAME_INTERVAL);
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
        Some(Ok(Msg::Ctl(CtlMsg::Event(event)))) if is_input_event(&event.name) => {
          match input_of(event) {
            Ok(event) => {
              if let TermEvent::Resize(geometry) = &event {
                freeze.attach.geometry = *geometry;
                // The frames were cut for the old size; the paint the
                // resize brings repaints everything instead.
                if matches!(reveal, Reveal::Sweeping { .. }) {
                  reveal = Reveal::Off;
                  painter.assume(None);
                }
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
  painter: &mut Painter,
  vt: &ScreenHandle,
  present: &Option<ScreenHandle>,
  title: &mut String,
  out: &mut Vec<u8>,
) {
  let live = vt.load();
  if live.title.as_ref() != title.as_str() {
    *title = live.title.to_string();
    emit::osc_title(out, title);
  }
  let frame = match present {
    Some(present) => present.load(),
    None => live,
  };
  painter.paint(out, &frame);
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
      Bye, ByeSwitch, ConnReceiver, ConnSender, CtlMsg, Event, Msg, Request,
      RpcRequest, codes,
      ctl::{
        CopyEvent, EVENT_COPY, GeometryParams, Hello, InputDialect, local_hello,
      },
    },
    term::{
      Geometry, TermEvent,
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

  async fn press(sender: &mut ConnSender, c: char) {
    let key = Key::new(KeyCode::Char(c), KeyMods::NONE);
    sender
      .send_ctl(CtlMsg::Event(
        InputDialect::Named.input_event(&TermEvent::Key(key)),
      ))
      .await
      .unwrap();
  }

  /// Reads `Out` frames until one contains `needle`.
  async fn wait_for(receiver: &mut ConnReceiver, needle: &[u8]) -> bool {
    wait_for_within(receiver, needle, Duration::from_secs(2)).await
  }

  async fn wait_for_within(
    receiver: &mut ConnReceiver,
    needle: &[u8],
    limit: Duration,
  ) -> bool {
    timeout(limit, async {
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
    attach_through(&ctx, local_hello(), target, until_exit, 64 * 1024, None)
      .await
  }

  /// `attach` over a pipe of `buffer` bytes, with `fd` standing in for
  /// the socket fd, from a client that says `hello` of itself.
  async fn attach_through(
    ctx: &Arc<ServerCtx>,
    hello: Hello,
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
    sender.send_ctl(CtlMsg::Hello(hello)).await.unwrap();
    assert!(matches!(
      receiver.recv_ctl().await.unwrap(),
      CtlMsg::Hello(_)
    ));
    let (method, params) = RpcRequest::Attach {
      target: target.parse().unwrap(),
      geometry: GeometryParams::of(Geometry::cells(80, 24)),
      until_exit,
      caps: None,
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
      protocol::codes,
      task::process_task::{RunCx, process_task},
    };

    let config = Arc::new(Config::make_default());
    let mut kernel = Kernel::new();
    let pc = kernel.context();
    // Never started: the task sits Idle, which `until_exit` treats as
    // already finished.
    let task = process_task(
      &RunCx::of(&config),
      pc.alloc_id(),
      Some(TaskKey::default_space(TaskPath::new("oneshot").unwrap())),
      serde_yaml::from_str("{cmd: [sh, -c, 'true']}").unwrap(),
    )
    .unwrap();
    kernel.register_task_registration(task).unwrap();
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
  async fn clients_without_atch2_are_served_the_first_dialect() {
    let config = Arc::new(Config::make_default());
    let (pc, _, kernel_handle) = console_kernel(&config);
    let ctx = ServerCtx::local(pc.clone(), config.clone());
    // A client from before the feature.
    let hello = Hello {
      features: vec![],
      ..local_hello()
    };
    let (mut sender, mut receiver, session) =
      attach_through(&ctx, hello, "@dekit/console", false, 64 * 1024, None)
        .await;
    assert!(next_out(&mut receiver).await.is_some());
    // Its resizes come in the first dialect and still work.
    sender
      .send_ctl(CtlMsg::Event(
        InputDialect::Input.input_event(&TermEvent::resize(100, 30)),
      ))
      .await
      .unwrap();
    assert!(wait_for(&mut receiver, b"Tasks").await);
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
    press(&mut sender, '?').await;
    let repainted = timeout(Duration::from_secs(2), next_out(&mut receiver))
      .await
      .unwrap();
    assert!(repainted.is_some());

    finish(pc, sender, receiver, session, kernel_handle).await;
  }

  #[tokio::test]
  async fn console_quit_modal_detaches_without_stopping_runner() {
    let config = Arc::new(Config::make_default());
    let (pc, _, kernel_handle) = console_kernel(&config);

    let (mut sender, mut receiver, session) =
      attach(&pc, &config, "@dekit/console", false).await;
    assert!(wait_for(&mut receiver, b"Tasks").await);

    press(&mut sender, 'q').await;
    assert!(wait_for(&mut receiver, b"leave everything running").await);
    press(&mut sender, 'd').await;

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

  /// Reads control messages until the bye.
  /// Reads frames until a `copy` event arrives, returning its text.
  async fn wait_for_copy(receiver: &mut ConnReceiver) -> Option<String> {
    timeout(Duration::from_secs(2), async {
      loop {
        match receiver.recv().await {
          Some(Ok(Msg::Ctl(CtlMsg::Event(event))))
            if event.name == EVENT_COPY =>
          {
            let copy: CopyEvent =
              serde_json::from_value(event.params).expect("copy event params");
            return Some(copy.text);
          }
          Some(Ok(_)) => (),
          Some(Err(_)) | None => return None,
        }
      }
    })
    .await
    .unwrap()
  }

  async fn wait_for_bye(receiver: &mut ConnReceiver) -> Option<Bye> {
    timeout(Duration::from_secs(2), async {
      loop {
        match receiver.recv().await {
          Some(Ok(Msg::Ctl(CtlMsg::Bye(bye)))) => return Some(bye),
          Some(Ok(_)) => (),
          Some(Err(_)) | None => return None,
        }
      }
    })
    .await
    .unwrap()
  }

  /// A config whose console sends the attachment to another runner on
  /// `F5`.
  fn switching_config() -> Arc<Config> {
    use crate::console::keymap::KeymapGroup;
    let mut config = Config::make_default();
    config.keymap.group_mut(KeymapGroup::Tasks).insert(
      Key::new(KeyCode::F(5), KeyMods::NONE),
      crate::console::action::Action::SwitchRunner {
        kind: "project".to_string(),
        root: "/home/me/other".to_string(),
      },
    );
    Arc::new(config)
  }

  async fn press_key(sender: &mut ConnSender, key: Key) {
    sender
      .send_ctl(CtlMsg::Event(
        InputDialect::Named.input_event(&TermEvent::Key(key)),
      ))
      .await
      .unwrap();
  }

  #[tokio::test]
  async fn console_switch_ends_the_session_with_the_next_runner() {
    let config = switching_config();
    let (pc, _, kernel_handle) = console_kernel(&config);

    let (mut sender, mut receiver, session) =
      attach(&pc, &config, "@dekit/console", false).await;
    assert!(wait_for(&mut receiver, b"Tasks").await);

    press_key(&mut sender, Key::new(KeyCode::F(5), KeyMods::NONE)).await;
    let bye = wait_for_bye(&mut receiver).await.unwrap();
    assert_eq!(bye.code, codes::QUIT);
    assert_eq!(
      bye.switch,
      Some(ByeSwitch {
        kind: "project".to_string(),
        root: "/home/me/other".to_string(),
      })
    );
    timeout(Duration::from_secs(2), session)
      .await
      .unwrap()
      .unwrap();
    drop(sender);
    drop(receiver);

    // The console and the runner go on for the next attachment.
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
        crate::project::Project::new(config.clone()),
        config.keymap.build(),
        Default::default(),
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
    let (_sender, _receiver, session) = attach_through(
      &ctx,
      local_hello(),
      "@dekit/console",
      false,
      256,
      Some(7),
    )
    .await;
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
      super::Attachment {
        geometry: Geometry::cells(80, 24),
        until_exit: false,
        caps: None,
      },
      ConnSender::new(server_write),
      ConnReceiver::new(server_read),
    ));
    let mut sender = ConnSender::new(client_write);
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
    // The sweep hides the cursor and paints the screen tinted as the
    // band crosses, then whole once it is done: a second of frames, so
    // this waits well past it.
    assert!(wait_for(&mut receiver, b"\x1b[?25l").await);
    assert!(wait_for(&mut receiver, b"48;2;").await);
    // The sweep paints cell by cell as the band passes; a resize, during
    // or after it, brings the next whole paint, which the painter must
    // be ready for.
    sender
      .send_ctl(CtlMsg::Event(
        InputDialect::Input.input_event(&TermEvent::resize(81, 24)),
      ))
      .await
      .unwrap();
    assert!(
      wait_for_within(&mut receiver, b"Tasks", Duration::from_secs(10)).await
    );

    finish(pc, sender, receiver, session, kernel_handle).await;
  }

  #[cfg(not(windows))]
  #[tokio::test]
  async fn screen_commands_drive_copy_mode_on_a_process() {
    use crate::{
      protocol::{ScreenCommand, ctl::EVENT_SCREEN, screen},
      task::process_task::{RunCx, process_task},
    };

    let config = Arc::new(Config::make_default());
    let mut kernel = Kernel::new();
    let pc = kernel.context();
    let mut task = process_task(
      &RunCx::of(&config),
      pc.alloc_id(),
      Some(TaskKey::default_space(TaskPath::new("echo").unwrap())),
      serde_yaml::from_str("{cmd: [sh, -c, 'echo hello-copy; sleep 30']}")
        .unwrap(),
    )
    .unwrap();
    task.def.pinned = true;
    kernel.register_task_registration(task).unwrap();
    let kernel_handle = tokio::spawn(kernel.run());

    let (mut sender, mut receiver, session) =
      attach(&pc, &config, "echo", false).await;
    assert!(wait_for(&mut receiver, b"hello-copy").await);

    // Select the first cell of the top row and yank it: a `copy` event
    // comes back for the client to put on the clipboard.
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
    assert!(wait_for_copy(&mut receiver).await.is_some());

    // No SIGCHLD waiter in unit tests: remove the task so quit can finish.
    pc.send(KernelCommand::Remove(
      crate::kernel::kernel_message::TaskSelector::all(),
      None,
    ));
    finish(pc, sender, receiver, session, kernel_handle).await;
  }
}
