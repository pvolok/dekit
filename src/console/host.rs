//! Where a console runs: on this process's terminal. The console draws
//! into a screen of its own and says when it changed; the host paints
//! that screen on the terminal, hands the console the terminal's input,
//! and puts what it copies on the clipboard.

use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc::UnboundedSender;

use crate::console::app::{AppMsg, HostMsg, Started};
use crate::term::{Painter, TermEvent, vt::emit};
use crate::term_driver::TermDriver;

/// How a console on this terminal ended.
pub enum Ended {
  /// The user left it: detached, or quit.
  Left,
  /// What it showed went away, with why.
  Closed(String),
}

/// Runs a console on this process's terminal until it ends. `start`
/// makes it once the terminal has said what it is.
pub async fn run_on_terminal(
  start: impl FnOnce(crate::term::Geometry, crate::protocol::TermCaps) -> Started,
) -> anyhow::Result<Ended> {
  let mut term = TermDriver::create()?;
  term.probe().await?;
  let started = start(term.geometry()?, *term.caps());
  let result = terminal_loop(&mut term, started).await;
  // Leaving the alternate screen first: a message then stays.
  drop(term);
  result
}

async fn terminal_loop(
  term: &mut TermDriver,
  Started {
    app,
    mut host,
    screen,
  }: Started,
) -> anyhow::Result<Ended> {
  let mut painter = Painter::new(*term.caps());
  let mut stdout = tokio::io::stdout();
  let remote = remote_terminal();
  let mut messages = Vec::new();
  loop {
    tokio::select! {
      n = host.recv_many(&mut messages, 64) => {
        if n == 0 {
          return Ok(Ended::Closed("the console stopped".to_string()));
        }
        let mut out = Vec::new();
        let mut paint = false;
        for msg in messages.drain(..) {
          match msg {
            HostMsg::Paint => paint = true,
            HostMsg::Copy(text) => copy(&mut out, text, remote),
            HostMsg::Exit(why) => {
              stdout.write_all(&out).await?;
              stdout.flush().await?;
              return Ok(match why {
                None => Ended::Left,
                Some(why) => Ended::Closed(why),
              });
            }
          }
        }
        if paint {
          painter.paint(&mut out, &screen.load());
        }
        if !out.is_empty() {
          stdout.write_all(&out).await?;
          stdout.flush().await?;
        }
      }
      event = term.input() => match event? {
        Some(event) => send(&app, event),
        None => return Ok(Ended::Left),
      },
    }
  }
}

fn send(app: &UnboundedSender<AppMsg>, event: TermEvent) {
  let _ = app.send(AppMsg::Input(event));
}

/// Puts `text` on the clipboard of the user's terminal, and on this
/// machine's when the terminal is on it.
fn copy(out: &mut Vec<u8>, text: String, remote: bool) {
  emit::osc52_copy(out, &text);
  if !remote {
    tokio::task::spawn_blocking(move || crate::clipboard::copy(&text));
  }
}

/// Whether this client's terminal is on another machine: the client
/// runs in an SSH session, so this machine's clipboard is not the
/// user's and only OSC 52 reaches them. sshd sets these; none is
/// forwarded to a further hop by default, so they describe this very
/// process.
pub fn remote_terminal() -> bool {
  ["SSH_CONNECTION", "SSH_TTY", "SSH_CLIENT"]
    .iter()
    .any(|var| std::env::var_os(var).is_some())
}
