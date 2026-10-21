use std::fmt::Display;

use serde::{Deserialize, Serialize};

use crate::command::Command;
use crate::term::key::{Key, key_spec};

/// The built-in console's own messages: view state, modals, and verbs on
/// the current task. Keymap config spells them `{action: ..., ...}`, or
/// a bare name for argument-less ones.
#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(tag = "action", rename_all = "kebab-case")]
pub enum Action {
  Batch {
    cmds: Vec<Action>,
  },

  ShowQuit,
  /// Detach the attachment that pressed the key.
  Detach,
  /// Stop the runner; it keeps the tasks for its next start.
  Down,
  /// Stop the runner without saving the tasks.
  Quit,
  /// Execute a kernel command as-is.
  Command {
    command: Command,
  },

  ToggleFocus,
  FocusTasks,
  FocusTerm,
  Zoom,

  ShowCommandsMenu,
  /// The menu under the `dekit` badge in the header.
  ShowMainMenu,
  /// The runners on this machine: switch to a running one, go back to a
  /// recent project, or open a directory.
  ShowRunnerSelect {
    #[serde(default)]
    tab: RunnerTab,
  },
  /// Send the attachment that asked to the console of another runner.
  SwitchRunner {
    kind: String,
    root: String,
  },
  NextTask,
  PrevTask,
  /// Open the selected group, or step into it when it is open.
  Expand,
  /// Close the selected group, or step out to the group it is in.
  Collapse,
  /// Open, close, or toggle the groups `target` matches as a path or
  /// glob, or else the selected one or the one the selected task is in.
  Fold {
    op: FoldOp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    target: Option<String>,
  },
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
  /// The task dialog for the selected task. It replaced the rename
  /// dialog, whose name user keymaps may still bind.
  #[serde(alias = "show-rename-task")]
  ShowEditTask,
  /// The task dialog, filled from the selected task, for a copy of it.
  ShowDuplicateTask,
  AddTask {
    cmd: String,
    name: Option<String>,
  },
  /// A copy of the selected task, right after it, without a dialog.
  DuplicateTask,
  ShowRemoveTask,
  /// The selected task's menu, also on a right click.
  ShowTaskMenu,
  /// Swap the selected task or group with the one above it.
  MoveTaskUp,
  /// Swap the selected task or group with the one below it.
  MoveTaskDown,
  /// Copy the selected task's command.
  CopyTaskCommand,
  /// What the config on disk would change, to take or keep.
  ShowReload,
  /// What changed here and is not in the config, to write back.
  ShowWriteBack,
  /// Write the selected task, the runner's own, into the config.
  SaveToConfig,

  CloseCurrentModal,

  ScrollUp {
    #[serde(default = "default_scroll_n")]
    n: usize,
    #[serde(default = "default_scroll_unit")]
    unit: ScrollUnit,
  },
  ScrollDown {
    #[serde(default = "default_scroll_n")]
    n: usize,
    #[serde(default = "default_scroll_unit")]
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
  /// Dark or light; bound only in debug builds until themes settle.
  ToggleTheme,

  SendKey {
    #[serde(with = "key_spec")]
    key: Key,
  },
}

impl Action {
  /// The serde tag, e.g. `start-task`.
  pub fn name(&self) -> String {
    match serde_json::to_value(self) {
      Ok(serde_json::Value::Object(map)) => match map.get("action") {
        Some(serde_json::Value::String(name)) => name.clone(),
        _ => String::new(),
      },
      _ => String::new(),
    }
  }

  pub fn desc(&self) -> String {
    match self {
      Action::Batch { cmds: _ } => "Send multiple events".to_string(),
      Action::ShowQuit => "Quit".to_string(),
      Action::Detach => "Detach".to_string(),
      Action::Down => "Stop the runner and save".to_string(),
      Action::Quit => "Stop the runner without saving".to_string(),
      Action::Command { command } => format!("Run {:?}", command),
      Action::ToggleFocus => "Toggle focus".to_string(),
      Action::FocusTasks => "Focus task list".to_string(),
      Action::FocusTerm => "Focus terminal".to_string(),
      Action::Zoom => "Zoom".to_string(),
      Action::ShowCommandsMenu => "All commands".to_string(),
      Action::ShowMainMenu => "Main menu".to_string(),
      Action::ShowRunnerSelect { tab } => match tab {
        RunnerTab::Switch => "Switch project".to_string(),
        RunnerTab::Recent => "Recent projects".to_string(),
        RunnerTab::Open => "Open project".to_string(),
      },
      Action::SwitchRunner { root, .. } => format!("Switch to runner {root}"),
      Action::NextTask => "Next".to_string(),
      Action::PrevTask => "Prev".to_string(),
      Action::Expand => "Expand group".to_string(),
      Action::Collapse => "Collapse group".to_string(),
      Action::Fold { op, target } => {
        let verb = match op {
          FoldOp::Open => "Expand",
          FoldOp::Close => "Collapse",
          FoldOp::Toggle => "Toggle",
        };
        match target.as_deref() {
          None => format!("{verb} this group"),
          Some("**") => format!("{verb} all groups"),
          Some(target) => format!("{verb} {target}"),
        }
      }
      Action::SelectTask { index } => format!("Select task #{}", index),
      Action::StartTask => "Start".to_string(),
      Action::StopTask => "Stop".to_string(),
      Action::KillTask => "Kill".to_string(),
      Action::VetoTask => "Veto (stopped until started)".to_string(),
      Action::RestartTask => "Restart".to_string(),
      Action::RestartAll => "Restart all".to_string(),
      Action::RenameTask { name } => format!("Rename to \"{}\"", name),
      Action::ForceRestartTask => "Force restart".to_string(),
      Action::ForceRestartAll => "Force restart all".to_string(),
      Action::ShowAddTask => "New task".to_string(),
      Action::ShowEditTask => "Edit task".to_string(),
      Action::ShowDuplicateTask => "Duplicate task".to_string(),
      Action::AddTask { cmd, name: _ } => format!("New task `{}`", cmd),
      Action::DuplicateTask => "Duplicate task at once".to_string(),
      Action::ShowRemoveTask => "Remove task".to_string(),
      Action::ShowTaskMenu => "Task menu".to_string(),
      Action::MoveTaskUp => "Move up".to_string(),
      Action::MoveTaskDown => "Move down".to_string(),
      Action::CopyTaskCommand => "Copy command".to_string(),
      Action::ShowReload => "Reload the config".to_string(),
      Action::ShowWriteBack => "Write changes back to the config".to_string(),
      Action::SaveToConfig => "Save the task to the config".to_string(),
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
      Action::ToggleTheme => "Toggle dark/light theme".to_string(),
      Action::SendKey { key } => format!("Send {} key", key.spec()),
    }
  }
}

/// What `fold` does to the groups.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum FoldOp {
  Open,
  Close,
  Toggle,
}

/// A tab of the runners modal.
#[derive(
  Clone, Copy, Debug, Default, Deserialize, Eq, Hash, PartialEq, Serialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum RunnerTab {
  #[default]
  Switch,
  Recent,
  Open,
}

impl RunnerTab {
  pub const ALL: [RunnerTab; 3] =
    [RunnerTab::Switch, RunnerTab::Recent, RunnerTab::Open];

  pub fn label(self) -> &'static str {
    match self {
      RunnerTab::Switch => "switch",
      RunnerTab::Recent => "recent",
      RunnerTab::Open => "open",
    }
  }

  /// The tab after this one, wrapping around.
  pub fn next(self) -> Self {
    let i = Self::ALL.iter().position(|tab| *tab == self).unwrap_or(0);
    Self::ALL[(i + 1) % Self::ALL.len()]
  }
}

fn lines_str(n: usize) -> &'static str {
  if n == 1 { "line" } else { "lines" }
}

fn default_scroll_n() -> usize {
  1
}

fn default_scroll_unit() -> ScrollUnit {
  ScrollUnit::HalfScreen
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
  fn runner_select_defaults_to_the_switch_tab() {
    let a: Action =
      serde_yaml::from_str("action: show-runner-select\n").unwrap();
    assert_eq!(
      a,
      Action::ShowRunnerSelect {
        tab: RunnerTab::Switch
      }
    );
    let a: Action =
      serde_yaml::from_str("action: show-runner-select\ntab: open\n").unwrap();
    assert_eq!(
      a,
      Action::ShowRunnerSelect {
        tab: RunnerTab::Open
      }
    );
  }

  #[test]
  fn deserialize_scroll() {
    let a: Action = serde_yaml::from_str("action: scroll-up\n").unwrap();
    assert_eq!(
      a,
      Action::ScrollUp {
        n: 1,
        unit: ScrollUnit::HalfScreen
      }
    );

    let a: Action =
      serde_yaml::from_str("action: scroll-down\nn: 3\nunit: line\n").unwrap();
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
