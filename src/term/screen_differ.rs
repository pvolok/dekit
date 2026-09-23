use unicode_width::UnicodeWidthStr;

use super::{
  Cell, Screen,
  attrs::Attrs,
  common::{CursorStyle, Size},
  grid::{Grid, Pos},
  vt::emit,
};

pub struct ScreenDiffer {
  cells: Vec<Cell>,
  brush: Attrs,
  pos: Pos,
  cursor_pos: Option<Pos>,
  cursor_style: CursorStyle,
}

pub trait BufferView {
  fn size(&self) -> Size;
  fn get_cell(&self, pos: Pos) -> Option<&Cell>;
  fn get_cursor_pos(&self) -> Option<Pos>;
  fn get_cursor_style(&self) -> CursorStyle;
}

impl ScreenDiffer {
  pub fn new() -> Self {
    Self {
      cells: Vec::new(),
      brush: Attrs::default(),
      pos: Pos {
        row: u16::MAX,
        col: u16::MAX,
      },
      cursor_pos: Some(Pos { col: 0, row: 0 }),
      cursor_style: CursorStyle::default(),
    }
  }

  /// Puts a terminal in the state `new` assumes it is in.
  pub fn reset(out: &mut Vec<u8>) {
    out.extend_from_slice(emit::SGR_RESET.as_bytes());
    emit::dec_set(out, emit::DecMode::ShowCursor);
    emit::cursor_style(out, CursorStyle::default());
  }

  pub fn diff<V: BufferView>(&mut self, out: &mut Vec<u8>, view: &V) {
    let prev = &mut self.cells;
    let brush = &mut self.brush;
    let default_cell = Cell::default();

    let size = view.size();
    let mut full_rerender = false;
    let target_len = (size.height * size.width) as usize;
    if target_len != prev.len() {
      full_rerender = true;
      prev.resize(target_len, Cell::default());
      if prev.capacity() > target_len * 2 {
        prev.shrink_to(target_len);
      }
    }
    for y in 0..size.height {
      for x in 0..size.width {
        let offset = (size.width * y + x) as usize;
        let cell = view
          .get_cell(Pos { col: x, row: y })
          .unwrap_or(&default_cell);

        // Check if this cell is a continuation of a wide character.
        // Skip output for it but keep the diff state in sync.
        if x > 0
          && view
            .get_cell(Pos { col: x - 1, row: y })
            .is_some_and(Cell::is_wide)
        {
          if prev[offset] != *cell {
            prev[offset] = cell.clone();
          }
          continue;
        }

        if full_rerender || *cell != prev[offset] {
          let attrs = *cell.attrs();
          emit::sgr(out, *brush, attrs);
          *brush = attrs;

          let pos = Pos { row: y, col: x };
          if self.pos != pos {
            emit::cup(out, pos.row, pos.col);
            self.pos = pos;
          }

          let c = if cell.width() > 0 {
            cell.contents()
          } else {
            " "
          };
          out.extend_from_slice(c.as_bytes());
          self.pos.col = (size.width - 1).min(self.pos.col + c.width() as u16);
          prev[offset] = cell.clone();
        }
      }
    }

    if self.cursor_pos.is_some() != view.get_cursor_pos().is_some() {
      if view.get_cursor_pos().is_some() {
        emit::dec_set(out, emit::DecMode::ShowCursor);
      } else {
        emit::dec_reset(out, emit::DecMode::ShowCursor);
      }
    }
    if let Some(pos) = view.get_cursor_pos() {
      if self.pos != pos {
        emit::cup(out, pos.row, pos.col);
      }
      self.pos = pos;
    }
    self.cursor_pos = view.get_cursor_pos();

    if self.cursor_style != view.get_cursor_style() {
      emit::cursor_style(out, view.get_cursor_style());
      self.cursor_style = view.get_cursor_style();
    }
  }
}

impl BufferView for Vec<Vec<Cell>> {
  fn size(&self) -> Size {
    Size {
      height: self.len() as u16,
      width: self.get(0).map_or(0, |row| row.len() as u16),
    }
  }

  fn get_cell(&self, pos: Pos) -> Option<&Cell> {
    self
      .get(pos.row as usize)
      .map(|row| row.get(pos.col as usize))
      .flatten()
  }

  fn get_cursor_pos(&self) -> Option<Pos> {
    None
  }

  fn get_cursor_style(&self) -> CursorStyle {
    CursorStyle::Default
  }
}

impl BufferView for Grid {
  fn size(&self) -> Size {
    self.size()
  }

  fn get_cell(&self, pos: Pos) -> Option<&Cell> {
    self.visible_cell(pos)
  }

  fn get_cursor_pos(&self) -> Option<Pos> {
    self.cursor_pos
  }

  fn get_cursor_style(&self) -> CursorStyle {
    self.cursor_style
  }
}

impl BufferView for Screen {
  fn size(&self) -> Size {
    self.size()
  }

  fn get_cell(&self, pos: Pos) -> Option<&Cell> {
    self.cell(pos.row, pos.col)
  }

  fn get_cursor_pos(&self) -> Option<Pos> {
    if self.hide_cursor() {
      return None;
    }
    let (row, col) = self.cursor_position();
    Some(Pos { row, col })
  }

  fn get_cursor_style(&self) -> CursorStyle {
    self.cursor_style()
  }
}

#[cfg(test)]
mod tests {
  use crate::term::Color;

  use super::*;

  #[test]
  fn basic() {
    let attrs = Attrs {
      fgcolor: Color::Idx(4),
      ..Default::default()
    };

    let mut differ = ScreenDiffer::new();
    let mut out = Vec::new();

    differ.diff(&mut out, &vec![vec![]]);
    assert_eq!(out, b"\x1b[?25l"); // Hide cursor

    let screen = vec![vec![
      Cell::new("1"),
      Cell::new("2"),
      Cell::new("3").with_attrs(attrs),
      Cell::new("4").with_attrs(attrs),
      Cell::new("5"),
    ]];
    out.clear();
    differ.diff(&mut out, &screen);
    assert_eq!(out, b"\x1b[1;1H12\x1b[38;5;4m34\x1b[39m5");

    let screen = vec![vec![
      Cell::new("1"),
      Cell::new("_"),
      Cell::new("3"),
      Cell::new("4").with_attrs(attrs),
      Cell::new("5"),
    ]];
    out.clear();
    differ.diff(&mut out, &screen);
    assert_eq!(out, b"\x1b[1;2H_3");
  }

  #[test]
  fn wide_char_continuation_not_overwritten() {
    let mut differ = ScreenDiffer::new();
    let mut out = Vec::new();

    // "A测B": 测 is a wide character occupying two columns. The empty cell
    // after it is the continuation (right half) and must not be drawn,
    // otherwise the space would paint over the right half of the glyph.
    let screen = vec![vec![
      Cell::new("A"),
      Cell::new("测"),
      Cell::default(),
      Cell::new("B"),
    ]];
    differ.diff(&mut out, &screen);
    assert_eq!(out, "\x1b[1;1HA测B\x1b[?25l".as_bytes());

    // A redraw of the identical screen must not emit anything (the diff
    // state stays in sync even though the continuation cell was skipped).
    out.clear();
    differ.diff(&mut out, &screen);
    assert_eq!(out, b"");
  }
}
