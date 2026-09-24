use crate::term::{key::Key, mouse::MouseEvent};

#[derive(Clone, Debug, PartialEq, Eq)]
#[allow(dead_code)]
pub enum InternalTermEvent {
  Key(Key),
  Mouse(MouseEvent),
  Paste(String),
  Resize(u16, u16),
  FocusGained,
  FocusLost,
  CursorPos(u16, u16),
  PrimaryDeviceAttributes,

  ReplyKittyKeyboard(u8),
}

#[derive(Debug)]
pub enum KeyboardMode {
  Unknown,
  #[cfg_attr(windows, allow(dead_code))]
  ModifyOtherKeys,
  Kitty,
  #[cfg_attr(not(windows), allow(dead_code))]
  Win32,
}
