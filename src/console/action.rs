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
  FocusTasks,
  FocusTerm,
  Zoom,

  ShowCommandsMenu,
  NextTask,
  PrevTask,
  SelectTask {
    index: usize,
  },
  StartTask,
  StopTask,
  KillTask,
  VetoTask,
  RestartTask,
  RestartAll,
  RenameTask {
    name: String,
  },
  ForceRestartTask,
  ForceRestartAll,
  ShowAddTask,
  ShowRenameTask,
  AddTask {
    cmd: String,
    name: Option<String>,
  },
  DuplicateTask,
  ShowRemoveTask,
  RemoveTask {
    id: TaskId,
  },

  CloseCurrentModal,

  ScrollUp {
    #[serde(default = "default_scroll_n")]
    n: usize,
    unit: ScrollUnit,
  },
  ScrollDown {
    #[serde(default = "default_scroll_n")]
    n: usize,
    unit: ScrollUnit,
  },

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
      Action::FocusTasks => "Focus task list".to_string(),
      Action::FocusTerm => "Focus terminal".to_string(),
      Action::Zoom => "Zoom".to_string(),
      Action::ShowCommandsMenu => "All commands".to_string(),
      Action::NextTask => "Next".to_string(),
      Action::PrevTask => "Prev".to_string(),
      Action::SelectTask { index } => format!("Select task #{}", index),
      Action::StartTask => "Start".to_string(),
      Action::StopTask => "Stop".to_string(),
      Action::KillTask => "Kill".to_string(),
      Action::VetoTask => "Veto (down until started)".to_string(),
      Action::RestartTask => "Restart".to_string(),
      Action::RestartAll => "Restart all".to_string(),
      Action::RenameTask { name } => format!("Rename to \"{}\"", name),
      Action::ForceRestartTask => "Force restart".to_string(),
      Action::ForceRestartAll => "Force restart all".to_string(),
      Action::ShowAddTask => "New task dialog".to_string(),
      Action::ShowRenameTask => "Rename task dialog".to_string(),
      Action::AddTask { cmd, name: _ } => format!("New task `{}`", cmd),
      Action::DuplicateTask => "Duplicate current task".to_string(),
      Action::ShowRemoveTask => "Remove task dialog".to_string(),
      Action::RemoveTask { id } => format!("Remove task by id {}", id.0),
      Action::CloseCurrentModal => "Close current modal".to_string(),
      Action::ScrollUp { n, unit } => scroll_desc("up", *n, *unit),
      Action::ScrollDown { n, unit } => scroll_desc("down", *n, *unit),
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

fn default_scroll_n() -> usize {
  1
}

fn scroll_desc(dir: &str, n: usize, unit: ScrollUnit) -> String {
  match unit {
    ScrollUnit::Line => format!("Scroll {} {} {}", dir, n, lines_str(n)),
    ScrollUnit::HalfScreen => format!("Scroll {} half screen", dir),
    ScrollUnit::Screen => format!("Scroll {} screen", dir),
  }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ScrollUnit {
  Line,
  HalfScreen,
  Screen,
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn deserialize_scroll() {
    let a: Action =
      serde_yaml::from_str("c: scroll-up\nunit: half-screen\n").unwrap();
    assert_eq!(
      a,
      Action::ScrollUp {
        n: 1,
        unit: ScrollUnit::HalfScreen
      }
    );

    let a: Action =
      serde_yaml::from_str("c: scroll-down\nn: 3\nunit: line\n").unwrap();
    assert_eq!(
      a,
      Action::ScrollDown {
        n: 3,
        unit: ScrollUnit::Line
      }
    );
  }
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
