use windows::Win32::{
  Foundation::HANDLE,
  System::Console::{
    CAPSLOCK_ON, CONSOLE_MODE, ENABLE_EXTENDED_FLAGS, ENABLE_MOUSE_INPUT,
    ENABLE_PROCESSED_OUTPUT, ENABLE_QUICK_EDIT_MODE,
    ENABLE_VIRTUAL_TERMINAL_PROCESSING, ENABLE_WINDOW_INPUT,
    FROM_LEFT_1ST_BUTTON_PRESSED, FROM_LEFT_2ND_BUTTON_PRESSED, GetConsoleMode,
    GetStdHandle, INPUT_RECORD, KEY_EVENT, KEY_EVENT_RECORD, MOUSE_EVENT,
    MOUSE_EVENT_RECORD, MOUSE_HWHEELED, MOUSE_MOVED, MOUSE_WHEELED,
    RIGHTMOST_BUTTON_PRESSED, SHIFT_PRESSED, STD_INPUT_HANDLE,
    STD_OUTPUT_HANDLE, SetConsoleMode, WINDOW_BUFFER_SIZE_EVENT,
    WINDOW_BUFFER_SIZE_RECORD,
  },
  UI::{
    Input::KeyboardAndMouse::{GetKeyboardLayout, ToUnicodeEx, VK_MENU},
    WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId},
  },
};

use crate::{
  error::ResultLogger,
  term::{
    key::{Key, KeyCode, KeyEventKind, KeyMods},
    mouse::{MouseButton, MouseEvent, MouseEventKind},
  },
};

use super::{input::EventDecoder, internal::InternalTermEvent};

pub struct WinVt {
  h_in: HANDLE,
  h_out: HANDLE,

  orig_stdin_mode: CONSOLE_MODE,
  orig_stdout_mode: CONSOLE_MODE,
}

impl WinVt {
  pub fn enable() -> anyhow::Result<Self> {
    unsafe {
      let h_in = GetStdHandle(STD_INPUT_HANDLE)?;
      let h_out = GetStdHandle(STD_OUTPUT_HANDLE)?;

      let mut orig_stdin_mode = std::mem::zeroed();
      GetConsoleMode(h_in, &mut orig_stdin_mode)?;
      SetConsoleMode(
        h_in,
        (ENABLE_MOUSE_INPUT | ENABLE_EXTENDED_FLAGS | ENABLE_WINDOW_INPUT)
          & !ENABLE_QUICK_EDIT_MODE,
      )?;

      let mut orig_stdout_mode = std::mem::zeroed();
      GetConsoleMode(h_out, &mut orig_stdout_mode)?;
      SetConsoleMode(
        h_out,
        orig_stdout_mode
          | ENABLE_PROCESSED_OUTPUT
          | ENABLE_VIRTUAL_TERMINAL_PROCESSING,
      )?;

      Ok(Self {
        h_in,
        h_out,
        orig_stdin_mode,
        orig_stdout_mode,
      })
    }
  }

  pub fn disable(&mut self) {
    unsafe {
      SetConsoleMode(self.h_in as _, self.orig_stdin_mode).log_ignore();
      SetConsoleMode(self.h_out as _, self.orig_stdout_mode).log_ignore();
    }
  }
}

fn decode_key_record<F: FnMut(InternalTermEvent)>(
  record: &KEY_EVENT_RECORD,
  f: &mut F,
) {
  use windows::Win32::UI::Input::KeyboardAndMouse::*;

  let uchar = unsafe { record.uChar.UnicodeChar };

  let modifiers = modifiers_from_ctrl_key_state(record.dwControlKeyState);
  let virtual_key_code = record.wVirtualKeyCode;

  // We normally ignore all key release events, but we will make an exception for an Alt key
  // release if it carries a u_char value, as this indicates an Alt code.
  if is_alt_code(record) {
    match uchar {
      surrogate @ 0xD800..=0xDFFF => {
        log::debug!("Unhandled surrogate key record: {surrogate:#06x}");
        return;
      }
      unicode_scalar_value => {
        // Unwrap is safe: We tested for surrogate values above and those are the only
        // u16 values that are invalid when directly interpreted as unicode scalar
        // values.
        let ch = std::char::from_u32(unicode_scalar_value as u32).unwrap();
        let key_code = KeyCode::Char(ch);
        let kind = if record.bKeyDown.as_bool() {
          KeyEventKind::Press
        } else {
          KeyEventKind::Release
        };
        let key_event = Key::new_with_kind(key_code, modifiers, kind);
        f(InternalTermEvent::Key(key_event));
        return;
      }
    }
  }

  // Don't generate events for numpad key presses when they're producing Alt codes.
  let is_numpad_numeric_key =
    (VK_NUMPAD0.0..=VK_NUMPAD9.0).contains(&virtual_key_code);
  let is_only_alt_modifier = modifiers.contains(KeyMods::ALT)
    && !modifiers.contains(KeyMods::SHIFT | KeyMods::CONTROL);
  if is_only_alt_modifier && is_numpad_numeric_key {
    return;
  }

  let parse_result = match VIRTUAL_KEY(virtual_key_code) {
    VK_SHIFT | VK_CONTROL | VK_MENU => None,
    VK_BACK => Some(KeyCode::Backspace),
    VK_ESCAPE => Some(KeyCode::Esc),
    VK_RETURN => Some(KeyCode::Enter),
    vk if (VK_F1.0..=VK_F24.0).contains(&vk.0) => {
      Some(KeyCode::F((record.wVirtualKeyCode - 111) as u8))
    }
    VK_LEFT => Some(KeyCode::Left),
    VK_UP => Some(KeyCode::Up),
    VK_RIGHT => Some(KeyCode::Right),
    VK_DOWN => Some(KeyCode::Down),
    VK_PRIOR => Some(KeyCode::PageUp),
    VK_NEXT => Some(KeyCode::PageDown),
    VK_HOME => Some(KeyCode::Home),
    VK_END => Some(KeyCode::End),
    VK_DELETE => Some(KeyCode::Delete),
    VK_INSERT => Some(KeyCode::Insert),
    VK_TAB => Some(KeyCode::Tab),
    _ => {
      match uchar {
        0x00..=0x1f => {
          // Some key combinations generate either no u_char value or generate control
          // codes. To deliver back a KeyCode::Char(...) event we want to know which
          // character the key normally maps to on the user's keyboard layout.
          // The keys that intentionally generate control codes (ESC, ENTER, TAB, etc.)
          // are handled by their virtual key codes above.
          get_char_for_key(record).map(KeyCode::Char)
        }
        surrogate @ 0xD800..=0xDFFF => {
          log::debug!("Unhandled surrogate key record: {surrogate:#06x}");
          return;
        }
        unicode_scalar_value => {
          // Unwrap is safe: We tested for surrogate values above and those are the only
          // u16 values that are invalid when directly interpreted as unicode scalar
          // values.
          let ch = std::char::from_u32(unicode_scalar_value as u32).unwrap();
          Some(KeyCode::Char(ch))
        }
      }
    }
  };

  if let Some(key_code) = parse_result {
    let kind = if record.bKeyDown.as_bool() {
      KeyEventKind::Press
    } else {
      KeyEventKind::Release
    };
    let key_event = Key::new_with_kind(key_code, modifiers, kind);
    f(InternalTermEvent::Key(key_event));
  }
}

fn is_alt_code(record: &KEY_EVENT_RECORD) -> bool {
  record.wVirtualKeyCode == VK_MENU.0
    && !record.bKeyDown.as_bool()
    && unsafe { record.uChar.UnicodeChar } != 0
}

enum CharCase {
  LowerCase,
  UpperCase,
}

fn try_ensure_char_case(ch: char, desired_case: CharCase) -> char {
  match desired_case {
    CharCase::LowerCase if ch.is_uppercase() => {
      let mut iter = ch.to_lowercase();
      // Unwrap is safe; iterator yields one or more chars.
      let ch_lower = iter.next().unwrap();
      if iter.next().is_none() { ch_lower } else { ch }
    }
    CharCase::UpperCase if ch.is_lowercase() => {
      let mut iter = ch.to_uppercase();
      // Unwrap is safe; iterator yields one or more chars.
      let ch_upper = iter.next().unwrap();
      if iter.next().is_none() { ch_upper } else { ch }
    }
    _ => ch,
  }
}

// Attempts to return the character for a key event accounting for the user's keyboard layout.
// The returned character (if any) is capitalized (if applicable) based on shift and capslock state.
// Returns None if the key doesn't map to a character or if it is a dead key.
// We use the *currently* active keyboard layout (if it can be determined). This layout may not
// correspond to the keyboard layout that was active when the user typed their input, since console
// applications get their input asynchronously from the terminal. By the time a console application
// can process a key input, the user may have changed the active layout. In this case, the character
// returned might not correspond to what the user expects, but there is no way for a console
// application to know what the keyboard layout actually was for a key event, so this is our best
// effort. If a console application processes input in a timely fashion, then it is unlikely that a
// user has time to change their keyboard layout before a key event is processed.
fn get_char_for_key(record: &KEY_EVENT_RECORD) -> Option<char> {
  let virtual_key_code = record.wVirtualKeyCode as u32;
  let virtual_scan_code = record.wVirtualScanCode as u32;
  let key_state = [0u8; 256];
  let mut utf16_buf = [0u16, 16];
  let dont_change_kernel_keyboard_state = 0x4;

  // Best-effort attempt at determining the currently active keyboard layout.
  // At the time of writing, this works for a console application running in Windows Terminal, but
  // doesn't work under a Conhost terminal. For Conhost, the window handle returned by
  // GetForegroundWindow() does not appear to actually be the foreground window which has the
  // keyboard layout associated with it (or perhaps it is, but also has special protection that
  // doesn't allow us to query it).
  // When this determination fails, the returned keyboard layout handle will be null, which is an
  // acceptable input for ToUnicodeEx, as that argument is optional. In this case ToUnicodeEx
  // appears to use the keyboard layout associated with the current thread, which will be the
  // layout that was inherited when the console application started (or possibly when the current
  // thread was spawned). This is then unfortunately not updated when the user changes their
  // keyboard layout in the terminal, but it's what we get.
  let active_keyboard_layout = unsafe {
    let foreground_window = GetForegroundWindow();
    let foreground_thread = GetWindowThreadProcessId(foreground_window, None);
    GetKeyboardLayout(foreground_thread)
  };

  let ret = unsafe {
    ToUnicodeEx(
      virtual_key_code,
      virtual_scan_code,
      &key_state,
      &mut utf16_buf,
      dont_change_kernel_keyboard_state,
      Some(active_keyboard_layout),
    )
  };

  // -1 indicates a dead key.
  // 0 indicates no character for this key.
  if ret < 1 {
    return None;
  }

  let mut ch_iter =
    std::char::decode_utf16(utf16_buf.into_iter().take(ret as usize));
  let mut ch = ch_iter.next()?.ok()?;
  if ch_iter.next().is_some() {
    // Key doesn't map to a single char.
    return None;
  }

  let is_shift_pressed = record.dwControlKeyState & SHIFT_PRESSED != 0;
  let is_capslock_on = record.dwControlKeyState & CAPSLOCK_ON != 0;
  let desired_case = if is_shift_pressed ^ is_capslock_on {
    CharCase::UpperCase
  } else {
    CharCase::LowerCase
  };
  ch = try_ensure_char_case(ch, desired_case);
  Some(ch)
}

struct Buttons(u32);

impl Buttons {
  pub fn none(&self) -> bool {
    self.0 == 0
  }

  pub fn left(&self) -> bool {
    self.0 & FROM_LEFT_1ST_BUTTON_PRESSED != 0
  }
  pub fn right(&self) -> bool {
    self.0 & RIGHTMOST_BUTTON_PRESSED != 0
  }
  pub fn middle(&self) -> bool {
    self.0 & FROM_LEFT_2ND_BUTTON_PRESSED != 0
  }
}

fn decode_mouse_record<F: FnMut(InternalTermEvent)>(
  decoder: &mut EventDecoder,
  event: &MOUSE_EVENT_RECORD,
  f: &mut F,
) {
  let prev = Buttons(decoder.windows_mouse_buttons);
  decoder.windows_mouse_buttons = event.dwButtonState;

  let btns = Buttons(event.dwButtonState);

  let kind = if event.dwEventFlags & MOUSE_MOVED != 0 {
    if btns.none() {
      MouseEventKind::Moved
    } else if btns.left() {
      MouseEventKind::Drag(MouseButton::Left)
    } else if btns.right() {
      MouseEventKind::Drag(MouseButton::Right)
    } else if btns.middle() {
      MouseEventKind::Drag(MouseButton::Middle)
    } else {
      // Mouse button not supported
      return;
    }
  } else if event.dwEventFlags & MOUSE_WHEELED != 0 {
    if (btns.0 as i32) < 0 {
      MouseEventKind::ScrollDown
    } else {
      MouseEventKind::ScrollUp
    }
  } else if event.dwEventFlags & MOUSE_HWHEELED != 0 {
    if (btns.0 as i32) < 0 {
      MouseEventKind::ScrollLeft
    } else {
      MouseEventKind::ScrollRight
    }
  } else {
    if btns.left() && !prev.left() {
      MouseEventKind::Down(MouseButton::Left)
    } else if !btns.left() && prev.left() {
      MouseEventKind::Up(MouseButton::Left)
    } else if btns.right() && !prev.right() {
      MouseEventKind::Down(MouseButton::Right)
    } else if !btns.right() && prev.right() {
      MouseEventKind::Up(MouseButton::Right)
    } else if btns.middle() && !prev.middle() {
      MouseEventKind::Down(MouseButton::Middle)
    } else if !btns.middle() && prev.middle() {
      MouseEventKind::Up(MouseButton::Middle)
    } else {
      // Mouse button not supported
      return;
    }
  };

  let mods = modifiers_from_ctrl_key_state(event.dwControlKeyState);

  f(InternalTermEvent::Mouse(MouseEvent {
    kind,
    x: event.dwMousePosition.X as i32,
    y: event.dwMousePosition.Y as i32,
    mods,
  }))
}

fn decode_resize_record<F: FnMut(InternalTermEvent)>(
  event: &WINDOW_BUFFER_SIZE_RECORD,
  f: &mut F,
) {
  f(InternalTermEvent::Resize(
    event.dwSize.X as u16,
    event.dwSize.Y as u16,
  ));
}

pub fn decode_input_records<F: FnMut(InternalTermEvent)>(
  decoder: &mut EventDecoder,
  records: &[INPUT_RECORD],
  f: &mut F,
) {
  for record in records {
    match record.EventType as u32 {
      KEY_EVENT => {
        let record = unsafe { &record.Event.KeyEvent };
        let uchar = unsafe { record.uChar.UnicodeChar };

        if !record.bKeyDown.as_bool() && !is_alt_code(record) {
          // Ignore release events.
          //
          // If we want to support release key events on Windows we need to
          // handle:
          // - if the key is part of escape sequence: consume both press+release
          //   events
          // - otherwise emit both press and release
        } else if (1..=0x7f).contains(&uchar)
          && record.dwControlKeyState == 0
          && record.bKeyDown.as_bool()
        {
          let ch = uchar as u8 as char;
          let mut buf = [0u8; 4];
          let bytes = ch.encode_utf8(&mut buf).as_bytes();
          for _ in 0..record.wRepeatCount {
            decoder.feed(bytes, &mut *f);
          }
        } else {
          decode_key_record(record, f);
        }
      }
      MOUSE_EVENT => {
        decode_mouse_record(decoder, unsafe { &record.Event.MouseEvent }, f)
      }
      WINDOW_BUFFER_SIZE_EVENT => {
        decode_resize_record(unsafe { &record.Event.WindowBufferSizeEvent }, f)
      }
      _ => {}
    }
  }

  // Process pending ESC character.
  decoder.flush(f);
}

fn modifiers_from_ctrl_key_state(state: u32) -> KeyMods {
  use windows::Win32::System::Console::*;

  let mut mods = KeyMods::NONE;

  if (state & (LEFT_ALT_PRESSED | RIGHT_ALT_PRESSED)) != 0 {
    mods |= KeyMods::ALT;
  }

  if (state & (LEFT_CTRL_PRESSED | RIGHT_CTRL_PRESSED)) != 0 {
    mods |= KeyMods::CONTROL;
  }

  if (state & SHIFT_PRESSED) != 0 {
    mods |= KeyMods::SHIFT;
  }

  // TODO: we could report caps lock, numlock and scrolllock

  mods
}
