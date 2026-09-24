//! The single place that writes terminal escape sequences.

use base64::Engine;

use crate::term::{
  attrs::Attrs,
  color::Color,
  common::CursorStyle,
  key::{Key, KeyCode, KeyMods},
  mouse::{MouseButton, MouseEvent, MouseEventKind},
  screen::MouseProtocolEncoding,
  vt::keys,
};

pub const SGR_RESET: &str = "\x1b[0m";
pub const SAVE_CURSOR: &str = "\x1b7";
pub const RESTORE_CURSOR: &str = "\x1b8";
pub const CLEAR_ALL: &str = "\x1b[2J";
pub const DA1_QUERY: &str = "\x1b[c";
// Kitty keyboard protocol is not queried on Windows (issue #215).
#[cfg_attr(windows, allow(dead_code))]
pub const KITTY_QUERY: &str = "\x1b[?u";
pub const KITTY_POP: &str = "\x1b[<1u";

/// DEC private modes used by dekit itself (`CSI ? {n} h/l`).
#[derive(Clone, Copy, Debug)]
pub enum DecMode {
  ShowCursor = 25,
  MousePressRelease = 1000,
  MouseButtonMotion = 1002,
  MouseAnyMotion = 1003,
  MouseRxvt = 1015,
  MouseSgr = 1006,
  AltScreen = 1049,
  BracketedPaste = 2004,
  Win32Input = 9001,
}

fn num(out: &mut Vec<u8>, n: i64) {
  let mut buf = itoa::Buffer::new();
  out.extend_from_slice(buf.format(n).as_bytes());
}

pub fn dec_set(out: &mut Vec<u8>, mode: DecMode) {
  out.extend_from_slice(b"\x1b[?");
  num(out, mode as i64);
  out.push(b'h');
}

pub fn dec_reset(out: &mut Vec<u8>, mode: DecMode) {
  out.extend_from_slice(b"\x1b[?");
  num(out, mode as i64);
  out.push(b'l');
}

/// Kitty keyboard protocol: push the given flags.
pub fn kitty_push(out: &mut Vec<u8>, flags: u8) {
  out.extend_from_slice(b"\x1b[>");
  num(out, flags as i64);
  out.push(b'u');
}

/// Reply to a kitty keyboard flags query.
pub fn kitty_flags_reply(out: &mut Vec<u8>, flags: u8) {
  out.extend_from_slice(b"\x1b[?");
  num(out, flags as i64);
  out.push(b'u');
}

/// xterm modifyOtherKeys level (0 disables).
pub fn modify_other_keys(out: &mut Vec<u8>, level: u8) {
  out.extend_from_slice(b"\x1b[>4;");
  num(out, level as i64);
  out.push(b'm');
}

/// Cursor position; takes 0-based coordinates.
pub fn cup(out: &mut Vec<u8>, row: u16, col: u16) {
  out.extend_from_slice(b"\x1b[");
  num(out, row as i64 + 1);
  out.push(b';');
  num(out, col as i64 + 1);
  out.push(b'H');
}

pub fn cursor_style(out: &mut Vec<u8>, style: CursorStyle) {
  out.extend_from_slice(b"\x1b[");
  num(out, style as i64);
  out.extend_from_slice(b" q");
}

/// Primary Device Attributes reply: VT500 with selective erase, ANSI color,
/// and clipboard access.
pub fn da1_reply(out: &mut Vec<u8>) {
  out.extend_from_slice(b"\x1b[?65;6;22;52c");
}

/// Cursor Position Report; takes 0-based coordinates.
pub fn cpr(out: &mut Vec<u8>, row: u16, col: u16) {
  out.extend_from_slice(b"\x1b[");
  num(out, row as i64 + 1);
  out.push(b';');
  num(out, col as i64 + 1);
  out.push(b'R');
}

/// The SGR transition from one attribute set to another. Writes nothing when
/// they are equal.
pub fn sgr(out: &mut Vec<u8>, from: Attrs, to: Attrs) {
  if from == to {
    return;
  }
  out.extend_from_slice(b"\x1b[");
  let mut first = true;
  let mut sep = |out: &mut Vec<u8>| {
    if first {
      first = false;
    } else {
      out.push(b';');
    }
  };
  if from.fgcolor != to.fgcolor {
    sep(out);
    match to.fgcolor {
      Color::Default => out.extend_from_slice(b"39"),
      Color::Idx(idx) => {
        out.extend_from_slice(b"38;5;");
        num(out, idx as i64);
      }
      Color::Rgb(r, g, b) => {
        out.extend_from_slice(b"38;2;");
        num(out, r as i64);
        out.push(b';');
        num(out, g as i64);
        out.push(b';');
        num(out, b as i64);
      }
    }
  }
  if from.bgcolor != to.bgcolor {
    sep(out);
    match to.bgcolor {
      Color::Default => out.extend_from_slice(b"49"),
      Color::Idx(idx) => {
        out.extend_from_slice(b"48;5;");
        num(out, idx as i64);
      }
      Color::Rgb(r, g, b) => {
        out.extend_from_slice(b"48;2;");
        num(out, r as i64);
        out.push(b';');
        num(out, g as i64);
        out.push(b';');
        num(out, b as i64);
      }
    }
  }
  if from.bold() != to.bold() {
    sep(out);
    num(out, if to.bold() { 1 } else { 22 });
  }
  if from.italic() != to.italic() {
    sep(out);
    num(out, if to.italic() { 3 } else { 23 });
  }
  if from.underline() != to.underline() {
    sep(out);
    num(out, if to.underline() { 4 } else { 24 });
  }
  if from.inverse() != to.inverse() {
    sep(out);
    num(out, if to.inverse() { 7 } else { 27 });
  }
  out.push(b'm');
}

/// Pasted text for a pty, wrapped in the bracketed paste markers when the
/// receiving program enabled them.
pub fn paste(out: &mut Vec<u8>, text: &str, bracketed: bool) {
  if bracketed {
    out.extend_from_slice(b"\x1b[200~");
  }
  out.extend_from_slice(text.as_bytes());
  if bracketed {
    out.extend_from_slice(b"\x1b[201~");
  }
}

/// OSC 2: set the window title.
pub fn osc_title(out: &mut Vec<u8>, title: &str) {
  out.extend_from_slice(b"\x1b]2;");
  out.extend_from_slice(title.as_bytes());
  out.push(0x07);
}

/// OSC 52: copy text to the clipboard through the outer terminal.
pub fn osc52_copy(out: &mut Vec<u8>, text: &str) {
  out.extend_from_slice(b"\x1b]52;;");
  let encoded = base64::engine::general_purpose::STANDARD.encode(text);
  out.extend_from_slice(encoded.as_bytes());
  out.push(0x07);
}

/// A mouse report in the encoding the receiving program enabled.
pub fn mouse(
  out: &mut Vec<u8>,
  event: &MouseEvent,
  encoding: MouseProtocolEncoding,
) {
  let mods = mouse_mods_bits(event.mods);
  match encoding {
    // SGR: CSI < Cb ; Cx ; Cy M/m
    MouseProtocolEncoding::Sgr => {
      let base: u8 = match event.kind {
        MouseEventKind::Down(btn) | MouseEventKind::Up(btn) => {
          mouse_button_bits(btn)
        }
        MouseEventKind::Drag(btn) => 32 + mouse_button_bits(btn),
        MouseEventKind::Moved => 35,
        MouseEventKind::ScrollUp => 64,
        MouseEventKind::ScrollDown => 65,
        MouseEventKind::ScrollLeft => 66,
        MouseEventKind::ScrollRight => 67,
      };
      out.extend_from_slice(b"\x1b[<");
      num(out, (base | mods) as i64);
      out.push(b';');
      num(out, event.x as i64 + 1);
      out.push(b';');
      num(out, event.y as i64 + 1);
      out.push(match event.kind {
        MouseEventKind::Up(_) => b'm',
        MouseEventKind::Down(_)
        | MouseEventKind::Drag(_)
        | MouseEventKind::Moved
        | MouseEventKind::ScrollDown
        | MouseEventKind::ScrollUp
        | MouseEventKind::ScrollLeft
        | MouseEventKind::ScrollRight => b'M',
      });
    }
    // Legacy byte encodings: CSI M Cb Cx Cy; a release loses its button.
    MouseProtocolEncoding::Default | MouseProtocolEncoding::Utf8 => {
      let base: u8 = match event.kind {
        MouseEventKind::Down(btn) => mouse_button_bits(btn),
        MouseEventKind::Up(_) => 3,
        MouseEventKind::Drag(btn) => 32 + mouse_button_bits(btn),
        MouseEventKind::Moved => 35,
        MouseEventKind::ScrollUp => 64,
        MouseEventKind::ScrollDown => 65,
        MouseEventKind::ScrollLeft => 66,
        MouseEventKind::ScrollRight => 67,
      };
      out.extend_from_slice(b"\x1b[M");
      out.push((base | mods) + 32);
      let x = event.x.max(0) as u32 + 33;
      let y = event.y.max(0) as u32 + 33;
      match encoding {
        MouseProtocolEncoding::Default => {
          out.push(x.min(255) as u8);
          out.push(y.min(255) as u8);
        }
        MouseProtocolEncoding::Utf8 => {
          mouse_coord_utf8(out, x);
          mouse_coord_utf8(out, y);
        }
        MouseProtocolEncoding::Sgr => unreachable!(),
      }
    }
  }
}

fn mouse_button_bits(btn: MouseButton) -> u8 {
  match btn {
    MouseButton::Left => 0,
    MouseButton::Middle => 1,
    MouseButton::Right => 2,
  }
}

fn mouse_mods_bits(mods: KeyMods) -> u8 {
  let mut bits = 0;
  if mods.contains(KeyMods::SHIFT) {
    bits |= 4;
  }
  if mods.contains(KeyMods::ALT) {
    bits |= 8;
  }
  if mods.contains(KeyMods::CONTROL) {
    bits |= 16;
  }
  bits
}

/// The 1005 extension encodes coordinates as UTF-8 (up to 2 bytes).
fn mouse_coord_utf8(out: &mut Vec<u8>, value: u32) {
  let value = value.min(2047);
  if value < 128 {
    out.push(value as u8);
  } else {
    out.push(0xC0 | (value >> 6) as u8);
    out.push(0x80 | (value & 0x3F) as u8);
  }
}

/// Terminal modes that change how a key is encoded for the pty.
#[derive(Debug, Clone, Copy, Default)]
pub struct KeyEncodeModes {
  pub enable_csi_u_key_encoding: bool,
  pub application_cursor_keys: bool,
  pub newline_mode: bool,
}

/// Writes the xterm-compatible byte sequence for this key and modifier
/// combination. Keys with no encoding (media keys, bare modifiers) write
/// nothing.
pub fn key(out: &mut Vec<u8>, key: &Key, modes: KeyEncodeModes) {
  use KeyCode::*;

  #[cfg(windows)]
  if key_win32(out, key) {
    return;
  }

  let code = key.code();
  let mods = key.mods();

  // Normalize the modifier state for Char's that are uppercase; remove
  // the SHIFT modifier so that reduce ambiguity below
  let mods = match code {
    Char(c)
      if (c.is_ascii_punctuation() || c.is_ascii_uppercase())
        && mods.contains(KeyMods::SHIFT) =>
    {
      mods.difference(KeyMods::SHIFT)
    }
    _ => mods,
  };

  // Normalize Backspace and Delete
  let code = match code {
    Char('\x7f') => KeyCode::Backspace,
    Char('\x08') => KeyCode::Delete,
    c => c,
  };

  match code {
    // Kitty "disambiguate escape codes": Esc and every ctrl/alt chord are
    // CSI u so the peer never has to guess at a bare ESC or ESC-prefix.
    Esc if modes.enable_csi_u_key_encoding => {
      csi_u_key(out, '\x1b', mods, true);
    }
    Char(c)
      if mods.intersects(KeyMods::CONTROL | KeyMods::ALT)
        && modes.enable_csi_u_key_encoding =>
    {
      csi_u_key(out, c, mods, true);
    }
    Enter | Backspace | Tab
      if mods.contains(KeyMods::ALT) && modes.enable_csi_u_key_encoding =>
    {
      let c = match code {
        Enter => '\r',
        Backspace => '\x7f',
        Tab => '\t',
        _ => unreachable!(),
      };
      csi_u_key(out, c, mods, true);
    }
    Char(c) if c.is_ascii_uppercase() && mods.contains(KeyMods::CONTROL) => {
      csi_u_key(out, c, mods, false);
    }

    Char(c)
      if mods.contains(KeyMods::CONTROL) && keys::ctrl_char(c).is_some() =>
    {
      let c = keys::ctrl_char(c).unwrap();
      if mods.contains(KeyMods::ALT) {
        out.push(0x1b);
      }
      push_char(out, c);
    }

    // When alt is pressed, send escape first to indicate to the peer that
    // ALT is pressed.  We do this only for ascii alnum characters because
    // eg: on macOS generates altgr style glyphs and keeps the ALT key
    // in the modifier set.  This confuses eg: zsh which then just displays
    // <fffffffff> as the input, so we want to avoid that.
    Char(c)
      if (c.is_ascii_alphanumeric() || c.is_ascii_punctuation())
        && mods.contains(KeyMods::ALT) =>
    {
      out.push(0x1b);
      push_char(out, c);
    }

    Enter | Esc | Backspace => {
      let c = match code {
        Enter => '\r',
        Esc => '\x1b',
        // Backspace sends the default VERASE which is confusingly
        // the DEL ascii codepoint
        Backspace => '\x7f',
        _ => unreachable!(),
      };
      if mods.contains(KeyMods::SHIFT) || mods.contains(KeyMods::CONTROL) {
        csi_u_key(out, c, mods, modes.enable_csi_u_key_encoding);
      } else {
        if mods.contains(KeyMods::ALT) {
          out.push(0x1b);
        }
        push_char(out, c);
        if modes.newline_mode && code == Enter {
          out.push(b'\n');
        }
      }
    }

    Tab => {
      if mods.contains(KeyMods::ALT) {
        out.push(0x1b);
      }
      let mods = mods & !KeyMods::ALT;
      if mods == KeyMods::CONTROL {
        out.extend_from_slice(b"\x1b[9;5u");
      } else if mods == KeyMods::CONTROL | KeyMods::SHIFT {
        out.extend_from_slice(b"\x1b[1;5Z");
      } else if mods == KeyMods::SHIFT {
        out.extend_from_slice(b"\x1b[Z");
      } else {
        out.push(b'\t');
      }
    }

    Char(c) => {
      if mods.is_empty() {
        push_char(out, c);
      } else {
        csi_u_key(out, c, mods, modes.enable_csi_u_key_encoding);
      }
    }

    Home | End | Up | Down | Right | Left => {
      let c = keys::cursor_key_final(code).unwrap();
      if mods.intersects(KeyMods::ALT | KeyMods::SHIFT | KeyMods::CONTROL) {
        out.extend_from_slice(b"\x1b[1;");
        num(out, keys::mods_mask(mods) as i64 + 1);
        out.push(c);
      } else if modes.application_cursor_keys {
        // Use SS3 in application mode.
        // Strict reading of DECCKM suggests that application_cursor_keys
        // only applies when DECANM and DECKPAM are active, but that seems
        // to break unmodified cursor keys in vim
        out.extend_from_slice(b"\x1bO");
        out.push(c);
      } else {
        out.extend_from_slice(b"\x1b[");
        out.push(c);
      }
    }

    F(n) if mods.is_empty() && n >= 1 && n < 5 => {
      // F1-F4 are encoded using SS3 if there are no modifiers
      out.extend_from_slice(b"\x1bO");
      out.push(b'P' + n - 1);
    }

    PageUp | PageDown | Insert | Delete | F(_) => {
      // Higher numbered F-keys plus modified keys are encoded using
      // `CSI n ~`.
      let Some(tilde) = keys::key_tilde(code) else {
        return;
      };
      out.extend_from_slice(b"\x1b[");
      num(out, tilde as i64);
      let mask = keys::mods_mask(mods);
      if mask != 0 {
        out.push(b';');
        num(out, mask as i64 + 1);
      }
      out.push(b'~');
    }

    Null | CapsLock | ScrollLock | NumLock | PrintScreen | Pause | Menu
    | KeypadBegin | Media(_) | Modifier(_) => (),
  }
}

fn push_char(out: &mut Vec<u8>, c: char) {
  let mut buf = [0u8; 4];
  out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
}

fn csi_u_key(out: &mut Vec<u8>, c: char, mods: KeyMods, csi_u: bool) {
  if csi_u {
    out.extend_from_slice(b"\x1b[");
    num(out, c as i64);
    out.push(b';');
    num(out, keys::mods_mask(mods) as i64 + 1);
    out.push(b'u');
  } else {
    let c = if mods.contains(KeyMods::CONTROL) {
      keys::ctrl_char(c).unwrap_or(c)
    } else {
      c
    };
    // Non-ascii glyphs (macOS Option keys) are sent alone; see the ALT
    // arm in `key` for why.
    if mods.contains(KeyMods::ALT) && c.is_ascii() {
      out.push(0x1b);
    }
    push_char(out, c);
  }
}

/// <https://github.com/microsoft/terminal/blob/main/doc/specs/%234999%20-%20Improved%20keyboard%20handling%20in%20Conpty.md>
/// Returns false when the key has no win32 encoding.
#[cfg(windows)]
fn key_win32(out: &mut Vec<u8>, key: &Key) -> bool {
  // <https://docs.microsoft.com/en-us/windows/console/key-event-record-str>
  // defines the dwControlKeyState values
  let mut control_key_state = 0;

  if key.mods().contains(KeyMods::SHIFT) {
    control_key_state |= windows::Win32::System::Console::SHIFT_PRESSED;
  }
  if key.mods().contains(KeyMods::ALT) {
    control_key_state |= windows::Win32::System::Console::LEFT_ALT_PRESSED;
  }
  if key.mods().contains(KeyMods::CONTROL) {
    control_key_state |= windows::Win32::System::Console::LEFT_CTRL_PRESSED;
  }

  let Some(vkey) = virtual_key_code(&key.code()) else {
    return false;
  };
  let uni = match key.code() {
    KeyCode::Char(c) => {
      let c = match c {
        // Delete key is transmitted as 0x0
        '\x7f' => '\x00',
        // Backspace key is transmitted as 0x8, 0x7f or 0x0
        '\x08' => {
          if key.mods().contains(KeyMods::CONTROL) {
            if key.mods().contains(KeyMods::ALT)
              || key.mods().contains(KeyMods::SHIFT)
            {
              '\x00'
            } else {
              '\x7f'
            }
          } else {
            '\x08'
          }
        }
        _ => c,
      };

      let c = if key.mods().contains(KeyMods::CONTROL) {
        // Ensure that we rewrite the unicode value to the ASCII CTRL
        // equivalent value.
        // <https://github.com/microsoft/terminal/issues/13134>
        keys::ctrl_char(c).unwrap_or(c)
      } else {
        c
      };
      c as u32
    }
    KeyCode::Backspace => 0x8,
    KeyCode::Enter => 0xd,
    KeyCode::Tab => 0x9,
    KeyCode::Delete => 0x7f,
    _ => 0,
  };

  let scan_code = 0;
  let key_down = 1;
  let repeat_count = 1;
  out.extend_from_slice(b"\x1b[");
  num(out, vkey.0 as i64);
  out.push(b';');
  num(out, scan_code);
  out.push(b';');
  num(out, uni as i64);
  out.push(b';');
  num(out, key_down);
  out.push(b';');
  num(out, control_key_state as i64);
  out.push(b';');
  num(out, repeat_count);
  out.push(b'_');
  true
}

/// <https://docs.microsoft.com/en-us/windows/win32/inputdev/virtual-key-codes>
#[cfg(windows)]
fn virtual_key_code(
  code: &KeyCode,
) -> Option<windows::Win32::UI::Input::KeyboardAndMouse::VIRTUAL_KEY> {
  use windows::Win32::UI::Input::KeyboardAndMouse::*;

  let code = match code {
    KeyCode::Char(c) => match c {
      '0'..='9' => VIRTUAL_KEY(*c as u16),
      'a'..='z' => VIRTUAL_KEY(c.to_ascii_uppercase() as u16),
      ' ' => VK_SPACE,
      '*' => VK_MULTIPLY,
      '+' => VK_ADD,
      ',' => VK_SEPARATOR,
      '-' => VK_SUBTRACT,
      '.' => VK_DECIMAL,
      '/' => VK_DIVIDE,
      _ => return None,
    },
    KeyCode::Backspace => VK_BACK,
    KeyCode::Enter => VK_RETURN,
    KeyCode::Left => VK_LEFT,
    KeyCode::Right => VK_RIGHT,
    KeyCode::Up => VK_UP,
    KeyCode::Down => VK_DOWN,
    KeyCode::Home => VK_HOME,
    KeyCode::End => VK_END,
    KeyCode::PageUp => VK_PRIOR,
    KeyCode::PageDown => VK_NEXT,
    KeyCode::Tab => VK_TAB,
    KeyCode::Delete => VK_DELETE,
    KeyCode::Insert => VK_INSERT,
    KeyCode::F(n) => match n {
      1..=24 => VIRTUAL_KEY(VK_F1.0 - 1 + *n as u16),
      _ => return None,
    },
    KeyCode::Esc => VK_ESCAPE,
    KeyCode::Null
    | KeyCode::CapsLock
    | KeyCode::ScrollLock
    | KeyCode::NumLock
    | KeyCode::PrintScreen
    | KeyCode::Pause
    | KeyCode::Menu
    | KeyCode::KeypadBegin
    | KeyCode::Media(_)
    | KeyCode::Modifier(_) => VIRTUAL_KEY(0),
  };

  Some(code)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn basics() {
    let mut out = Vec::new();
    dec_set(&mut out, DecMode::AltScreen);
    assert_eq!(out, b"\x1b[?1049h");

    out.clear();
    dec_reset(&mut out, DecMode::ShowCursor);
    assert_eq!(out, b"\x1b[?25l");

    out.clear();
    cup(&mut out, 0, 0);
    assert_eq!(out, b"\x1b[1;1H");

    out.clear();
    cursor_style(&mut out, CursorStyle::BlinkingBar);
    assert_eq!(out, b"\x1b[5 q");

    out.clear();
    kitty_push(&mut out, 15);
    assert_eq!(out, b"\x1b[>15u");

    out.clear();
    modify_other_keys(&mut out, 2);
    assert_eq!(out, b"\x1b[>4;2m");
  }

  // On Windows `key` always emits win32-input-mode (see `key_win32`).
  #[cfg(not(windows))]
  #[test]
  fn key_kitty_disambiguate() {
    fn enc(spec: &str, csi_u: bool) -> Vec<u8> {
      let mut out = Vec::new();
      let modes = KeyEncodeModes {
        enable_csi_u_key_encoding: csi_u,
        ..KeyEncodeModes::default()
      };
      key(&mut out, &Key::parse(spec).unwrap(), modes);
      out
    }
    // Legacy encoding when the program did not ask for the flag.
    assert_eq!(enc("<Esc>", false), b"\x1b");
    assert_eq!(enc("<C-c>", false), b"\x03");
    assert_eq!(enc("<M-j>", false), b"\x1bj");
    assert_eq!(enc("<C-Enter>", false), b"\r");
    // With the flag, anything a peer could mistake for an ESC prefix is
    // CSI u; plain text and unmodified special keys stay legacy.
    assert_eq!(enc("<Esc>", true), b"\x1b[27;1u");
    assert_eq!(enc("<M-Esc>", true), b"\x1b[27;3u");
    assert_eq!(enc("<C-c>", true), b"\x1b[99;5u");
    assert_eq!(enc("<M-j>", true), b"\x1b[106;3u");
    assert_eq!(enc("<C-M-a>", true), b"\x1b[97;7u");
    assert_eq!(enc("<C-Enter>", true), b"\x1b[13;5u");
    assert_eq!(enc("<M-Enter>", true), b"\x1b[13;3u");
    assert_eq!(enc("<M-BS>", true), b"\x1b[127;3u");
    assert_eq!(enc("<M-Tab>", true), b"\x1b[9;3u");
    assert_eq!(enc("<a>", true), b"a");
    assert_eq!(enc("<Enter>", true), b"\r");
    assert_eq!(enc("<Up>", true), b"\x1b[A");
  }

  #[test]
  fn sgr_transition() {
    let mut out = Vec::new();
    sgr(&mut out, Attrs::default(), Attrs::default());
    assert_eq!(out, b"");

    let colored = Attrs {
      fgcolor: Color::Idx(4),
      ..Default::default()
    };
    sgr(&mut out, Attrs::default(), colored);
    assert_eq!(out, b"\x1b[38;5;4m");

    out.clear();
    let mut bold = Attrs {
      fgcolor: Color::Rgb(1, 2, 3),
      bgcolor: Color::Idx(7),
      ..Default::default()
    };
    bold.set_bold(true);
    sgr(&mut out, colored, bold);
    assert_eq!(out, b"\x1b[38;2;1;2;3;48;5;7;1m");

    out.clear();
    sgr(&mut out, bold, Attrs::default());
    assert_eq!(out, b"\x1b[39;49;22m");
  }

  #[test]
  fn mouse_encodings() {
    let down = MouseEvent {
      kind: MouseEventKind::Down(MouseButton::Left),
      x: 4,
      y: 9,
      mods: KeyMods::NONE,
    };
    let mut out = Vec::new();
    mouse(&mut out, &down, MouseProtocolEncoding::Sgr);
    assert_eq!(out, b"\x1b[<0;5;10M");

    out.clear();
    mouse(
      &mut out,
      &MouseEvent {
        kind: MouseEventKind::Up(MouseButton::Right),
        x: 0,
        y: 0,
        mods: KeyMods::NONE,
      },
      MouseProtocolEncoding::Sgr,
    );
    assert_eq!(out, b"\x1b[<2;1;1m");

    out.clear();
    mouse(
      &mut out,
      &MouseEvent {
        kind: MouseEventKind::ScrollUp,
        x: 4,
        y: 9,
        mods: KeyMods::CONTROL,
      },
      MouseProtocolEncoding::Sgr,
    );
    assert_eq!(out, b"\x1b[<80;5;10M");

    out.clear();
    mouse(&mut out, &down, MouseProtocolEncoding::Default);
    assert_eq!(out, b"\x1b[M\x20\x25\x2a");

    // Legacy encoding clamps coordinates it cannot represent.
    out.clear();
    mouse(
      &mut out,
      &MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        x: 500,
        y: 9,
        mods: KeyMods::NONE,
      },
      MouseProtocolEncoding::Default,
    );
    assert_eq!(out, b"\x1b[M\x20\xff\x2a");

    // The UTF-8 extension covers larger coordinates.
    out.clear();
    mouse(
      &mut out,
      &MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        x: 500,
        y: 9,
        mods: KeyMods::NONE,
      },
      MouseProtocolEncoding::Utf8,
    );
    assert_eq!(out, b"\x1b[M\x20\xc8\x95\x2a");
  }

  #[test]
  fn paste_wrapping() {
    let mut out = Vec::new();
    paste(&mut out, "hi\nthere", false);
    assert_eq!(out, b"hi\nthere");

    out.clear();
    paste(&mut out, "hi", true);
    assert_eq!(out, b"\x1b[200~hi\x1b[201~");
  }
}
