use std::fmt::Display;

use serde::{Deserialize, Serialize};

use crate::console::server_message::ClientId;
use crate::kernel::task::TaskId;
use crate::term::key::{Key, key_spec};

#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(tag = "c", rename_all = "kebab-case")]
pub enum Action {
  Batch {
    cmds: Vec<Action>,
  },

  QuitOrAsk,
  Quit,
  ForceQuit,
  Detach {
    client_id: ClientId,
  },

  ToggleFocus,
  FocusProcs,
  FocusTerm,
  Zoom,

  ShowCommandsMenu,
  NextProc,
  PrevProc,
  SelectProc {
    index: usize,
  },
  StartProc,
  StopProc,
  KillProc,
  VetoProc,
  RestartProc,
  RestartAll,
  RenameProc {
    name: String,
  },
  ForceRestartProc,
  ForceRestartAll,
  ShowAddProc,
  ShowRenameProc,
  AddProc {
    cmd: String,
    name: Option<String>,
  },
  DuplicateProc,
  ShowRemoveProc,
  RemoveProc {
    id: TaskId,
  },

  CloseCurrentModal,

  ScrollDownLines {
    n: usize,
  },
  ScrollUpLines {
    n: usize,
  },
  ScrollDown,
  ScrollUp,

  CopyModeEnter,
  CopyModeLeave,
  CopyModeMove {
    dir: CopyMove,
  },
  CopyModeEnd,
  CopyModeCopy,
  ToggleKeymapWindow,

  SendKey {
    #[serde(with = "key_spec")]
    key: Key,
  },
}

impl Action {
  pub fn desc(&self) -> String {
    match self {
      Action::Batch { cmds: _ } => "Send multiple events".to_string(),
      Action::QuitOrAsk => "Quit".to_string(),
      Action::Quit => "Quit".to_string(),
      Action::ForceQuit => "Force quit".to_string(),
      Action::Detach { client_id } => {
        format!("Detach client #{:?}", client_id)
      }
      Action::ToggleFocus => "Toggle focus".to_string(),
      Action::FocusProcs => "Focus process list".to_string(),
      Action::FocusTerm => "Focus terminal".to_string(),
      Action::Zoom => "Zoom".to_string(),
      Action::ShowCommandsMenu => "All commands".to_string(),
      Action::NextProc => "Next".to_string(),
      Action::PrevProc => "Prev".to_string(),
      Action::SelectProc { index } => format!("Select process #{}", index),
      Action::StartProc => "Start".to_string(),
      Action::StopProc => "Stop".to_string(),
      Action::KillProc => "Kill".to_string(),
      Action::VetoProc => "Veto (down until started)".to_string(),
      Action::RestartProc => "Restart".to_string(),
      Action::RestartAll => "Restart all".to_string(),
      Action::RenameProc { name } => format!("Rename to \"{}\"", name),
      Action::ForceRestartProc => "Force restart".to_string(),
      Action::ForceRestartAll => "Force restart all".to_string(),
      Action::ShowAddProc => "New process dialog".to_string(),
      Action::ShowRenameProc => "Rename process dialog".to_string(),
      Action::AddProc { cmd, name: _ } => format!("New process `{}`", cmd),
      Action::DuplicateProc => "Duplicate current process".to_string(),
      Action::ShowRemoveProc => "Remove process dialog".to_string(),
      Action::RemoveProc { id } => format!("Remove process by id {}", id.0),
      Action::CloseCurrentModal => "Close current modal".to_string(),
      Action::ScrollDownLines { n } => {
        format!("Scroll down {} {}", n, lines_str(*n))
      }
      Action::ScrollUpLines { n } => {
        format!("Scroll up {} {}", n, lines_str(*n))
      }
      Action::ScrollDown => "Scroll down".to_string(),
      Action::ScrollUp => "Scroll up".to_string(),
      Action::CopyModeEnter => "Enter copy mode".to_string(),
      Action::CopyModeLeave => "Leave copy mode".to_string(),
      Action::CopyModeMove { dir } => {
        format!("Move selection cursor {}", dir)
      }
      Action::CopyModeEnd => "Select end position".to_string(),
      Action::CopyModeCopy => "Copy selected text".to_string(),
      Action::ToggleKeymapWindow => "Toggle help".to_string(),
      Action::SendKey { key } => format!("Send {} key", key.spec()),
    }
  }
}

fn lines_str(n: usize) -> &'static str {
  if n == 1 { "line" } else { "lines" }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub enum CopyMove {
  Up,
  Right,
  Left,
  Down,
}

impl Display for CopyMove {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    let str = match self {
      CopyMove::Up => "up",
      CopyMove::Right => "right",
      CopyMove::Left => "left",
      CopyMove::Down => "down",
    };
    f.write_str(str)
  }
}
