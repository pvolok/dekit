use crate::term::{
  key::{Key, KeyCode, KeyEventKind, KeyEventState, KeyMods},
  mouse::{MouseButton, MouseEvent, MouseEventKind},
  vt::{Params, ScanMode, Scanner, Seq, keys},
};

use super::internal::InternalTermEvent as E;

/// An unterminated bracketed paste stops accumulating past this size.
const MAX_PASTE_BYTES: usize = 1024 * 1024;

/// Decodes the byte stream a terminal sends (keys, mouse, focus, replies)
/// into events. The other direction of the same protocol lives in
/// `term::vt::emit::key`.
pub struct EventDecoder {
  scanner: Scanner,
  /// Text being collected between bracketed paste markers.
  paste: Option<String>,
  /// The previous pasted byte was CR, so a following LF is the same newline.
  paste_cr: bool,
  #[cfg(windows)]
  pub windows_mouse_buttons: u32,
}

impl EventDecoder {
  pub fn new() -> Self {
    Self {
      scanner: Scanner::new(ScanMode::Input),
      paste: None,
      paste_cr: false,
      #[cfg(windows)]
      windows_mouse_buttons: 0,
    }
  }

  pub fn feed<F: FnMut(E)>(&mut self, input: &[u8], mut f: F) {
    let mut scanner = std::mem::take(&mut self.scanner);
    scanner.feed(input, |seq| self.decode(seq, &mut f));
    self.scanner = scanner;
  }

  /// Resolves a trailing lone ESC into an Esc key press. Call when no more
  /// input has arrived shortly after a read.
  pub fn flush<F: FnMut(E)>(&mut self, mut f: F) {
    let mut scanner = std::mem::take(&mut self.scanner);
    scanner.flush(|seq| self.decode(seq, &mut f));
    self.scanner = scanner;
  }

  #[cfg_attr(windows, allow(dead_code))]
  pub fn esc_pending(&self) -> bool {
    self.scanner.esc_pending()
  }

  fn decode<F: FnMut(E)>(&mut self, seq: Seq, f: &mut F) {
    if let Some(text) = &mut self.paste {
      let after_cr = std::mem::replace(&mut self.paste_cr, false);
      match seq {
        Seq::Text(run) => {
          if text.len() < MAX_PASTE_BYTES {
            text.push_str(run);
          }
        }
        Seq::Ctl(b'\r') => {
          text.push('\n');
          self.paste_cr = true;
        }
        Seq::Ctl(b'\n') => {
          if !after_cr {
            text.push('\n');
          }
        }
        Seq::Ctl(b'\t') => text.push('\t'),
        Seq::Csi(p)
          if p.prefix == 0 && p.final_ == b'~' && p.get(0, 0) == 201 =>
        {
          let text = self.paste.take().unwrap();
          f(E::Paste(text));
        }
        // Anything else inside a paste is dropped.
        _ => (),
      }
      return;
    }
    if let Seq::Csi(p) = &seq {
      if p.prefix == 0 && p.final_ == b'~' && p.get(0, 0) == 200 {
        self.paste = Some(String::new());
        return;
      }
    }
    decode_seq(seq, f);
  }
}

fn decode_seq<F: FnMut(E)>(seq: Seq, f: &mut F) {
  match seq {
    Seq::Text(run) => {
      for c in run.chars() {
        if let Some(key) = keys::char_key(c) {
          f(E::Key(key));
        }
      }
    }
    Seq::Ctl(0x1B) => f(E::Key(Key::new(KeyCode::Esc, KeyMods::NONE))),
    Seq::Ctl(b) => {
      if let Some(key) = keys::char_key(b as char) {
        f(E::Key(key));
      }
    }
    Seq::EscChar(c) => {
      if let Some(mut key) = keys::char_key(c) {
        key.mods |= KeyMods::ALT;
        f(E::Key(key));
      }
    }
    Seq::Ss3(final_) => match keys::ss3_key(final_) {
      Some(code) => f(E::Key(Key::new(code, KeyMods::NONE))),
      None => log::debug!("Unknown SS3 final: {final_:#x}"),
    },
    Seq::Csi(p) => decode_csi(p, f),
    Seq::X10Mouse(cb, cx, cy) => {
      // http://www.xfree86.org/current/ctlseqs.html#Mouse%20Tracking
      let b = cb.saturating_sub(32);
      let x = cx.saturating_sub(32).saturating_sub(1);
      let y = cy.saturating_sub(32).saturating_sub(1);
      match mouse_cb(b) {
        Ok((kind, mods)) => f(E::Mouse(MouseEvent {
          kind,
          mods,
          x: x as i32,
          y: y as i32,
        })),
        Err(()) => log::debug!("Unknown X10 mouse button: {cb:#x}"),
      }
    }
    Seq::Esc { inter, final_ } => {
      log::debug!("Unexpected escape on input: {inter:#x} {final_:#x}")
    }
    Seq::Osc(_) | Seq::Dcs(_) => (),
  }
}

fn decode_csi<F: FnMut(E)>(p: &Params, f: &mut F) {
  match p.final_ {
    b'A' | b'B' | b'C' | b'D' | b'F' | b'H' | b'P' | b'Q' | b'S'
      if p.prefix == 0 =>
    {
      let code = keys::final_key(p.final_).unwrap();
      let (mods, kind) = if p.len() >= 2 {
        let mods = keys::mask_mods(p.get16(1, 0).min(255) as u8);
        // The kitty event type is a colon subparam of the modifiers.
        let kind = if p.is_sub(2) { p.get16(2, 0) } else { 0 };
        (mods, keys::event_kind(kind.min(255) as u8))
      } else {
        (KeyMods::NONE, KeyEventKind::Press)
      };
      f(E::Key(Key::new_with_kind(code, mods, kind)));
    }
    b'c' if p.prefix == b'?' => f(E::PrimaryDeviceAttributes),
    b'Z' => f(E::Key(Key::new(KeyCode::Tab, KeyMods::SHIFT))),
    b'I' => f(E::FocusGained),
    b'O' => f(E::FocusLost),
    // Kitty keyboard protocol reply.
    b'u' if p.prefix == b'?' => {
      let flags = p.get(0, 0).min(255) as u8;
      f(E::ReplyKittyKeyboard(flags));
    }
    // CSI unicode-key-code:alternate-key-codes ; modifiers:event-type ;
    // text-as-codepoints u
    b'u' if p.prefix == 0 => match decode_csi_u(p) {
      Some(key) => f(E::Key(key)),
      None => log::debug!("Unhandled CSI-u: {p:?}"),
    },
    // SGR mouse: CSI < Cb ; Cx ; Cy (M or m)
    b'm' | b'M' if p.prefix == b'<' => {
      let (Some(b), Some(x), Some(y)) =
        (p.get_opt(0), p.get_opt(1), p.get_opt(2))
      else {
        log::debug!("Incomplete SGR mouse report: {p:?}");
        return;
      };
      let Ok((kind, mods)) = mouse_cb(b.min(255) as u8) else {
        log::debug!("Unknown SGR mouse button: {b}");
        return;
      };
      let kind = if p.final_ == b'm' {
        match kind {
          MouseEventKind::Down(button) => MouseEventKind::Up(button),
          other => other,
        }
      } else {
        kind
      };
      f(E::Mouse(MouseEvent {
        kind,
        mods,
        x: x.saturating_sub(1).min(i32::MAX as u32) as i32,
        y: y.saturating_sub(1).min(i32::MAX as u32) as i32,
      }));
    }
    // rxvt mouse: CSI Cb ; Cx ; Cy M
    b'M' if p.prefix == 0 => {
      let (Some(b), Some(x), Some(y)) =
        (p.get_opt(0), p.get_opt(1), p.get_opt(2))
      else {
        log::debug!("Incomplete rxvt mouse report: {p:?}");
        return;
      };
      let b = (b.min(255) as u8).saturating_sub(32);
      let Ok((kind, mods)) = mouse_cb(b) else {
        log::debug!("Unknown rxvt mouse button: {b}");
        return;
      };
      f(E::Mouse(MouseEvent {
        kind,
        mods,
        x: x.saturating_sub(1).min(i32::MAX as u32) as i32,
        y: y.saturating_sub(1).min(i32::MAX as u32) as i32,
      }));
    }
    // Cursor position report: CSI Cy ; Cx R
    b'R' if p.prefix == 0 => {
      let y = p.get16(0, 0).saturating_sub(1);
      let x = p.get16(1, 0).saturating_sub(1);
      f(E::CursorPos(x, y));
    }
    b'~' if p.prefix == 0 => {
      let Some(first) = p.get_opt(0) else {
        log::debug!("No key param in CSI ~");
        return;
      };
      let wire = p.get16(1, 0).min(255) as u8;
      let mods = keys::mask_mods(wire);
      let state = keys::mask_state(wire);

      let code = if first == 27 {
        // modifyOtherKeys
        let Some(code) = p.get_opt(2) else {
          log::debug!("Empty code in modifyOtherKeys");
          return;
        };
        match code {
          8 | 0x7f => KeyCode::Backspace,
          0x1b => KeyCode::Esc,
          9 => KeyCode::Tab,
          10 | 13 => KeyCode::Enter,
          code => match char::from_u32(code) {
            Some(c) => KeyCode::Char(c),
            None => return,
          },
        }
      } else {
        match keys::tilde_key(first.min(u16::MAX as u32) as u16) {
          Some(code) => code,
          None => {
            log::debug!("Wrong key param ({}) in CSI ~", first);
            return;
          }
        }
      };

      f(E::Key(Key {
        code,
        mods,
        kind: KeyEventKind::Press,
        state,
      }));
    }
    _ => log::debug!("Unknown CSI on input: {p:?}"),
  }
}

fn decode_csi_u(p: &Params) -> Option<Key> {
  let code = p.get(0, 0);
  // The shifted key is a colon subparam of the key code.
  let alt_code = if p.is_sub(1) { p.get_opt(1) } else { None };

  // The modifiers group starts at the first non-subparam after the code.
  let mut mods_at = 1;
  while p.is_sub(mods_at) {
    mods_at += 1;
  }
  let wire = p.get(mods_at, 0).min(255) as u8;
  let kind = if p.is_sub(mods_at + 1) {
    p.get(mods_at + 1, 1).min(255) as u8
  } else {
    1
  };

  let mut mods = keys::mask_mods(wire);
  let kind = keys::event_kind(kind);
  let state_from_mods = keys::mask_state(wire);

  let (mut keycode, state_from_keycode) =
    if let Some((code, state)) = keys::functional_key(code) {
      (code, state)
    } else if let Some(c) = char::from_u32(code) {
      (
        match c {
          '\x1B' => KeyCode::Esc,
          '\r' => KeyCode::Enter,
          '\t' => KeyCode::Tab,
          '\x7F' => KeyCode::Backspace,
          _ => KeyCode::Char(c),
        },
        KeyEventState::empty(),
      )
    } else {
      return None;
    };

  if let KeyCode::Modifier(modifier) = keycode {
    use crate::term::key::ModKeyCode::*;
    match modifier {
      LeftAlt | RightAlt => mods.set(KeyMods::ALT, true),
      LeftControl | RightControl => mods.set(KeyMods::CONTROL, true),
      LeftShift | RightShift => mods.set(KeyMods::SHIFT, true),
      LeftSuper | RightSuper => mods.set(KeyMods::SUPER, true),
      LeftHyper | RightHyper => mods.set(KeyMods::HYPER, true),
      LeftMeta | RightMeta => mods.set(KeyMods::META, true),
    }
  }

  // When the "report alternate keys" flag is enabled in the Kitty Keyboard
  // Protocol and the terminal sends a keyboard event containing shift, the
  // sequence will contain an additional codepoint separated by a ':'
  // character which contains the shifted character according to the
  // keyboard layout.
  if mods.contains(KeyMods::SHIFT) {
    if let Some(shifted) = alt_code.and_then(char::from_u32) {
      keycode = KeyCode::Char(shifted);
      mods.set(KeyMods::SHIFT, false);
    }
  }

  Some(Key {
    code: keycode,
    mods,
    kind,
    state: state_from_keycode | state_from_mods,
  })
}

/// Cb is the byte of a mouse input that contains the button being used, the
/// key modifiers being held and whether the mouse is dragging or not.
///
/// Bit layout of cb, from low to high:
///
/// - button number
/// - button number
/// - shift
/// - meta (alt)
/// - control
/// - mouse is dragging
/// - button number
/// - button number
fn mouse_cb(cb: u8) -> Result<(MouseEventKind, KeyMods), ()> {
  let button_number = (cb & 0b0000_0011) | ((cb & 0b1100_0000) >> 4);
  let dragging = cb & 0b0010_0000 == 0b0010_0000;

  let kind = match (button_number, dragging) {
    (0, false) => MouseEventKind::Down(MouseButton::Left),
    (1, false) => MouseEventKind::Down(MouseButton::Middle),
    (2, false) => MouseEventKind::Down(MouseButton::Right),
    (0, true) => MouseEventKind::Drag(MouseButton::Left),
    (1, true) => MouseEventKind::Drag(MouseButton::Middle),
    (2, true) => MouseEventKind::Drag(MouseButton::Right),
    (3, false) => MouseEventKind::Up(MouseButton::Left),
    (3, true) | (4, true) | (5, true) => MouseEventKind::Moved,
    (4, false) => MouseEventKind::ScrollUp,
    (5, false) => MouseEventKind::ScrollDown,
    (6, false) => MouseEventKind::ScrollLeft,
    (7, false) => MouseEventKind::ScrollRight,
    // We do not support other buttons.
    _ => return Err(()),
  };

  let mut mods = KeyMods::empty();
  if cb & 0b0000_0100 != 0 {
    mods |= KeyMods::SHIFT;
  }
  if cb & 0b0000_1000 != 0 {
    mods |= KeyMods::ALT;
  }
  if cb & 0b0001_0000 != 0 {
    mods |= KeyMods::CONTROL;
  }

  Ok((kind, mods))
}

#[cfg(test)]
mod tests {
  use super::*;

  fn decode(chunks: &[&[u8]]) -> Vec<E> {
    let mut decoder = EventDecoder::new();
    let mut events = Vec::new();
    for chunk in chunks {
      decoder.feed(chunk, |e| events.push(e));
    }
    decoder.flush(|e| events.push(e));
    events
  }

  fn keys_of(chunks: &[&[u8]]) -> Vec<Key> {
    decode(chunks)
      .into_iter()
      .map(|e| match e {
        E::Key(key) => key,
        other => panic!("expected key, got {other:?}"),
      })
      .collect()
  }

  fn key(spec: &str) -> Key {
    Key::parse(spec).unwrap()
  }

  #[test]
  fn plain_and_ctrl_chars() {
    assert_eq!(keys_of(&[b"ab"]), vec![key("<a>"), key("<b>")]);
    assert_eq!(keys_of(&[b"\x01"]), vec![key("<C-a>")]);
    assert_eq!(keys_of(&[b"\x00"]), vec![key("<C-Space>")]);
    assert_eq!(keys_of(&[b"\r"]), vec![key("<Enter>")]);
    assert_eq!(keys_of(&[b"\t"]), vec![key("<Tab>")]);
    assert_eq!(keys_of(&[b"\x7f"]), vec![key("<BS>")]);
    assert_eq!(keys_of(&["é".as_bytes()]), vec![key("<é>")]);
  }

  #[test]
  fn alt_chords_and_esc() {
    assert_eq!(keys_of(&[b"\x1bx"]), vec![key("<M-x>")]);
    assert_eq!(keys_of(&[b"\x1b\x01"]), vec![key("<C-M-a>")]);
    // Lone ESC resolves on flush.
    assert_eq!(keys_of(&[b"\x1b"]), vec![key("<Esc>")]);
    assert_eq!(keys_of(&[b"\x1b\x1b"]), vec![key("<Esc>")]);
    // ESC split before a chord char still decodes as Alt: the driver only
    // flushes when nothing follows the ESC for a while.
    assert_eq!(keys_of(&[b"\x1bq"]), vec![key("<M-q>")]);
  }

  #[test]
  fn arrows_and_function_keys() {
    assert_eq!(keys_of(&[b"\x1b[A"]), vec![key("<Up>")]);
    assert_eq!(keys_of(&[b"\x1bOB"]), vec![key("<Down>")]);
    assert_eq!(keys_of(&[b"\x1b[1;5C"]), vec![key("<C-Right>")]);
    assert_eq!(keys_of(&[b"\x1b[1;2H"]), vec![key("<S-Home>")]);
    assert_eq!(keys_of(&[b"\x1bOP"]), vec![key("<F1>")]);
    assert_eq!(keys_of(&[b"\x1b[15~"]), vec![key("<F5>")]);
    assert_eq!(keys_of(&[b"\x1b[24;5~"]), vec![key("<C-F12>")]);
    assert_eq!(keys_of(&[b"\x1b[3~"]), vec![key("<Del>")]);
    assert_eq!(keys_of(&[b"\x1b[Z"]), vec![key("<S-Tab>")]);
  }

  #[test]
  fn csi_u_keys() {
    assert_eq!(keys_of(&[b"\x1b[97;5u"]), vec![key("<C-a>")]);
    assert_eq!(keys_of(&[b"\x1b[105;5u"]), vec![key("<C-i>")]);
    // Release event kind.
    let released = keys_of(&[b"\x1b[97;1:3u"]);
    assert_eq!(released[0].kind, KeyEventKind::Release);
    // Shifted char via alternate code.
    assert_eq!(keys_of(&[b"\x1b[97:65;2u"]), vec![key("<A>")]);
    // Functional keys.
    assert_eq!(keys_of(&[b"\x1b[57376u"]), vec![key("<F13>")]);
    let keypad = keys_of(&[b"\x1b[57400u"]);
    assert_eq!(keypad[0].code, KeyCode::Char('1'));
    assert_eq!(keypad[0].state, KeyEventState::KEYPAD);
  }

  #[test]
  fn modify_other_keys() {
    assert_eq!(keys_of(&[b"\x1b[27;5;99~"]), vec![key("<C-c>")]);
    assert_eq!(keys_of(&[b"\x1b[27;5;13~"]), vec![key("<C-Enter>")]);
  }

  #[test]
  fn bracketed_paste() {
    assert_eq!(
      decode(&[b"\x1b[200~hello\rworld\x1b[201~"]),
      vec![E::Paste("hello\nworld".to_string())]
    );
    // Split across reads; keys around the paste still decode.
    assert_eq!(
      decode(&[b"a\x1b[200~pa", b"ste\x1b[2", b"01~b"]),
      vec![
        E::Key(key("<a>")),
        E::Paste("paste".to_string()),
        E::Key(key("<b>")),
      ]
    );
    // CRLF is one newline, also when split across reads; a lone LF or CR
    // is one too.
    assert_eq!(
      decode(&[b"\x1b[200~a\r\nb\r", b"\nc\n\rd\x1b[201~"]),
      vec![E::Paste("a\nb\nc\n\nd".to_string())]
    );
  }

  #[test]
  fn special_reports() {
    assert_eq!(decode(&[b"\x1b[?65;6c"]), vec![E::PrimaryDeviceAttributes]);
    assert_eq!(decode(&[b"\x1b[I"]), vec![E::FocusGained]);
    assert_eq!(decode(&[b"\x1b[O"]), vec![E::FocusLost]);
    assert_eq!(decode(&[b"\x1b[5;10R"]), vec![E::CursorPos(9, 4)]);
    // Multi-digit kitty flags parse fully.
    assert_eq!(decode(&[b"\x1b[?31u"]), vec![E::ReplyKittyKeyboard(31)]);
  }

  #[test]
  fn mouse_reports() {
    assert_eq!(
      decode(&[b"\x1b[<0;5;10M"]),
      vec![E::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        x: 4,
        y: 9,
        mods: KeyMods::NONE,
      })]
    );
    assert_eq!(
      decode(&[b"\x1b[<0;5;10m"]),
      vec![E::Mouse(MouseEvent {
        kind: MouseEventKind::Up(MouseButton::Left),
        x: 4,
        y: 9,
        mods: KeyMods::NONE,
      })]
    );
    assert_eq!(
      decode(&[b"\x1b[<69;3;4M"]),
      vec![E::Mouse(MouseEvent {
        kind: MouseEventKind::ScrollDown,
        x: 2,
        y: 3,
        mods: KeyMods::SHIFT,
      })]
    );
    // X10 encoding.
    assert_eq!(
      decode(&[b"\x1b[M \x25\x2a"]),
      vec![E::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        x: 4,
        y: 9,
        mods: KeyMods::NONE,
      })]
    );
    // Rxvt encoding.
    assert_eq!(
      decode(&[b"\x1b[32;5;10M"]),
      vec![E::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        x: 4,
        y: 9,
        mods: KeyMods::NONE,
      })]
    );
  }

  // Windows encodes keys as win32-input-mode records, which this decoder
  // does not read back (console input is decoded from KEY_EVENT_RECORDs).
  #[cfg(not(windows))]
  mod round_trip {
    use super::*;
    use crate::term::vt::emit::{self, KeyEncodeModes};
    use proptest::prelude::*;

    fn code_strategy() -> impl Strategy<Value = KeyCode> {
      prop_oneof![
        proptest::sample::select(vec![
          KeyCode::Up,
          KeyCode::Down,
          KeyCode::Left,
          KeyCode::Right,
          KeyCode::Home,
          KeyCode::End,
          KeyCode::PageUp,
          KeyCode::PageDown,
          KeyCode::Insert,
          KeyCode::Delete,
          KeyCode::Enter,
          KeyCode::Esc,
          KeyCode::Backspace,
          KeyCode::Tab,
        ]),
        (1u8..=12).prop_map(KeyCode::F),
        proptest::char::range('a', 'z').prop_map(KeyCode::Char),
        proptest::char::range('0', '9').prop_map(KeyCode::Char),
      ]
    }

    fn mods_strategy() -> impl Strategy<Value = KeyMods> {
      proptest::sample::select(vec![
        KeyMods::NONE,
        KeyMods::CONTROL,
        KeyMods::ALT,
        KeyMods::SHIFT,
        KeyMods::CONTROL | KeyMods::ALT,
      ])
    }

    /// Combinations whose wire encoding legitimately decodes to a
    /// different (aliased) key.
    fn aliased(key: &Key, csi_u: bool) -> bool {
      let ctrl = key.mods.contains(KeyMods::CONTROL);
      let shift = key.mods.contains(KeyMods::SHIFT);
      let alt = key.mods.contains(KeyMods::ALT);
      match key.code {
        KeyCode::Char(c) => {
          // Ctrl+i/j/m collide with Tab/Enter as raw control bytes;
          // CSI-u disambiguates i and m but not j.
          (ctrl
            && matches!(c, 'i' | 'j' | 'm')
            && !(csi_u && (c == 'i' || c == 'm')))
            // Ctrl+digit maps onto other control chars.
            || (ctrl && c.is_ascii_digit())
            // Shift is not represented for plain chars on the legacy
            // wire.
            || (shift && !csi_u)
        }
        // Alt+Esc encodes as ESC ESC, which reads back as plain Esc.
        KeyCode::Esc => alt || (!csi_u && (ctrl || shift)),
        // Ctrl/Shift+Enter/BS need CSI-u to be distinguishable.
        KeyCode::Enter | KeyCode::Backspace => !csi_u && (ctrl || shift),
        // Ctrl+Shift+Tab drops Ctrl, and the Alt prefix on top of the
        // Ctrl/Shift CSI forms reads back as ESC ESC.
        KeyCode::Tab => (ctrl && shift) || (alt && (ctrl || shift)),
        _ => false,
      }
    }

    proptest! {
      // What emit::key writes must decode back to the same key.
      #[test]
      fn key_round_trip(
        code in code_strategy(),
        mods in mods_strategy(),
        csi_u in proptest::bool::ANY,
      ) {
        let key = Key::new(code, mods);
        prop_assume!(!aliased(&key, csi_u));

        let modes = KeyEncodeModes {
          enable_csi_u_key_encoding: csi_u,
          application_cursor_keys: false,
          newline_mode: false,
        };
        let mut bytes = Vec::new();
        emit::key(&mut bytes, &key, modes);
        prop_assume!(!bytes.is_empty());

        let decoded = keys_of(&[&bytes]);
        prop_assert_eq!(decoded, vec![key]);
      }
    }
  }
}
