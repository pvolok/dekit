use serde::{Deserialize, Serialize};

/// Attachment-owned screen commands, the params of a `screen` event.
/// Scrolling and copy mode act on the attached screen as a whole (every
/// observer sees the same view); key input stays an `input` event.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "command", rename_all = "kebab-case")]
pub enum ScreenCommand {
  /// Positive `delta` scrolls up into history.
  Scroll {
    delta: i32,
    unit: ScrollUnit,
  },
  CopyEnter,
  CopyLeave,
  CopyMove {
    dir: CopyMove,
  },
  /// Anchor the selection at the cursor and start extending it.
  CopySelect,
  /// Copy the selection (delivered back as a `copy` event) and leave
  /// copy mode.
  CopyYank,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ScrollUnit {
  Line,
  HalfScreen,
  Screen,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CopyMove {
  Up,
  Down,
  Left,
  Right,
}

#[cfg(test)]
mod tests {
  use super::*;

  /// Append-only: these encodings are wire API.
  #[test]
  fn golden_encodings() {
    let samples = [
      (
        ScreenCommand::Scroll {
          delta: 3,
          unit: ScrollUnit::Line,
        },
        r#"{"command":"scroll","delta":3,"unit":"line"}"#,
      ),
      (
        ScreenCommand::Scroll {
          delta: -1,
          unit: ScrollUnit::HalfScreen,
        },
        r#"{"command":"scroll","delta":-1,"unit":"half-screen"}"#,
      ),
      (ScreenCommand::CopyEnter, r#"{"command":"copy-enter"}"#),
      (ScreenCommand::CopyLeave, r#"{"command":"copy-leave"}"#),
      (
        ScreenCommand::CopyMove { dir: CopyMove::Up },
        r#"{"command":"copy-move","dir":"up"}"#,
      ),
      (ScreenCommand::CopySelect, r#"{"command":"copy-select"}"#),
      (ScreenCommand::CopyYank, r#"{"command":"copy-yank"}"#),
    ];
    for (command, json) in samples {
      assert_eq!(serde_json::to_string(&command).unwrap(), json);
      assert_eq!(
        serde_json::from_str::<ScreenCommand>(json).unwrap(),
        command
      );
    }
  }
}
