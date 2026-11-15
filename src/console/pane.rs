//! The terminal pane's screen: the shown task attached over a connection
//! of its own, its screen stream run through an emulator here, whose
//! frames the console draws. Input goes to the task as the attach
//! client's would; scrolling and copy mode as screen commands.

use bytes::Bytes;
use tokio::sync::mpsc;

use crate::console::link::Link;
use crate::kernel::task::TaskId;
use crate::protocol::{
  CtlMsg, Event, Msg, Request, RpcRequest, ScreenCommand, TermCaps, codes,
  ctl::{
    CopyEvent, EVENT_COPY, EVENT_SCREEN, EVENT_SCREEN_STATE,
    FEATURE_SCREEN_STATE, GeometryParams, InputDialect, ScreenStateEvent,
  },
};
use crate::target::Target;
use crate::term::{Frame, Geometry, Screen, ScreenHandle, TermEvent};

/// What the console asks of the pane's session.
pub enum PaneCtl {
  Input(TermEvent),
  Screen(ScreenCommand),
}

/// What the pane's session tells the console.
#[derive(Debug)]
pub enum PaneEvent {
  /// The screen shows something new.
  Render,
  State(ScreenStateEvent),
  /// Copied from the screen, for the clipboard.
  Copy(String),
  /// The session ended: why, when not for the task going.
  Ended(Option<String>),
}

pub struct Pane {
  pub handle: ScreenHandle,
  /// As the runner last said; copy mode is off and the mouse the
  /// screen's own until it says otherwise.
  pub state: ScreenStateEvent,
  /// The size it was last asked to be.
  pub asked: Option<Geometry>,
  ctl: mpsc::UnboundedSender<PaneCtl>,
}

impl Pane {
  /// Attaches to `task`, telling `events` what happens until the pane is
  /// dropped or the session ends.
  pub fn open(
    link: &Link,
    task: TaskId,
    geometry: Geometry,
    caps: TermCaps,
    events: impl Fn(PaneEvent) + Send + Sync + 'static,
  ) -> Pane {
    let handle = ScreenHandle::new(Frame::empty(geometry.size));
    let (ctl, rx) = mpsc::unbounded_channel();
    let session = link.session(vec![FEATURE_SCREEN_STATE.to_string()]);
    let shown = handle.clone();
    tokio::spawn(async move {
      let ended = run(session, task, geometry, caps, shown, rx, &events).await;
      events(PaneEvent::Ended(ended.err().map(|err| format!("{err:#}"))));
    });
    Pane {
      handle,
      state: ScreenStateEvent {
        width: geometry.size.width,
        height: geometry.size.height,
        copy: false,
        mouse: None,
      },
      asked: None,
      ctl,
    }
  }

  pub fn send(&self, ctl: PaneCtl) {
    let _ = self.ctl.send(ctl);
  }
}

async fn run(
  session: impl Future<
    Output = anyhow::Result<(
      crate::protocol::ConnSender,
      crate::protocol::ConnReceiver,
      crate::protocol::ctl::Hello,
    )>,
  >,
  task: TaskId,
  geometry: Geometry,
  caps: TermCaps,
  handle: ScreenHandle,
  mut ctl: mpsc::UnboundedReceiver<PaneCtl>,
  events: &impl Fn(PaneEvent),
) -> anyhow::Result<()> {
  let (mut sender, mut receiver, server) = session.await?;
  let dialect = InputDialect::of(&server);
  let (method, params) = RpcRequest::Attach {
    target: Target::Id(task),
    geometry: GeometryParams::of(geometry),
    until_exit: false,
    caps: Some(caps),
  }
  .to_wire();
  sender.queue_ctl(CtlMsg::Request(Request {
    id: 1,
    method,
    params,
  }))?;
  let mut screen = Screen::new(geometry.size, 0);
  let mut replies = Vec::new();
  loop {
    tokio::select! {
      msg = receiver.recv() => match msg {
        Some(Ok(Msg::Out(bytes))) => {
          paint(&mut screen, &bytes, &mut replies);
          if !screen.in_sync_update() {
            handle.publish(screen.frame());
            events(PaneEvent::Render);
          }
        }
        Some(Ok(Msg::Ctl(msg))) => match msg {
          CtlMsg::Response(response) => {
            if let Some(error) = response.error {
              return Err(error.into());
            }
          }
          CtlMsg::Event(event) if event.name == EVENT_SCREEN_STATE => {
            match serde_json::from_value::<ScreenStateEvent>(event.params) {
              Ok(state) => {
                screen.set_size(state.height, state.width);
                events(PaneEvent::State(state));
              }
              Err(err) => log::debug!("dropping screen-state: {err}"),
            }
          }
          CtlMsg::Event(event) if event.name == EVENT_COPY => {
            match serde_json::from_value::<CopyEvent>(event.params) {
              Ok(copy) => events(PaneEvent::Copy(copy.text)),
              Err(err) => log::debug!("dropping copy: {err}"),
            }
          }
          CtlMsg::Bye(bye) if bye.code == codes::QUIT => return Ok(()),
          CtlMsg::Bye(bye) => anyhow::bail!("the session ended: {}", bye.code),
          CtlMsg::Event(_) | CtlMsg::Hello(_) | CtlMsg::Request(_) => (),
        },
        Some(Err(err)) => return Err(err),
        None => anyhow::bail!("lost the connection to the runner"),
      },
      next = ctl.recv() => {
        let event = match next {
          Some(PaneCtl::Input(event)) => {
            if let TermEvent::Resize(geometry) = &event {
              // What it is painted at, until the runner says otherwise.
              screen.set_size(geometry.size.height, geometry.size.width);
            }
            dialect.input_event(&event)
          }
          Some(PaneCtl::Screen(command)) => Event {
            name: EVENT_SCREEN.to_string(),
            params: serde_json::to_value(command)?,
          },
          // The console moved on: the runner detaches it as it goes.
          None => return Ok(()),
        };
        sender.queue_ctl(CtlMsg::Event(event))?;
      }
      written = sender.flush(), if !sender.pending().is_empty() => written?,
    }
  }
}

/// Runs what the runner painted through the emulator. Its answers to
/// queries are dropped: the runner paints, it does not ask.
fn paint(
  screen: &mut Screen,
  bytes: &Bytes,
  replies: &mut Vec<crate::term::VtEvent>,
) {
  screen.process(bytes);
  screen.take_events(replies);
  replies.clear();
}
