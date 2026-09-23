use std::{fmt::Debug, str};

use super::{
  attrs::Attrs,
  color::Color,
  common::{CursorStyle, Size},
  vt::{Params, Scanner, Seq},
};
use unicode_width::UnicodeWidthChar as _;

const MODE_APPLICATION_KEYPAD: u8 = 0b0000_0001;
const MODE_APPLICATION_CURSOR: u8 = 0b0000_0010;
const MODE_HIDE_CURSOR: u8 = 0b0000_0100;
const MODE_ALTERNATE_SCREEN: u8 = 0b0000_1000;
const MODE_BRACKETED_PASTE: u8 = 0b0001_0000;

/// Kitty keyboard protocol flags implemented by the input encoder.
///
/// TODO: We currently support only "disambiguate escape codes".
pub const KITTY_KEYBOARD_SUPPORTED_FLAGS: u8 = 0b0000_0001;

#[derive(Clone, Debug)]
pub enum CharSet {
  Ascii,
  Uk,
  DecLineDrawing,
}

/// The xterm mouse handling mode currently in use.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum MouseProtocolMode {
  /// Mouse handling is disabled.
  None,

  /// Mouse button events should be reported on button press. Also known as
  /// X10 mouse mode.
  /// On/off: `CSI ? 9 h` / `CSI ? 9 l`
  Press,

  /// Mouse button events should be reported on button press and release.
  /// Also known as VT200 mouse mode.
  /// On/off: `CSI ? 1000 h` / `CSI ? 1000 l`
  PressRelease,

  /// On/off: `CSI ? 1001 h` / `CSI ? 1001 l`
  // Highlight,
  //
  /// Mouse button events should be reported on button press and release, as
  /// well as when the mouse moves between cells while a button is held
  /// down.
  /// On/off: `CSI ? 1002 h` / `CSI ? 1002 l`
  ButtonMotion,

  /// Mouse button events should be reported on button press and release,
  /// and mouse motion events should be reported when the mouse moves
  /// between cells regardless of whether a button is held down or not.
  /// On/off: `CSI ? 1003 h` / `CSI ? 1003 l`
  AnyMotion,
  // DecLocator,
}

impl Default for MouseProtocolMode {
  fn default() -> Self {
    Self::None
  }
}

/// The encoding to use for the enabled `MouseProtocolMode`.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum MouseProtocolEncoding {
  /// Default single-printable-byte encoding.
  #[default]
  Default,

  /// UTF-8-based encoding.
  Utf8,

  /// SGR-like encoding.
  Sgr,
  // Urxvt,
}

/// Represents the overall terminal state.
#[derive(Clone, Debug)]
pub struct Screen {
  scanner: Scanner,

  grid: super::grid::Grid,
  alternate_grid: super::grid::Grid,

  attrs: super::attrs::Attrs,
  saved_attrs: super::attrs::Attrs,

  modes: u8,
  mouse_protocol_mode: MouseProtocolMode,
  mouse_protocol_encoding: MouseProtocolEncoding,

  g0: CharSet,
  g1: CharSet,
  shift_out: bool,

  /// If true, writing a character inserts a new cell
  insert: bool,

  /// Kitty keyboard protocol flag stacks, one per screen; the top entry is
  /// the active flags.
  kitty_flags: Vec<u8>,
  alt_kitty_flags: Vec<u8>,

  title: String,
}

impl Screen {
  #[must_use]
  pub fn get_selected_text(
    &self,
    low_x: i32,
    low_y: i32,
    high_x: i32,
    high_y: i32,
  ) -> String {
    self.grid().get_selected_text(low_x, low_y, high_x, high_y)
  }

  pub fn new(size: Size, scrollback_len: usize) -> Self {
    let grid = super::grid::Grid::new(size, scrollback_len);
    Self {
      scanner: Scanner::default(),

      grid,
      alternate_grid: super::grid::Grid::new(size, 0),

      attrs: super::attrs::Attrs::default(),
      saved_attrs: super::attrs::Attrs::default(),

      modes: 0,
      mouse_protocol_mode: MouseProtocolMode::default(),
      mouse_protocol_encoding: MouseProtocolEncoding::default(),

      g0: CharSet::Ascii,
      g1: CharSet::Ascii,
      shift_out: false,

      insert: false,

      kitty_flags: Vec::new(),
      alt_kitty_flags: Vec::new(),

      title: String::new(),
    }
  }

  /// Clears the screen and scrollback, returning it to the initial state at
  /// the current size.
  pub fn reset(&mut self) {
    let size = self.grid.size();
    let scrollback_len = self.grid.scrollback_len();
    *self = Self::new(size, scrollback_len);
  }

  pub fn set_size(&mut self, rows: u16, cols: u16) {
    self.grid.set_size(Size {
      height: rows,
      width: cols,
    });
    self.alternate_grid.set_size(Size {
      height: rows,
      width: cols,
    });
  }

  /// Returns the current size of the terminal.
  ///
  /// The return value will be (rows, cols).
  #[must_use]
  pub fn size(&self) -> Size {
    self.grid().size()
  }

  /// Returns the current position in the scrollback.
  ///
  /// This position indicates the offset from the top of the screen, and is
  /// `0` when the normal screen is in view.
  #[must_use]
  pub fn scrollback(&self) -> usize {
    self.grid().scrollback()
  }

  #[must_use]
  pub fn scrollback_len(&self) -> usize {
    self.grid().scrollback_len()
  }

  pub fn set_scrollback(&mut self, rows: usize) {
    self.grid_mut().set_scrollback(rows);
  }

  pub fn scroll_screen_up(&mut self, n: usize) {
    let pos = usize::saturating_add(self.scrollback(), n);
    self.set_scrollback(pos);
  }

  pub fn scroll_screen_down(&mut self, n: usize) {
    let pos = usize::saturating_sub(self.scrollback(), n);
    self.set_scrollback(pos);
  }

  /// Returns the current cursor position of the terminal.
  ///
  /// The return value will be (row, col).
  #[must_use]
  pub fn cursor_position(&self) -> (u16, u16) {
    let pos = self.grid().pos();
    (pos.row, pos.col)
  }

  /// Returns the `Cell` object at the given location in the terminal, if it
  /// exists.
  #[must_use]
  pub fn cell(&self, row: u16, col: u16) -> Option<&super::cell::Cell> {
    self.grid().visible_cell(super::grid::Pos { row, col })
  }

  #[must_use]
  pub fn cursor_style(&self) -> CursorStyle {
    self.grid.cursor_style
  }

  #[must_use]
  pub fn title(&self) -> &str {
    &self.title
  }

  /// Returns whether the terminal should be in application cursor mode.
  #[must_use]
  pub fn application_cursor(&self) -> bool {
    self.mode(MODE_APPLICATION_CURSOR)
  }

  /// Active kitty keyboard protocol flags (0 = legacy encoding).
  #[must_use]
  pub fn kitty_flags(&self) -> u8 {
    self.kitty_flags.last().copied().unwrap_or(0)
      & KITTY_KEYBOARD_SUPPORTED_FLAGS
  }

  /// Returns whether the terminal should be in hide cursor mode.
  #[must_use]
  pub fn hide_cursor(&self) -> bool {
    self.mode(MODE_HIDE_CURSOR)
  }

  /// For a task that draws into the screen directly instead of feeding
  /// it terminal output.
  pub fn set_hide_cursor(&mut self, hide: bool) {
    if hide {
      self.set_mode(MODE_HIDE_CURSOR);
    } else {
      self.clear_mode(MODE_HIDE_CURSOR);
    }
  }

  /// Returns the currently active `MouseProtocolMode`
  #[must_use]
  pub fn mouse_protocol_mode(&self) -> MouseProtocolMode {
    self.mouse_protocol_mode
  }

  #[must_use]
  pub fn mouse_protocol_encoding(&self) -> MouseProtocolEncoding {
    self.mouse_protocol_encoding
  }

  #[must_use]
  pub fn bracketed_paste(&self) -> bool {
    self.mode(MODE_BRACKETED_PASTE)
  }

  pub fn grid(&self) -> &super::grid::Grid {
    if self.mode(MODE_ALTERNATE_SCREEN) {
      &self.alternate_grid
    } else {
      &self.grid
    }
  }

  pub fn grid_mut(&mut self) -> &mut super::grid::Grid {
    if self.mode(MODE_ALTERNATE_SCREEN) {
      &mut self.alternate_grid
    } else {
      &mut self.grid
    }
  }

  fn enter_alternate_grid(&mut self) {
    self.grid_mut().set_scrollback(0);
    if !self.mode(MODE_ALTERNATE_SCREEN) {
      self.set_mode(MODE_ALTERNATE_SCREEN);
      std::mem::swap(&mut self.kitty_flags, &mut self.alt_kitty_flags);
    }
  }

  fn exit_alternate_grid(&mut self) {
    if self.mode(MODE_ALTERNATE_SCREEN) {
      self.clear_mode(MODE_ALTERNATE_SCREEN);
      std::mem::swap(&mut self.kitty_flags, &mut self.alt_kitty_flags);
    }
  }

  fn save_cursor(&mut self) {
    self.grid_mut().save_cursor();
    self.saved_attrs = self.attrs;
  }

  fn restore_cursor(&mut self) {
    self.grid_mut().restore_cursor();
    self.attrs = self.saved_attrs;
  }

  fn set_mode(&mut self, mode: u8) {
    self.modes |= mode;
  }

  fn clear_mode(&mut self, mode: u8) {
    self.modes &= !mode;
  }

  fn mode(&self, mode: u8) -> bool {
    self.modes & mode != 0
  }

  fn set_mouse_mode(&mut self, mode: MouseProtocolMode) {
    self.mouse_protocol_mode = mode;
  }

  fn clear_mouse_mode(&mut self, mode: MouseProtocolMode) {
    if self.mouse_protocol_mode == mode {
      self.mouse_protocol_mode = MouseProtocolMode::default();
    }
  }

  fn set_mouse_encoding(&mut self, encoding: MouseProtocolEncoding) {
    self.mouse_protocol_encoding = encoding;
  }

  fn clear_mouse_encoding(&mut self, encoding: MouseProtocolEncoding) {
    if self.mouse_protocol_encoding == encoding {
      self.mouse_protocol_encoding = MouseProtocolEncoding::default();
    }
  }
}

impl Screen {
  fn text(&mut self, c: char) {
    let pos = self.grid().pos();
    let size = self.grid().size();
    let attrs = self.attrs;

    let width = c.width();
    if width.is_none() && (u32::from(c)) < 256 {
      // don't even try to draw control characters
      return;
    }
    let width = width
      .unwrap_or(1)
      .try_into()
      // width() can only return 0, 1, or 2
      .unwrap();

    // it doesn't make any sense to wrap if the last column in a row
    // didn't already have contents. don't try to handle the case where a
    // character wraps because there was only one column left in the
    // previous row - literally everything handles this case differently,
    // and this is tmux behavior (and also the simplest). i'm open to
    // reconsidering this behavior, but only with a really good reason
    // (xterm handles this by introducing the concept of triple width
    // cells, which i really don't want to do).
    let mut wrap = false;
    if pos.col > size.width - width {
      let last_cell_pos = super::grid::Pos {
        row: pos.row,
        col: size.width - 1,
      };
      let last_cell = self
        .grid()
        .drawing_cell(last_cell_pos)
        // pos.row is valid, since it comes directly from
        // self.grid().pos() which we assume to always have a valid
        // row value. size.cols - 1 is also always a valid column.
        .unwrap();
      if last_cell.has_contents()
        || self.grid().is_wide_continuation(last_cell_pos)
      {
        wrap = true;
      }
    }
    self.grid_mut().col_wrap(width, wrap);
    let pos = self.grid().pos();

    if width == 0 {
      if pos.col > 0 {
        let prev_cell_pos = super::grid::Pos {
          row: pos.row,
          col: pos.col - 1,
        };
        let prev_cell_pos = if self.grid().is_wide_continuation(prev_cell_pos) {
          super::grid::Pos {
            row: pos.row,
            col: pos.col - 2,
          }
        } else {
          prev_cell_pos
        };
        let prev_cell = self
          .grid_mut()
          .drawing_cell_mut(prev_cell_pos)
          // pos.row is valid, since it comes directly from
          // self.grid().pos() which we assume to always have a
          // valid row value. pos.col - 1 is valid because we just
          // checked for pos.col > 0.
          // pos.col - 2 is valid because pos.col - 1 is a wide continuation
          .unwrap();
        prev_cell.append(c);
      } else if pos.row > 0 {
        let prev_row = self
          .grid()
          .drawing_row(pos.row - 1)
          // pos.row is valid, since it comes directly from
          // self.grid().pos() which we assume to always have a
          // valid row value. pos.row - 1 is valid because we just
          // checked for pos.row > 0.
          .unwrap();
        if prev_row.wrapped() {
          let prev_cell_pos = super::grid::Pos {
            row: pos.row - 1,
            col: size.width - 1,
          };
          let prev_cell_pos = if self.grid().is_wide_continuation(prev_cell_pos)
          {
            super::grid::Pos {
              row: pos.row - 1,
              col: size.width - 2,
            }
          } else {
            prev_cell_pos
          };
          let prev_cell = self
            .grid_mut()
            .drawing_cell_mut(prev_cell_pos)
            // pos.row is valid, since it comes directly from
            // self.grid().pos() which we assume to always
            // have a valid row value. pos.row - 1 is valid
            // because we just checked for pos.row > 0. col of
            // size.cols - 2 is valid because the cell at
            // size.cols - 1 is a wide continuation character,
            // so it must have the first half of the wide
            // character before it.
            .unwrap();
          prev_cell.append(c);
        }
      }
    } else {
      if self.insert {
        self.grid_mut().insert_cells(width);
      }
      if self.grid().is_wide_continuation(pos) {
        let prev_cell = self
          .grid_mut()
          .drawing_cell_mut(super::grid::Pos {
            row: pos.row,
            col: pos.col - 1,
          })
          // pos.row is valid because we assume self.grid().pos() to
          // always have a valid row value. pos.col is valid because
          // we called col_wrap() immediately before this, which
          // ensures that self.grid().pos().col has a valid value.
          // pos.col - 1 is valid because the cell at pos.col is a
          // wide continuation character, so it must have the first
          // half of the wide character before it.
          .unwrap();
        prev_cell.clear(attrs);
      }

      if self
        .grid()
        .drawing_cell(pos)
        // pos.row is valid because we assume self.grid().pos() to
        // always have a valid row value. pos.col is valid because we
        // called col_wrap() immediately before this, which ensures
        // that self.grid().pos().col has a valid value.
        .unwrap()
        .is_wide()
      {
        if let Some(next_cell) =
          self.grid_mut().drawing_cell_mut(super::grid::Pos {
            row: pos.row,
            col: pos.col + 1,
          })
        {
          next_cell.set(' ', attrs);
        }
      }

      let cell = self
        .grid_mut()
        .drawing_cell_mut(pos)
        // pos.row is valid because we assume self.grid().pos() to
        // always have a valid row value. pos.col is valid because we
        // called col_wrap() immediately before this, which ensures
        // that self.grid().pos().col has a valid value.
        .unwrap();
      cell.set(c, attrs);
      self.grid_mut().col_inc(1);
      if width > 1 {
        let pos = self.grid().pos();
        if self
          .grid()
          .drawing_cell(pos)
          // pos.row is valid because we assume self.grid().pos() to
          // always have a valid row value. pos.col is valid because
          // we called col_wrap() earlier, which ensures that
          // self.grid().pos().col has a valid value. this is true
          // even though we just called col_inc, because this branch
          // only happens if width > 1, and col_wrap takes width
          // into account.
          .unwrap()
          .is_wide()
        {
          let next_next_pos = super::grid::Pos {
            row: pos.row,
            col: pos.col + 1,
          };
          let next_next_cell = self
            .grid_mut()
            .drawing_cell_mut(next_next_pos)
            // pos.row is valid because we assume
            // self.grid().pos() to always have a valid row value.
            // pos.col is valid because we called col_wrap()
            // earlier, which ensures that self.grid().pos().col
            // has a valid value. this is true even though we just
            // called col_inc, because this branch only happens if
            // width > 1, and col_wrap takes width into account.
            // pos.col + 1 is valid because the cell at pos.col is
            // wide, and so it must have the second half of the
            // wide character after it.
            .unwrap();
          next_next_cell.clear(attrs);
          if next_next_pos.col == size.width - 1 {
            self
              .grid_mut()
              .drawing_row_mut(pos.row)
              // we assume self.grid().pos().row is always valid
              .unwrap()
              .wrap(false);
          }
        }
        let next_cell = self
          .grid_mut()
          .drawing_cell_mut(pos)
          // pos.row is valid because we assume self.grid().pos() to
          // always have a valid row value. pos.col is valid because
          // we called col_wrap() earlier, which ensures that
          // self.grid().pos().col has a valid value. this is true
          // even though we just called col_inc, because this branch
          // only happens if width > 1, and col_wrap takes width
          // into account.
          .unwrap();
        next_cell.clear(super::attrs::Attrs::default());
        self.grid_mut().col_inc(1);
      }
    }
  }

  // control codes

  fn tab(&mut self) {
    self.grid_mut().col_tab();
  }

  // escape codes

  // ESC 7
  fn decsc(&mut self) {
    self.save_cursor();
  }

  // ESC 8
  fn decrc(&mut self) {
    self.restore_cursor();
  }

  // ESC M
  fn ri(&mut self) {
    self.grid_mut().row_dec_scroll(1);
  }

  // ESC c
  fn ris(&mut self) {
    *self = Self::new(self.grid.size(), self.grid.scrollback_len());
  }

  // csi codes

  // CSI @
  fn ich(&mut self, count: u16) {
    self.grid_mut().insert_cells(count);
  }

  // CSI J
  fn ed(&mut self, mode: u16) {
    let attrs = self.attrs;
    match mode {
      0 => self.grid_mut().erase_all_forward(attrs),
      1 => self.grid_mut().erase_all_backward(attrs),
      2 => self.grid_mut().erase_all(attrs),
      n => {
        log::debug!("Unhandled ED mode: {n}");
      }
    }
  }

  // CSI ? J
  fn decsed(&mut self, mode: u16) {
    self.ed(mode);
  }

  // CSI K
  fn el(&mut self, mode: u16) {
    let attrs = self.attrs;
    match mode {
      0 => self.grid_mut().erase_row_forward(attrs),
      1 => self.grid_mut().erase_row_backward(attrs),
      2 => self.grid_mut().erase_row(attrs),
      n => {
        log::debug!("unhandled EL mode: {n}");
      }
    }
  }

  // CSI ? K
  fn decsel(&mut self, mode: u16) {
    self.el(mode);
  }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VtEvent {
  Bell,
  Reply(Reply),
}

/// A reply the terminal sends back to the program that requested it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reply {
  PrimaryDeviceAttrs,
  /// 0-based cursor position.
  CursorPos {
    row: u16,
    col: u16,
  },
  KittyFlags(u8),
}

impl Screen {
  /// <https://man7.org/linux/man-pages/man4/console_codes.4.html>
  /// <https://en.wikipedia.org/wiki/ANSI_escape_code>
  /// <https://terminalguide.namepad.de/seq>/
  /// <https://vt100.net/docs/vt510-rm/contents.html>
  /// <https://xtermjs.org/docs/api/vtfeatures/>
  /// <https://learn.microsoft.com/en-us/windows/console/console-virtual-terminal-sequences>
  /// <https://bjh21.me.uk/all-escapes/all-escapes.txt>
  pub fn process(&mut self, data: &[u8], events: &mut Vec<VtEvent>) {
    let mut scanner = std::mem::take(&mut self.scanner);
    scanner.feed(data, |seq| self.apply(seq, events));
    self.scanner = scanner;
  }

  fn apply(&mut self, seq: Seq, events: &mut Vec<VtEvent>) {
    match seq {
      Seq::Text(text) => self.text_run(text),
      Seq::Ctl(b) => match b {
        0x07 => events.push(VtEvent::Bell),
        0x08 => self.grid_mut().col_dec(1),
        0x09 => self.tab(),
        0x0A | 0x0B | 0x0C => {
          self.grid_mut().row_inc_scroll(1);
        }
        0x0D => self.grid_mut().col_set(0),
        0x0E => self.shift_out = true,
        0x0F => self.shift_out = false,
        // Legacy C1 RI byte.
        0x8D => self.ri(),
        _ => (),
      },
      Seq::Esc { inter: 0, final_ } => match final_ {
        b'7' => self.decsc(),
        b'8' => self.decrc(),
        // DECKPAM
        b'=' => self.set_mode(MODE_APPLICATION_KEYPAD),
        // DECKPNM
        b'>' => self.clear_mode(MODE_APPLICATION_KEYPAD),
        // PAD
        b'@' => (),
        // RI - Reverse Index
        b'M' => self.ri(),
        // RIS - Full Reset
        b'c' => self.ris(),
        c => log::debug!("Unhandled ESC {} ({:#x})", c as char, c),
      },
      Seq::Esc { inter, final_ } => match inter {
        // ESC ( rest - Setup G0 charset with 94 characters
        // ESC ) rest - Setup G1 charset with 94 characters
        // ESC * rest - Setup G2 charset with 94 characters
        // ESC + rest - Setup G3 charset with 94 characters
        b'(' | b')' | b'*' | b'+' => {
          let charset = match final_ {
            // UK
            b'A' => Some(CharSet::Uk),
            // ASCII
            b'B' => Some(CharSet::Ascii),
            // DEC Special Character and Line Drawing Set
            b'0' => Some(CharSet::DecLineDrawing),
            _ => None,
          };
          if let Some(charset) = charset {
            match inter {
              b'(' => self.g0 = charset,
              b')' => self.g1 = charset,
              _ => (),
            }
          }
        }
        _ => {
          log::debug!("Ignored nF: ESC {} {}", inter as char, final_ as char)
        }
      },
      Seq::Csi(p) => self.csi(p, events),
      // TODO: Handle DCS
      Seq::Dcs(_) => (),
      Seq::Osc(data) => self.osc(data),
      // Input-only items; an output scanner never produces them.
      Seq::EscChar(_) | Seq::Ss3(_) | Seq::X10Mouse(..) => (),
    }
  }

  fn text_run(&mut self, text: &str) {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
      let c = text[i..].chars().next().unwrap();
      i += c.len_utf8();
      self.text(c);

      if self.insert {
        continue;
      }
      // Blit the following run of plain ASCII cells within the current
      // row; wrapping and wide chars go through the per-char path.
      let ascii_len = bytes[i..]
        .iter()
        .take_while(|b| (0x20..=0x7E).contains(*b))
        .count();
      if ascii_len == 0 {
        continue;
      }
      let pos = self.grid().pos();
      let size = self.grid().size();
      if pos.col >= size.width {
        // Pending wrap; the per-char path handles it.
        continue;
      }
      let n = ascii_len.min((size.width - pos.col) as usize);
      let attrs = self.attrs;
      self
        .grid_mut()
        .write_ascii_row(pos, &bytes[i..i + n], attrs);
      self.grid_mut().col_inc(n as u16);
      i += n;
    }
  }

  fn csi(&mut self, p: &Params, events: &mut Vec<VtEvent>) {
    if p.invalid() {
      csi_todo(p);
      return;
    }
    match (p.prefix, p.inter, p.final_) {
      // DECSED - Selective Erase Display
      // https://terminalguide.namepad.de/seq/csi_cj__p/
      (b'?', 0, b'J') => self.decsed(p.get16(0, 0)),
      // DECSEL - Selective Erase Line
      // https://terminalguide.namepad.de/seq/csi_ck__p/
      (b'?', 0, b'K') => self.decsel(p.get16(0, 0)),
      // ICH - Insert Character
      // https://terminalguide.namepad.de/seq/csi_x40_at/
      (0, 0, b'@') => self.ich(p.get16(0, 1)),
      // CUU - Cursor Up
      // https://terminalguide.namepad.de/seq/csi_ca/
      (0, 0, b'A') => self.grid_mut().row_dec_clamp(p.get16(0, 1).max(1)),
      // CUD - Cursor Down
      // https://terminalguide.namepad.de/seq/csi_cb/
      (0, 0, b'B') => self.grid_mut().row_inc_clamp(p.get16(0, 1).max(1)),
      // CUF - Cursor Right
      // https://terminalguide.namepad.de/seq/csi_cc/
      (0, 0, b'C') => self.grid_mut().col_inc_clamp(p.get16(0, 1).max(1)),
      // CUB - Cursor Left
      // https://terminalguide.namepad.de/seq/csi_cd/
      (0, 0, b'D') => self.grid_mut().col_dec(p.get16(0, 1).max(1)),
      // CNL - Cursor Next Line
      // https://terminalguide.namepad.de/seq/csi_ce/
      (0, 0, b'E') => {
        let count = p.get16(0, 1).max(1);
        self.grid_mut().row_inc_clamp(count);
        self.grid_mut().col_set(0);
      }
      // CPL - Cursor Previous Line
      // https://terminalguide.namepad.de/seq/csi_cf/
      (0, 0, b'F') => {
        let count = p.get16(0, 1).max(1);
        self.grid_mut().row_dec_clamp(count);
        self.grid_mut().col_set(0);
      }
      // CHA - Cursor Horizontal Absolute
      // https://terminalguide.namepad.de/seq/csi_cg/
      (0, 0, b'G') => self.grid_mut().col_set(p.get16(0, 1).max(1) - 1),
      // CUP - Cursor Position
      // https://terminalguide.namepad.de/seq/csi_ch/
      (0, 0, b'H') => {
        let row = p.get16(0, 1).max(1) - 1;
        let col = p.get16(1, 1).max(1) - 1;
        self.grid_mut().set_pos(super::grid::Pos { row, col });
      }
      // ED - Erase Display
      // https://terminalguide.namepad.de/seq/csi_cj/
      (0, 0, b'J') => self.ed(p.get16(0, 0)),
      // EL - Erase Line
      // https://terminalguide.namepad.de/seq/csi_ck/
      (0, 0, b'K') => self.el(p.get16(0, 0)),
      // IL - Insert Line
      // https://terminalguide.namepad.de/seq/csi_cl/
      (0, 0, b'L') => self.grid_mut().insert_lines(p.get16(0, 1)),
      // DL - Delete Line
      // https://terminalguide.namepad.de/seq/csi_cm/
      (0, 0, b'M') => {
        let amount = p.get16(0, 1).max(1);
        let attrs = self.attrs;
        self.grid_mut().delete_lines(amount, attrs);
      }
      // DCH - Delete Character
      // https://terminalguide.namepad.de/seq/csi_cp/
      (0, 0, b'P') => self.grid_mut().delete_cells(p.get16(0, 1).max(1)),
      // SU - Scroll Up
      // https://terminalguide.namepad.de/seq/csi_cs/
      (0, 0, b'S') => self.grid_mut().scroll_up(p.get16(0, 1)),
      // SD - Scroll Down; only the one-param form (the 5-param form is
      // mouse tracking)
      // https://terminalguide.namepad.de/seq/csi_ct_1param/
      (0, 0, b'T') => {
        if p.len() == 1 && p.get_opt(0).is_some() {
          self.grid_mut().scroll_down(p.get16(0, 1));
        } else {
          csi_todo(p);
        }
      }
      // ECH - Erase Character
      // https://terminalguide.namepad.de/seq/csi_cx/
      (0, 0, b'X') => {
        let amount = p.get16(0, 1).max(1);
        let attrs = self.attrs;
        self.grid_mut().erase_cells(amount, attrs);
      }
      // DA1 - Primary Device Attributes
      // https://terminalguide.namepad.de/seq/csi_sc/
      (0, 0, b'c') => events.push(VtEvent::Reply(Reply::PrimaryDeviceAttrs)),
      // Kitty keyboard protocol
      // https://sw.kovidgoyal.net/kitty/keyboard-protocol/
      (b'?', 0, b'u') => {
        events.push(VtEvent::Reply(Reply::KittyFlags(self.kitty_flags())))
      }
      (b'>', 0, b'u') => {
        if self.kitty_flags.len() >= 16 {
          self.kitty_flags.remove(0);
        }
        self
          .kitty_flags
          .push((p.get(0, 0) & KITTY_KEYBOARD_SUPPORTED_FLAGS as u32) as u8);
      }
      (b'<', 0, b'u') => {
        let n = p.get(0, 1).max(1) as usize;
        let len = self.kitty_flags.len().saturating_sub(n);
        self.kitty_flags.truncate(len);
      }
      (b'=', 0, b'u') => {
        let flags = (p.get(0, 0) & KITTY_KEYBOARD_SUPPORTED_FLAGS as u32) as u8;
        if self.kitty_flags.is_empty() {
          self.kitty_flags.push(0);
        }
        let top = self.kitty_flags.last_mut().expect("just pushed");
        match p.get(1, 1) {
          2 => *top |= flags,
          3 => *top &= !flags,
          _ => *top = flags,
        }
      }
      // VPA - Vertical Position Absolute
      // https://terminalguide.namepad.de/seq/csi_sd/
      (0, 0, b'd') => self.grid_mut().row_set(p.get16(0, 1).max(1) - 1),
      // HVP - Horizontal and Vertical Position
      // https://terminalguide.namepad.de/seq/csi_sf/
      (0, 0, b'f') => {
        let row = p.get16(0, 1).max(1) - 1;
        let col = p.get16(1, 1).max(1) - 1;
        self.grid_mut().set_pos(super::grid::Pos { row, col });
      }
      // HPA - Horizontal Position Absolute
      // https://terminalguide.namepad.de/seq/csi_x60_backtick/
      (0, 0, b'`') => self.grid_mut().col_set(p.get16(0, 1).max(1) - 1),
      // SGR - Select Graphic Rendition
      (0, 0, b'm') => self.sgr(p),
      // Set/reset modes
      // https://terminalguide.namepad.de/mode/
      (0 | b'?', 0, b'h' | b'l') => self.set_modes(p),
      // DSR - Device Status Report
      (0, 0, b'n') => match p.get_opt(0) {
        // CPR - Request Cursor Position Report
        // https://terminalguide.namepad.de/seq/csi_sn-6/
        Some(6) => {
          let pos = self.grid().pos();
          events.push(VtEvent::Reply(Reply::CursorPos {
            row: pos.row,
            col: pos.col,
          }));
        }
        n => log::debug!("Ignored DSR: {n:?}"),
      },
      // DECSCUSR - Select Cursor Style
      // https://terminalguide.namepad.de/seq/csi_sq_t_space/
      (0, b' ', b'q') => {
        self.grid.cursor_style = CursorStyle::from_u16(p.get16(0, 0));
      }
      // DECSTBM - Set Top and Bottom Margins
      // https://terminalguide.namepad.de/seq/csi_sr/
      (0, 0, b'r') => {
        let top = match p.get_opt(0) {
          Some(_) => p.get16(0, 1).max(1),
          None => 1,
        };
        let bottom = match p.get_opt(1) {
          Some(_) => p.get16(1, 1).max(1),
          None => self.grid().size().height,
        };
        self.grid_mut().set_scroll_region(top - 1, bottom - 1);
      }
      _ => csi_todo(p),
    }
  }

  fn sgr(&mut self, p: &Params) {
    if p.len() == 0 {
      self.attrs = Attrs::default();
      return;
    }
    let mut i = 0;
    while i < p.len() {
      if p.is_sub(i) {
        // A subparameter of an attribute that does not use it
        // (e.g. the 3 in underline style 4:3).
        i += 1;
        continue;
      }
      match p.get(i, 0) {
        // Reset
        0 => self.attrs = Attrs::default(),
        // Bold
        1 => {
          self.attrs.set_bold(true);
        }
        // Dim
        2 => {
          self.attrs.set_bold(false);
        }
        // Italic
        3 => {
          self.attrs.set_italic(true);
        }
        // Underline
        4 => {
          self.attrs.set_underline(true);
        }
        // Slow blink
        5 => (),
        // Rapid blink
        6 => (),
        // Invert
        7 => {
          self.attrs.set_inverse(true);
        }
        // Crossed-out
        9 => (),
        // Doubly underlined
        21 => {
          self.attrs.set_underline(true);
        }
        // Normal intensity
        22 => {
          self.attrs.set_bold(false);
        }
        // Not italic
        23 => {
          self.attrs.set_italic(false);
        }
        // Not underlined
        24 => {
          self.attrs.set_underline(false);
        }
        // Not blinking
        25 => (),
        // Not reversed
        27 => {
          self.attrs.set_inverse(false);
        }
        // Not crossed-out
        29 => (),
        n @ 30..=37 => {
          self.attrs.fgcolor = Color::Idx(n as u8 - 30);
        }
        38 => {
          let (color, next) = sgr_color(p, i);
          self.attrs.fgcolor = color;
          i = next;
          continue;
        }
        39 => {
          self.attrs.fgcolor = Color::Default;
        }
        n @ 40..=47 => {
          self.attrs.bgcolor = Color::Idx(n as u8 - 40);
        }
        48 => {
          let (color, next) = sgr_color(p, i);
          self.attrs.bgcolor = color;
          i = next;
          continue;
        }
        49 => {
          self.attrs.bgcolor = Color::Default;
        }
        n @ 90..=97 => {
          self.attrs.fgcolor = Color::Idx(n as u8 - 90 + 8);
        }
        n @ 100..=107 => {
          self.attrs.bgcolor = Color::Idx(n as u8 - 100 + 8);
        }
        n => {
          log::debug!("Ignored SGR: {}", n);
        }
      }
      i += 1;
    }
  }

  fn set_modes(&mut self, p: &Params) {
    let set = p.final_ == b'h';
    if p.len() == 0 {
      csi_todo(p);
      return;
    }
    for i in 0..p.len() {
      if p.is_sub(i) {
        continue;
      }
      let Some(mode) = p.get_opt(i) else {
        continue;
      };
      match (p.prefix, mode) {
        // IRM - Insert Mode
        (0, 4) => self.insert = set,
        // DECRLM - Cursor direction, right to left. Not supported.
        // https://vt100.net/docs/vt510-rm/DECRLM.html
        (0, 34) => (),
        // DECCKM
        (b'?', 1) => {
          if set {
            self.set_mode(MODE_APPLICATION_CURSOR);
          } else {
            self.clear_mode(MODE_APPLICATION_CURSOR);
          }
        }
        // DECOM - Origin Mode
        (b'?', 6) => self.grid_mut().set_origin_mode(set),
        // Mouse Click-Only Tracking (X10_MOUSE)
        (b'?', 9) => {
          if set {
            self.set_mouse_mode(MouseProtocolMode::Press);
          } else {
            self.clear_mouse_mode(MouseProtocolMode::Press);
          }
        }
        // DECTCEM
        (b'?', 25) => {
          if set {
            self.clear_mode(MODE_HIDE_CURSOR);
          } else {
            self.set_mode(MODE_HIDE_CURSOR);
          }
        }
        // Alternate Screen Buffer (ALTBUF)
        (b'?', 47) => {
          if set {
            self.enter_alternate_grid();
          } else {
            self.exit_alternate_grid();
          }
        }
        (b'?', 1000) => {
          if set {
            self.set_mouse_mode(MouseProtocolMode::PressRelease);
          } else {
            self.clear_mouse_mode(MouseProtocolMode::PressRelease);
          }
        }
        (b'?', 1002) => {
          if set {
            self.set_mouse_mode(MouseProtocolMode::ButtonMotion);
          } else {
            self.clear_mouse_mode(MouseProtocolMode::ButtonMotion);
          }
        }
        (b'?', 1003) => {
          if set {
            self.set_mouse_mode(MouseProtocolMode::AnyMotion);
          } else {
            self.clear_mouse_mode(MouseProtocolMode::AnyMotion);
          }
        }
        (b'?', 1005) => {
          if set {
            self.set_mouse_encoding(MouseProtocolEncoding::Utf8);
          } else {
            self.clear_mouse_encoding(MouseProtocolEncoding::Utf8);
          }
        }
        (b'?', 1006) => {
          if set {
            self.set_mouse_encoding(MouseProtocolEncoding::Sgr);
          } else {
            self.clear_mouse_encoding(MouseProtocolEncoding::Sgr);
          }
        }
        // Alternate Screen Buffer, With Cursor Save and Clear on Enter
        (b'?', 1049) => {
          if set {
            self.decsc();
            self.alternate_grid.clear();
            self.enter_alternate_grid();
          } else {
            self.exit_alternate_grid();
            self.decrc();
          }
        }
        // Bracketed Paste Mode
        (b'?', 2004) => {
          if set {
            self.set_mode(MODE_BRACKETED_PASTE);
          } else {
            self.clear_mode(MODE_BRACKETED_PASTE);
          }
        }
        _ => csi_todo(p),
      }
    }
  }

  fn osc(&mut self, data: &[u8]) {
    let s = match str::from_utf8(data) {
      Ok(s) => s,
      Err(_) => return,
    };
    // OSC Ps ; Pt ST
    // Ps = 0: Set icon name and window title
    // Ps = 1: Set icon name
    // Ps = 2: Set window title
    if let Some((ps, pt)) = s.split_once(';') {
      match ps {
        "0" | "2" => {
          self.title = pt.to_string();
        }
        "1" => {
          // Icon name
        }
        _ => {
          log::debug!("Unhandled OSC: {ps}");
        }
      }
    }
  }
}

fn csi_todo(p: &Params) {
  log::debug!("CSI not implemented: {:?}", p);
}

/// Extended color argument of SGR 38/48; accepts both the semicolon and the
/// colon forms. Returns the color and the index after the consumed params.
fn sgr_color(p: &Params, i: usize) -> (Color, usize) {
  match p.get(i + 1, 2) {
    2 => {
      // The ITU colon form may carry an empty color space id before the
      // components: `38:2::r:g:b`.
      let i = if p.is_sub(i + 2) && p.get_opt(i + 2).is_none() {
        i + 1
      } else {
        i
      };
      (
        Color::Rgb(
          p.get(i + 2, 0).min(255) as u8,
          p.get(i + 3, 0).min(255) as u8,
          p.get(i + 4, 0).min(255) as u8,
        ),
        i + 5,
      )
    }
    5 => (Color::Idx(p.get(i + 2, 0).min(255) as u8), i + 3),
    _ => (Color::Default, i + 2),
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn screen(width: u16, height: u16) -> Screen {
    Screen::new(Size { height, width }, 100)
  }

  fn feed(screen: &mut Screen, bytes: &[u8]) -> Vec<VtEvent> {
    let mut events = Vec::new();
    screen.process(bytes, &mut events);
    events
  }

  fn row_text(screen: &Screen, row: u16) -> String {
    let mut out = String::new();
    let mut skip_continuation = false;
    for col in 0..screen.size().width {
      if skip_continuation {
        skip_continuation = false;
        continue;
      }
      match screen.cell(row, col) {
        Some(cell) if cell.has_contents() => {
          out.push_str(cell.contents());
          skip_continuation = cell.is_wide();
        }
        _ => out.push(' '),
      }
    }
    out.trim_end().to_string()
  }

  #[test]
  fn text_wrap_and_controls() {
    let mut s = screen(5, 3);
    feed(&mut s, b"hello, world");
    assert_eq!(row_text(&s, 0), "hello");
    assert_eq!(row_text(&s, 1), ", wor");
    assert_eq!(row_text(&s, 2), "ld");
    assert_eq!(s.cursor_position(), (2, 2));

    let mut s = screen(11, 3);
    feed(&mut s, b"ab\r\ncd\x08X\ttab");
    assert_eq!(row_text(&s, 0), "ab");
    assert_eq!(row_text(&s, 1), "cX      tab");
  }

  #[test]
  fn wide_chars() {
    let mut s = screen(10, 2);
    feed(&mut s, "a\u{6d4b}b".as_bytes());
    assert_eq!(row_text(&s, 0), "a\u{6d4b}b");
    assert_eq!(s.cursor_position(), (0, 4));
    // Overwrite the left half of the wide char.
    feed(&mut s, b"\x1b[1;2HX");
    assert_eq!(row_text(&s, 0), "aX b");
  }

  // Feeding byte-at-a-time never enters the ASCII blit in text_run (every
  // Text item is a single char), while a one-shot feed blits maximally.
  // Both must produce identical screens, including at wide-char overwrite
  // boundaries and across wraps.
  #[test]
  fn blit_matches_per_char() {
    let width = 10;
    for start_col in 0..width {
      for run in ["XY", "XYZXYZXYZXYZXYZ", "x"] {
        let paint = "\u{6d4b}\u{8bd5}\u{5bbd}".as_bytes();
        let mut blit = screen(width, 3);
        let mut per_char = screen(width, 3);
        for s in [&mut blit, &mut per_char] {
          feed(s, paint);
          feed(s, format!("\x1b[1;{}H", start_col + 1).as_bytes());
        }

        feed(&mut blit, run.as_bytes());
        for b in run.as_bytes() {
          feed(&mut per_char, &[*b]);
        }

        for row in 0..3 {
          assert_eq!(
            row_text(&blit, row),
            row_text(&per_char, row),
            "row {row}, start {start_col}, run {run:?}"
          );
        }
        assert_eq!(
          blit.cursor_position(),
          per_char.cursor_position(),
          "cursor, start {start_col}, run {run:?}"
        );
        assert_eq!(
          crate::term::ansi::render_screen_ansi(&blit),
          crate::term::ansi::render_screen_ansi(&per_char),
          "attrs, start {start_col}, run {run:?}"
        );
      }
    }
  }

  #[test]
  fn sgr_colors() {
    let mut s = screen(20, 2);
    feed(
      &mut s,
      b"\x1b[1;31mR\x1b[0m\x1b[38;5;100mI\x1b[38;2;1;2;3mT\x1b[38:5:200mC\x1b[38:2::4:5:6;1mX",
    );
    let cell = s.cell(0, 0).unwrap();
    assert!(cell.attrs().bold());
    assert_eq!(cell.attrs().fgcolor, Color::Idx(1));
    assert_eq!(s.cell(0, 1).unwrap().attrs().fgcolor, Color::Idx(100));
    assert_eq!(s.cell(0, 2).unwrap().attrs().fgcolor, Color::Rgb(1, 2, 3));
    assert_eq!(s.cell(0, 3).unwrap().attrs().fgcolor, Color::Idx(200));
    let cell = s.cell(0, 4).unwrap();
    assert_eq!(cell.attrs().fgcolor, Color::Rgb(4, 5, 6));
    assert!(cell.attrs().bold(), "params after the colour still apply");
  }

  #[test]
  fn cursor_and_erase() {
    let mut s = screen(10, 4);
    feed(&mut s, b"aaaa\r\nbbbb\x1b[1;1H\x1b[K");
    assert_eq!(row_text(&s, 0), "");
    assert_eq!(row_text(&s, 1), "bbbb");
    feed(&mut s, b"\x1b[2J\x1b[3;2Hx");
    assert_eq!(row_text(&s, 1), "");
    assert_eq!(row_text(&s, 2), " x");
    assert_eq!(s.cursor_position(), (2, 2));
  }

  #[test]
  fn replies() {
    let mut s = screen(10, 4);
    let events = feed(&mut s, b"\x1b[c\x1b[2;5H\x1b[6n\x07");
    assert_eq!(
      events,
      vec![
        VtEvent::Reply(Reply::PrimaryDeviceAttrs),
        VtEvent::Reply(Reply::CursorPos { row: 1, col: 4 }),
        VtEvent::Bell,
      ]
    );
  }

  #[test]
  fn title() {
    let mut s = screen(10, 2);
    feed(&mut s, b"\x1b]0;hello\x07");
    assert_eq!(s.title(), "hello");
    feed(&mut s, b"\x1b]2;there\x1b\\");
    assert_eq!(s.title(), "there");
  }

  #[test]
  fn modes_and_alt_screen() {
    let mut s = screen(10, 4);
    feed(&mut s, b"main\x1b[?1049h");
    assert_eq!(row_text(&s, 0), "");
    feed(&mut s, b"alt");
    assert_eq!(row_text(&s, 0), "alt");
    feed(&mut s, b"\x1b[?1049l");
    assert_eq!(row_text(&s, 0), "main");
    assert_eq!(s.cursor_position(), (0, 4));

    feed(&mut s, b"\x1b[?1h");
    assert!(s.application_cursor());
    feed(&mut s, b"\x1b[?1l");
    assert!(!s.application_cursor());
    feed(&mut s, b"\x1b[?25l");
    assert!(s.hide_cursor());
  }

  #[test]
  fn kitty_keyboard_flags() {
    let mut s = screen(10, 4);
    assert_eq!(s.kitty_flags(), 0);
    assert_eq!(
      feed(&mut s, b"\x1b[?u"),
      vec![VtEvent::Reply(Reply::KittyFlags(0))]
    );

    // Unsupported flags are neither activated nor advertised.
    feed(&mut s, b"\x1b[>2u");
    assert_eq!(s.kitty_flags(), 0);
    assert_eq!(
      feed(&mut s, b"\x1b[?u"),
      vec![VtEvent::Reply(Reply::KittyFlags(0))]
    );
    feed(&mut s, b"\x1b[<u");

    feed(&mut s, b"\x1b[>1u");
    assert_eq!(s.kitty_flags(), 1);
    feed(&mut s, b"\x1b[>15u");
    assert_eq!(s.kitty_flags(), 1);
    assert_eq!(
      feed(&mut s, b"\x1b[?u"),
      vec![VtEvent::Reply(Reply::KittyFlags(1))]
    );
    feed(&mut s, b"\x1b[=4;3u");
    assert_eq!(s.kitty_flags(), 1);
    feed(&mut s, b"\x1b[=4;2u");
    assert_eq!(s.kitty_flags(), 1);
    feed(&mut s, b"\x1b[=2u");
    assert_eq!(s.kitty_flags(), 0);
    assert_eq!(
      feed(&mut s, b"\x1b[?u"),
      vec![VtEvent::Reply(Reply::KittyFlags(0))]
    );

    // The alternate screen has its own stack.
    feed(&mut s, b"\x1b[?1049h");
    assert_eq!(s.kitty_flags(), 0);
    feed(&mut s, b"\x1b[>1u");
    feed(&mut s, b"\x1b[?1049l");
    assert_eq!(s.kitty_flags(), 0);

    feed(&mut s, b"\x1b[<u");
    assert_eq!(s.kitty_flags(), 1);
    feed(&mut s, b"\x1b[<5u");
    assert_eq!(s.kitty_flags(), 0);
    // Combined modes in one sequence.
    feed(&mut s, b"\x1b[?1000;1006h");
    assert_eq!(s.mouse_protocol_mode(), MouseProtocolMode::PressRelease);
    feed(&mut s, b"\x1b[?1000;1006l");
    assert_eq!(s.mouse_protocol_mode(), MouseProtocolMode::None);
  }

  #[test]
  fn scroll_region() {
    let mut s = screen(4, 4);
    feed(&mut s, b"1\r\n2\r\n3\r\n4\x1b[2;3r\x1b[2;1H\x1b[1S");
    assert_eq!(row_text(&s, 0), "1");
    assert_eq!(row_text(&s, 1), "3");
    assert_eq!(row_text(&s, 2), "");
    assert_eq!(row_text(&s, 3), "4");
  }

  #[test]
  fn oversized_osc_aborts() {
    let mut s = screen(10, 2);
    feed(&mut s, b"\x1b]0;");
    feed(&mut s, &vec![b'x'; 64 * 1024 + 10]);
    // The flood aborted the OSC; the tail renders as plain text and no
    // title was set.
    assert_eq!(s.title(), "");
    assert_eq!(s.cell(0, 0).unwrap().contents(), "x");
  }

  mod chunking {
    use super::*;
    use proptest::prelude::*;

    fn atom() -> impl Strategy<Value = Vec<u8>> {
      prop_oneof![
        // Printable text
        "[ -~]{0,6}".prop_map(String::into_bytes),
        // Unicode text
        "[à-语]{0,3}".prop_map(String::into_bytes),
        // Arbitrary bytes
        proptest::collection::vec(any::<u8>(), 0..4),
        // Interesting sequences
        proptest::sample::select(vec![
          b"\x1b[2J".to_vec(),
          b"\x1b[1;31m".to_vec(),
          b"\x1b[38;5;100m".to_vec(),
          b"\x1b[38:2:9:8:7m".to_vec(),
          b"\x1b[10;10H".to_vec(),
          b"\r\n".to_vec(),
          b"\x1b]0;title\x07".to_vec(),
          b"\x1b]0;t\x1b\\".to_vec(),
          b"\x1b[?1049h".to_vec(),
          b"\x1b[?1049l".to_vec(),
          b"\x1b[A".to_vec(),
          b"\x1b[K".to_vec(),
          b"\x1b7".to_vec(),
          b"\x1b8".to_vec(),
          b"\x1b[6n".to_vec(),
          b"\x1b[c".to_vec(),
          b"\x1b(0".to_vec(),
          b"\x1b[2;3r".to_vec(),
          b"\x1bPdcs\x1b\\".to_vec(),
          b"\x07".to_vec(),
        ]),
      ]
    }

    fn snapshot(s: &Screen) -> (String, (u16, u16), String, bool) {
      (
        crate::term::ansi::render_screen_ansi(s),
        s.cursor_position(),
        s.title().to_string(),
        s.hide_cursor(),
      )
    }

    proptest! {
      // Feeding the same bytes in different chunkings must produce the
      // same screen state and the same events.
      #[test]
      fn chunking_invariant(
        atoms in proptest::collection::vec(atom(), 0..30),
        splits in proptest::collection::vec(any::<u16>(), 0..8),
      ) {
        let bytes: Vec<u8> = atoms.concat();

        let mut single = screen(12, 6);
        let mut single_events = Vec::new();
        single.process(&bytes, &mut single_events);

        let mut split_points: Vec<usize> = splits
          .iter()
          .map(|s| *s as usize % (bytes.len() + 1))
          .collect();
        split_points.sort_unstable();
        let mut chunked = screen(12, 6);
        let mut chunked_events = Vec::new();
        let mut prev = 0;
        for point in split_points {
          chunked.process(&bytes[prev..point], &mut chunked_events);
          prev = point;
        }
        chunked.process(&bytes[prev..], &mut chunked_events);

        prop_assert_eq!(snapshot(&single), snapshot(&chunked));
        prop_assert_eq!(single_events, chunked_events);
      }
    }
  }
}

impl Screen {
  pub fn snapshot(&self) -> crate::upgrade::snapshot::Screen {
    use super::snapshot::{AttrsTable, attrs_snapshot};
    use crate::upgrade::snapshot::to_base64;
    let mut table = AttrsTable::default();
    let grid = self.grid.snapshot(&mut table);
    let alt_grid = self.alternate_grid.snapshot(&mut table);
    let charset = |set: &CharSet| match set {
      CharSet::Ascii => "ascii",
      CharSet::Uk => "uk",
      CharSet::DecLineDrawing => "dec",
    };
    crate::upgrade::snapshot::Screen {
      grid,
      alt_grid,
      attrs: attrs_snapshot(&self.attrs),
      saved_attrs: attrs_snapshot(&self.saved_attrs),
      modes: self.modes,
      mouse_mode: match self.mouse_protocol_mode {
        MouseProtocolMode::None => "none",
        MouseProtocolMode::Press => "press",
        MouseProtocolMode::PressRelease => "press_release",
        MouseProtocolMode::ButtonMotion => "button_motion",
        MouseProtocolMode::AnyMotion => "any_motion",
      }
      .to_string(),
      mouse_encoding: match self.mouse_protocol_encoding {
        MouseProtocolEncoding::Default => "default",
        MouseProtocolEncoding::Utf8 => "utf8",
        MouseProtocolEncoding::Sgr => "sgr",
      }
      .to_string(),
      g0: charset(&self.g0).to_string(),
      g1: charset(&self.g1).to_string(),
      shift_out: self.shift_out,
      insert: self.insert,
      kitty_flags: self.kitty_flags.clone(),
      alt_kitty_flags: self.alt_kitty_flags.clone(),
      title: self.title.clone(),
      pending_input: to_base64(&self.scanner.pending()),
      attrs_table: table.into_entries(),
    }
  }

  pub fn from_snapshot(
    screen: &crate::upgrade::snapshot::Screen,
  ) -> anyhow::Result<Self> {
    use super::snapshot::attrs_from_snapshot;
    use crate::upgrade::snapshot::from_base64;
    let table: Vec<Attrs> =
      screen.attrs_table.iter().map(attrs_from_snapshot).collect();
    let charset = |name: &str| match name {
      "uk" => CharSet::Uk,
      "dec" => CharSet::DecLineDrawing,
      _ => CharSet::Ascii,
    };
    let mut restored = Self {
      scanner: Scanner::default(),
      grid: super::grid::Grid::from_snapshot(&screen.grid, &table)?,
      alternate_grid: super::grid::Grid::from_snapshot(
        &screen.alt_grid,
        &table,
      )?,
      attrs: attrs_from_snapshot(&screen.attrs),
      saved_attrs: attrs_from_snapshot(&screen.saved_attrs),
      modes: screen.modes,
      mouse_protocol_mode: match screen.mouse_mode.as_str() {
        "press" => MouseProtocolMode::Press,
        "press_release" => MouseProtocolMode::PressRelease,
        "button_motion" => MouseProtocolMode::ButtonMotion,
        "any_motion" => MouseProtocolMode::AnyMotion,
        _ => MouseProtocolMode::None,
      },
      mouse_protocol_encoding: match screen.mouse_encoding.as_str() {
        "utf8" => MouseProtocolEncoding::Utf8,
        "sgr" => MouseProtocolEncoding::Sgr,
        _ => MouseProtocolEncoding::Default,
      },
      g0: charset(&screen.g0),
      g1: charset(&screen.g1),
      shift_out: screen.shift_out,
      insert: screen.insert,
      kitty_flags: screen.kitty_flags.clone(),
      alt_kitty_flags: screen.alt_kitty_flags.clone(),
      title: screen.title.clone(),
    };
    // An unfinished sequence cannot complete or emit anything on its own,
    // so replaying it only rebuilds the parser state.
    let pending = from_base64(&screen.pending_input)?;
    let mut events = Vec::new();
    restored.process(&pending, &mut events);
    Ok(restored)
  }
}

#[cfg(test)]
mod snapshot_tests {
  use super::*;
  use crate::term::ansi::render_screen_ansi;

  fn state(s: &Screen) -> (String, (u16, u16), String, bool, usize) {
    (
      render_screen_ansi(s),
      s.cursor_position(),
      s.title().to_string(),
      s.hide_cursor(),
      s.scrollback(),
    )
  }

  #[test]
  fn round_trip_preserves_rendering_and_parser_state() {
    let mut screen = Screen::new(
      Size {
        width: 20,
        height: 5,
      },
      50,
    );
    let mut events = Vec::new();
    for i in 0..30 {
      screen.process(
        format!("\x1b[1;3{}mline {i} \u{4e2d}\x1b[0m\r\n", i % 8).as_bytes(),
        &mut events,
      );
    }
    screen.process(b"\x1b]2;title\x07\x1b[?25l\x1b[2;3H", &mut events);
    screen.scroll_screen_up(3);
    // Half an escape sequence, then the rest after the round trip.
    screen.process(b"\x1b[3", &mut events);

    let snapshot = screen.snapshot();
    let json = serde_json::to_vec(&snapshot).unwrap();
    let decoded = serde_json::from_slice(&json).unwrap();
    let mut restored = Screen::from_snapshot(&decoded).unwrap();
    assert_eq!(state(&restored), state(&screen));
    assert_eq!(restored.scanner.pending(), b"\x1b[3");

    screen.process(b"0mX", &mut events);
    restored.process(b"0mX", &mut events);
    assert_eq!(state(&restored), state(&screen));
    assert_eq!(restored.attrs, screen.attrs);
  }

  #[test]
  fn alternate_screen_and_modes_survive() {
    let mut screen = Screen::new(
      Size {
        width: 10,
        height: 3,
      },
      0,
    );
    let mut events = Vec::new();
    screen.process(b"main\x1b[?1049h\x1b[?1000h\x1b[?1006halt", &mut events);
    let restored = Screen::from_snapshot(&screen.snapshot()).unwrap();
    assert_eq!(state(&restored), state(&screen));
    assert_eq!(
      restored.mouse_protocol_mode(),
      MouseProtocolMode::PressRelease
    );
    assert_eq!(
      restored.mouse_protocol_encoding(),
      MouseProtocolEncoding::Sgr
    );
    assert_eq!(restored.modes, screen.modes);
  }

  #[test]
  fn cells_keep_their_boundaries() {
    let mut screen = Screen::new(
      Size {
        width: 10,
        height: 2,
      },
      0,
    );
    let mut events = Vec::new();
    // A combining accent, a wide char with its continuation, a gap.
    screen.process("e\u{301}x\u{4e2d}y\x1b[2Cz".as_bytes(), &mut events);
    let restored = Screen::from_snapshot(&screen.snapshot()).unwrap();
    for col in 0..10 {
      let cell = |s: &Screen| s.cell(0, col).map(|c| c.contents().to_string());
      assert_eq!(cell(&restored), cell(&screen), "col {col}");
    }
    assert_eq!(state(&restored), state(&screen));
  }

  #[test]
  fn pending_wrap_survives() {
    let mut screen = Screen::new(
      Size {
        width: 5,
        height: 3,
      },
      0,
    );
    let mut events = Vec::new();
    // Exactly fills the row: the next char wraps.
    screen.process(b"abcde", &mut events);
    let mut restored = Screen::from_snapshot(&screen.snapshot()).unwrap();
    screen.process(b"X", &mut events);
    restored.process(b"X", &mut events);
    assert_eq!(state(&restored), state(&screen));
  }
}
