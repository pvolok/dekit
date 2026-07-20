use serde::{Deserialize, Serialize};

use crate::console::action::{Action, CopyMove as ActionCopyMove, ScrollUnit};
use crate::console::server_message::ClientId;
use crate::kernel::task::TaskId;
use crate::term::key::{Key, key_spec};

#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(tag = "c", rename_all = "kebab-case")]
pub enum AppEvent {
  Batch {
    cmds: Vec<AppEvent>,
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
  TermProc,
  KillProc,
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

impl AppEvent {
  /// Translates the frozen mprocs format into the current console action.
  pub fn to_action(self) -> Action {
    match self {
      AppEvent::Batch { cmds } => Action::Batch {
        cmds: cmds.into_iter().map(AppEvent::to_action).collect(),
      },
      AppEvent::QuitOrAsk => Action::QuitOrAsk,
      AppEvent::Quit => Action::Quit,
      AppEvent::ForceQuit => Action::ForceQuit,
      AppEvent::Detach { client_id } => Action::Detach { client_id },
      AppEvent::ToggleFocus => Action::ToggleFocus,
      AppEvent::FocusProcs => Action::FocusTasks,
      AppEvent::FocusTerm => Action::FocusTerm,
      AppEvent::Zoom => Action::Zoom,
      AppEvent::ShowCommandsMenu => Action::ShowCommandsMenu,
      AppEvent::NextProc => Action::NextTask,
      AppEvent::PrevProc => Action::PrevTask,
      AppEvent::SelectProc { index } => Action::SelectTask { index },
      AppEvent::StartProc => Action::StartTask,
      AppEvent::TermProc => Action::StopTask,
      AppEvent::KillProc => Action::KillTask,
      AppEvent::RestartProc => Action::RestartTask,
      AppEvent::RestartAll => Action::RestartAll,
      AppEvent::RenameProc { name } => Action::RenameTask { name },
      AppEvent::ForceRestartProc => Action::ForceRestartTask,
      AppEvent::ForceRestartAll => Action::ForceRestartAll,
      AppEvent::ShowAddProc => Action::ShowAddTask,
      AppEvent::ShowRenameProc => Action::ShowRenameTask,
      AppEvent::AddProc { cmd, name } => Action::AddTask { cmd, name },
      AppEvent::DuplicateProc => Action::DuplicateTask,
      AppEvent::ShowRemoveProc => Action::ShowRemoveTask,
      AppEvent::RemoveProc { id } => Action::RemoveTask { id },
      AppEvent::CloseCurrentModal => Action::CloseCurrentModal,
      AppEvent::ScrollDownLines { n } => Action::ScrollDown {
        n,
        unit: ScrollUnit::Line,
      },
      AppEvent::ScrollUpLines { n } => Action::ScrollUp {
        n,
        unit: ScrollUnit::Line,
      },
      AppEvent::ScrollDown => Action::ScrollDown {
        n: 1,
        unit: ScrollUnit::HalfScreen,
      },
      AppEvent::ScrollUp => Action::ScrollUp {
        n: 1,
        unit: ScrollUnit::HalfScreen,
      },
      AppEvent::CopyModeEnter => Action::CopyModeEnter,
      AppEvent::CopyModeLeave => Action::CopyModeLeave,
      AppEvent::CopyModeMove { dir } => Action::CopyModeMove {
        dir: match dir {
          CopyMove::Up => ActionCopyMove::Up,
          CopyMove::Right => ActionCopyMove::Right,
          CopyMove::Left => ActionCopyMove::Left,
          CopyMove::Down => ActionCopyMove::Down,
        },
      },
      AppEvent::CopyModeEnd => Action::CopyModeEnd,
      AppEvent::CopyModeCopy => Action::CopyModeCopy,
      AppEvent::ToggleKeymapWindow => Action::ToggleKeymapWindow,
      AppEvent::SendKey { key } => Action::SendKey { key },
    }
  }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub enum CopyMove {
  Up,
  Right,
  Left,
  Down,
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn serialize() {
    assert_eq!(
      serde_yaml::to_string(&AppEvent::ForceQuit).unwrap(),
      "c: force-quit\n"
    );

    assert_eq!(
      serde_yaml::to_string(&AppEvent::SendKey {
        key: Key::parse("<c-a>").unwrap()
      })
      .unwrap(),
      "c: send-key\nkey: <C-a>\n"
    );
  }

  #[test]
  fn deserialize_send_key() {
    let ev: AppEvent =
      serde_yaml::from_str("c: send-key\nkey: <C-a>\n").unwrap();
    assert_eq!(
      ev,
      AppEvent::SendKey {
        key: Key::parse("<C-a>").unwrap()
      }
    );
  }

  #[test]
  fn old_scroll_spellings_convert() {
    let ev: AppEvent = serde_yaml::from_str("c: scroll-up\n").unwrap();
    assert_eq!(
      ev.to_action(),
      Action::ScrollUp {
        n: 1,
        unit: ScrollUnit::HalfScreen
      }
    );

    let ev: AppEvent =
      serde_yaml::from_str("c: scroll-down-lines\nn: 3\n").unwrap();
    assert_eq!(
      ev.to_action(),
      Action::ScrollDown {
        n: 3,
        unit: ScrollUnit::Line
      }
    );
  }
}
