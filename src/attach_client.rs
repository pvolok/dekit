use std::path::Path;

use anyhow::{Context, bail};
use tokio::io::AsyncWriteExt;

use crate::protocol::{
  ByeSwitch, ConnReceiver, ConnSender, CtlMsg, Event, Msg, Request, RpcRequest,
  client_handshake, codes, ctl::EVENT_INPUT,
};
use crate::runner::{RunnerKind, RunnerSpec};
use crate::target::Target;
use crate::term::TermEvent;
use crate::term::key::{Key, KeyEventKind};
use crate::term_driver::TermDriver;

/// How an attach session came to an end.
pub enum AttachEnd {
  /// The client or runner closed the session; the task lives on.
  Detached,
  /// `until_exit`: the attached task's execution finished, with the
  /// final state the runner reported in the bye.
  TaskExited(Option<crate::protocol::RpcState>),
  /// The console asked for a switch: attach to this runner's console
  /// next.
  Switch(RunnerSpec),
}

/// Attaches the local terminal to `target`'s screen until the session
/// ends.
pub async fn client_main(
  target: Target,
  until_exit: bool,
  mut sender: ConnSender,
  mut receiver: ConnReceiver,
) -> anyhow::Result<AttachEnd> {
  client_handshake(&mut sender, &mut receiver).await?;

  let mut term_driver = TermDriver::create()?;
  let result =
    client_loop(&mut term_driver, target, until_exit, sender, receiver).await;
  // Drop first: leaving the alternate screen restores the main screen,
  // so a foreground run's final output must be reprinted afterward to
  // survive on it.
  drop(term_driver);
  match result? {
    LoopEnd::Detached => Ok(AttachEnd::Detached),
    LoopEnd::Switch(runner) => Ok(AttachEnd::Switch(runner)),
    LoopEnd::TaskExited { state, screen } => {
      if let Some(screen) = screen {
        use std::io::Write;
        let mut stdout = std::io::stdout();
        let _ = stdout.write_all(screen.as_bytes());
        let _ = stdout.write_all(b"\n");
        let _ = stdout.flush();
      }
      Ok(AttachEnd::TaskExited(state))
    }
  }
}

enum LoopEnd {
  Detached,
  TaskExited {
    state: Option<crate::protocol::RpcState>,
    screen: Option<String>,
  },
  Switch(RunnerSpec),
}

/// The runner a `quit` bye sends the client on to, as a target.
fn switch_target(switch: ByeSwitch) -> anyhow::Result<RunnerSpec> {
  let Some(kind) = RunnerKind::from_name(&switch.kind) else {
    bail!(
      "runner asked for a switch to an unknown runner kind '{}'",
      switch.kind
    );
  };
  RunnerSpec::exact(kind, Path::new(&switch.root))
    .with_context(|| format!("cannot switch to runner at {}", switch.root))
}

async fn client_loop(
  term_driver: &mut TermDriver,
  target: Target,
  until_exit: bool,
  mut sender: ConnSender,
  mut receiver: ConnReceiver,
) -> anyhow::Result<LoopEnd> {
  let size = term_driver.size()?;
  let (method, params) = RpcRequest::Attach {
    target,
    width: size.width,
    height: size.height,
    until_exit,
  }
  .to_wire();
  sender
    .send_ctl(CtlMsg::Request(Request {
      id: 1,
      method,
      params,
    }))
    .await?;

  #[derive(Debug)]
  enum LocalEvent {
    ServerMsg(Option<anyhow::Result<Msg>>),
    TermEvent(std::io::Result<Option<TermEvent>>),
  }

  let mut stdout = tokio::io::stdout();

  loop {
    let event = tokio::select! {
      msg = receiver.recv() => LocalEvent::ServerMsg(msg),
      evt = term_driver.input() => LocalEvent::TermEvent(evt),
    };
    match event {
      LocalEvent::ServerMsg(msg) => match msg {
        Some(Ok(Msg::Out(bytes))) => {
          stdout.write_all(&bytes).await?;
          stdout.flush().await?;
        }
        Some(Ok(Msg::Ctl(msg))) => match msg {
          CtlMsg::Response(response) => {
            if let Some(error) = response.error {
              bail!("attach failed: {error}");
            }
          }
          CtlMsg::Bye(bye) => {
            if bye.code == codes::QUIT {
              if let Some(switch) = bye.switch {
                return Ok(LoopEnd::Switch(switch_target(switch)?));
              }
              break;
            }
            if bye.code == codes::TASK_EXITED {
              let _ = stdout.flush().await;
              return Ok(LoopEnd::TaskExited {
                state: bye.state,
                screen: bye.screen,
              });
            }
            bail!("runner closed the session: {}", bye.code);
          }
          msg @ (CtlMsg::Hello(_) | CtlMsg::Request(_) | CtlMsg::Event(_)) => {
            log::debug!("ignoring runner message {msg:?}");
          }
        },
        Some(Err(err)) => return Err(err),
        None => break,
      },
      LocalEvent::TermEvent(event) => match event? {
        Some(TermEvent::Key(Key {
          kind: KeyEventKind::Release,
          ..
        })) => (),
        Some(event) => {
          sender
            .send_ctl(CtlMsg::Event(Event {
              name: EVENT_INPUT.to_string(),
              params: serde_json::to_value(&event)?,
            }))
            .await?;
        }
        _ => break,
      },
    }
  }

  let _ = stdout.flush().await;

  Ok(LoopEnd::Detached)
}
