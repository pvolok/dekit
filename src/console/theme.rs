use crate::term::Color;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl From<Rgb> for Color {
  fn from(Rgb(r, g, b): Rgb) -> Self {
    Color::Rgb(r, g, b)
  }
}

/// The list paints its own background, so it reads the same on light and
/// dark terminals. Group counts: `failed` if any failed, `up` if all are up,
/// else `down`.
pub struct TaskListTheme {
  pub bg: Rgb,
  pub text: Rgb,
  /// The `/` or `/…` after a group name.
  pub mark: Rgb,
  pub selected_bg: Rgb,
  pub up: Rgb,
  pub down: Rgb,
  /// Ended with code 0: DONE, DOWN (0).
  pub done: Rgb,
  /// FAILED, DOWN (code), NOT READY.
  pub failed: Rgb,
}

impl Default for TaskListTheme {
  fn default() -> Self {
    TaskListTheme {
      bg: Rgb(0x16, 0x1b, 0x22),
      text: Rgb(0xd1, 0xd7, 0xe0),
      mark: Rgb(0x91, 0x98, 0xa1),
      selected_bg: Rgb(0x42, 0x4a, 0x57),
      up: Rgb(0x56, 0xd3, 0x64),
      down: Rgb(0xa3, 0xac, 0xb8),
      done: Rgb(0x79, 0xc0, 0xff),
      failed: Rgb(0xff, 0xa1, 0x98),
    }
  }
}
