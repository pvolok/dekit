#![allow(clippy::as_conversions, clippy::pedantic)]

use std::collections::VecDeque;

use serde::{Deserialize, Serialize};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use super::common::{CursorStyle, Size};

use super::{Cell, attrs::Attrs, row::Row};

#[derive(Clone, Debug)]
pub struct Grid {
  size: Size,
  pos: Pos,
  saved_pos: Pos,
  scroll_top: u16,
  scroll_bottom: u16,
  rows: VecDeque<super::row::Row>,
  /// Number of visible rows that were printed to. On resizing unused rows can
  /// be removed.
  used_rows: u16,
  origin_mode: bool,
  saved_origin_mode: bool,
  scrollback_len: usize,
  scrollback_offset: usize,

  pub cursor_pos: Option<Pos>,
  pub cursor_style: CursorStyle,
}

impl Grid {
  pub fn new(size: Size, scrollback_len: usize) -> Self {
    let size = Size {
      height: size.height.max(1),
      width: size.width.max(1),
    };

    let mut rows = VecDeque::with_capacity(size.height.into());
    for _ in 0..size.height {
      rows.push_back(Row::new(size.width));
    }

    Self {
      size,
      pos: Pos::default(),
      saved_pos: Pos::default(),
      scroll_top: 0,
      scroll_bottom: size.height - 1,
      rows,
      used_rows: 0,
      origin_mode: false,
      saved_origin_mode: false,
      scrollback_len,
      scrollback_offset: 0,

      cursor_pos: None,
      cursor_style: CursorStyle::Default,
    }
  }

  pub fn get_selected_text(
    &self,
    low_x: i32,
    low_y: i32,
    high_x: i32,
    high_y: i32,
  ) -> String {
    let lines = self
      .rows
      .iter()
      .skip((self.row0() as i32 + low_y) as usize)
      .take((high_y - low_y) as usize + 1)
      .enumerate();

    let mut contents = String::new();

    let lines_len = high_y - low_y + 1;
    for (i, row) in lines {
      let i = i as i32;
      let start = if i == 0 { low_x } else { 0 };

      let width: i32 = row.cols().into();
      let width = if i == lines_len - 1 {
        width.min(high_x + 1)
      } else {
        width
      };
      let width = width - start;

      row.write_contents(&mut contents, start as u16, width as u16, false);
      if i != lines_len - 1 && !row.wrapped() {
        contents.push('\n');
      }
    }

    contents
  }

  fn new_row(&self) -> Row {
    Row::new(self.size.width)
  }

  pub fn clear(&mut self) {
    self.pos = Pos::default();
    self.saved_pos = Pos::default();
    for row in self.drawing_rows_mut() {
      row.clear(Attrs::default());
    }
    self.scroll_top = 0;
    self.scroll_bottom = self.size.height - 1;
    self.used_rows = 0;
    self.origin_mode = false;
    self.saved_origin_mode = false;
  }

  pub fn size(&self) -> Size {
    self.size
  }

  pub fn set_size(&mut self, size: Size) {
    let size = Size {
      height: size.height.max(1),
      width: size.width.max(1),
    };

    if self.size == size {
      return;
    }

    let mut acc = VecDeque::with_capacity(self.rows.capacity());

    let prev_abs_pos_row = self.row0() + self.pos.row as usize;
    let mut abs_pos_row = 0;
    let max_rows =
      self.rows.len() - self.size.height as usize + self.used_rows as usize;
    let mut rows = self.rows.drain(..).enumerate();

    let mut line = Vec::new();
    'rows: while let Some((i, mut row)) = rows.next() {
      if i >= max_rows {
        break;
      }
      line.clear();

      if prev_abs_pos_row == i {
        abs_pos_row = acc.len();
      }
      row.take_cells(&mut line);

      while row.wrapped() {
        if let Some((i, next_row)) = rows.next() {
          if i >= max_rows {
            break 'rows;
          }

          if prev_abs_pos_row == i {
            abs_pos_row = acc.len();
          }
          next_row.take_cells(&mut line);
          row = next_row;
        } else {
          break;
        }
      }

      let mut i = 0;
      loop {
        let mut new_row = Row::new(size.width);

        let mut j = 0;
        while line.len() > i && j + line[i].width() <= size.width {
          if let Some(target) = new_row.get_mut(j) {
            *target = line[i].clone();
          }

          j += 1;
          i += 1;
        }

        if line.len() > i {
          new_row.wrap(true);
          acc.push_back(new_row);
        } else {
          acc.push_back(new_row);
          break;
        }
      }
    }
    drop(rows);
    self.rows = acc;

    let max_rows = self.scrollback_len + size.height as usize;
    self.rows.drain(..self.rows.len().saturating_sub(max_rows));
    self.scrollback_offset =
      (self.rows.len().saturating_sub(size.height as usize))
        .min(self.scrollback_offset);

    if self.scroll_bottom == self.size.height - 1 {
      self.scroll_bottom = size.height - 1;
    }

    self.size = size;

    if self.scroll_bottom >= size.height {
      self.scroll_bottom = size.height - 1;
    }
    if self.scroll_bottom < self.scroll_top {
      self.scroll_top = 0;
    }

    self.used_rows = u16::try_from(self.rows.len())
      .unwrap_or_default()
      .min(self.size.height);
    while self.rows.len() < self.size.height.into() {
      self.rows.push_back(self.new_row());
    }
    self.pos.row = u16::try_from(abs_pos_row.saturating_sub(self.row0()))
      .unwrap_or_default();

    self.row_clamp_top(false);
    self.row_clamp_bottom(false);
    self.col_clamp();

    while self.rows.len() < self.size.height.into() {
      self.rows.push_back(self.new_row());
    }
  }

  pub fn pos(&self) -> Pos {
    self.pos
  }

  pub fn set_pos(&mut self, mut pos: Pos) {
    if self.origin_mode {
      pos.row = pos.row.saturating_add(self.scroll_top);
    }
    self.pos = pos;
    self.row_clamp_top(self.origin_mode);
    self.row_clamp_bottom(self.origin_mode);
    self.col_clamp();
  }

  pub fn save_cursor(&mut self) {
    self.saved_pos = self.pos;
    self.saved_origin_mode = self.origin_mode;
  }

  pub fn restore_cursor(&mut self) {
    self.pos = self.saved_pos;
    self.origin_mode = self.saved_origin_mode;
  }

  fn row0(&self) -> usize {
    self.rows.len() - self.size.height as usize
  }

  fn drawing_rows_mut(&mut self) -> impl Iterator<Item = &mut Row> {
    let row0 = self.row0();
    self.rows.iter_mut().skip(row0)
  }

  pub fn visible_row(&self, row: u16) -> Option<&Row> {
    let first = self.row0().saturating_sub(self.scrollback_offset);
    self.rows.get(first + usize::from(row))
  }

  pub fn drawing_row(&self, row: u16) -> Option<&Row> {
    self.rows.get(self.row0() + usize::from(row))
  }

  pub fn drawing_row_mut(&mut self, row: u16) -> Option<&mut Row> {
    let row0 = self.row0();
    self.rows.get_mut(row0 + usize::from(row))
  }

  pub fn current_row_mut(&mut self) -> &mut Row {
    self
      .drawing_row_mut(self.pos.row)
      // we assume self.pos.row is always valid
      .unwrap()
  }

  pub fn visible_cell(&self, pos: Pos) -> Option<&Cell> {
    self.visible_row(pos.row).and_then(|r| r.get(pos.col))
  }

  pub fn drawing_cell(&self, pos: Pos) -> Option<&Cell> {
    self.drawing_row(pos.row).and_then(|r| r.get(pos.col))
  }

  pub fn drawing_cell_mut(&mut self, pos: Pos) -> Option<&mut Cell> {
    self.used_rows = self.used_rows.max(pos.row + 1);
    self
      .drawing_row_mut(pos.row)
      .and_then(|r| r.get_mut(pos.col))
  }

  pub fn scrollback_len(&self) -> usize {
    self.scrollback_len
  }

  pub fn scrollback(&self) -> usize {
    self.scrollback_offset
  }

  pub fn set_scrollback(&mut self, rows: usize) {
    self.scrollback_offset = rows.min(self.row0());
  }

  pub fn erase_all(&mut self, attrs: Attrs) {
    self.used_rows = 0;
    for row in self.drawing_rows_mut() {
      row.clear(attrs);
    }
  }

  pub fn erase_all_forward(&mut self, attrs: Attrs) {
    self.used_rows = self.used_rows.min(self.pos.row + 1);
    let pos = self.pos;
    for row in self.drawing_rows_mut().skip(usize::from(pos.row) + 1) {
      row.clear(attrs);
    }

    self.erase_row_forward(attrs);
  }

  pub fn erase_all_backward(&mut self, attrs: Attrs) {
    let pos = self.pos;
    for row in self.drawing_rows_mut().take(usize::from(pos.row)) {
      row.clear(attrs);
    }

    self.erase_row_backward(attrs);
  }

  pub fn erase_row(&mut self, attrs: Attrs) {
    self.current_row_mut().clear(attrs);
  }

  pub fn erase_row_forward(&mut self, attrs: Attrs) {
    let size = self.size;
    let pos = self.pos;
    let row = self.current_row_mut();
    for col in pos.col..size.width {
      row.erase(col, attrs);
    }
  }

  pub fn erase_row_backward(&mut self, attrs: Attrs) {
    let size = self.size;
    let pos = self.pos;
    let row = self.current_row_mut();
    for col in 0..=pos.col.min(size.width - 1) {
      row.erase(col, attrs);
    }
  }

  pub fn insert_cells(&mut self, count: u16) {
    let size = self.size;
    let pos = self.pos;
    let row = self.current_row_mut();
    for _ in 0..count {
      row.insert(pos.col, Cell::default());
    }
    row.truncate(size.width);
  }

  pub fn delete_cells(&mut self, count: u16) {
    let size = self.size;
    let pos = self.pos;
    let row = self.current_row_mut();
    for _ in 0..(count.min(size.width - pos.col)) {
      row.remove(pos.col);
    }
    row.resize(size.width, Cell::default());
  }

  pub fn erase_cells(&mut self, count: u16, attrs: Attrs) {
    let size = self.size;
    let pos = self.pos;
    let row = self.current_row_mut();
    for col in pos.col..((pos.col.saturating_add(count)).min(size.width)) {
      row.erase(col, attrs);
    }
  }

  pub fn insert_lines(&mut self, count: u16) {
    let row0 = self.row0();
    for _ in 0..count {
      self.rows.remove(row0 + usize::from(self.scroll_bottom));
      self
        .rows
        .insert(row0 + usize::from(self.pos.row), self.new_row());
      // self.scroll_bottom is maintained to always be a valid row
      self.rows[row0 + usize::from(self.scroll_bottom)].wrap(false);
    }
  }

  pub fn delete_lines(&mut self, count: u16, blank_attrs: Attrs) {
    let row0 = self.row0();
    for _ in 0..(count.min(self.size.height - self.pos.row)) {
      let row = Row::new_with_attrs(self.size.width, blank_attrs);
      self
        .rows
        .insert(row0 + usize::from(self.scroll_bottom) + 1, row);
      self.rows.remove(row0 + usize::from(self.pos.row));
    }
  }

  pub fn scroll_up(&mut self, count: u16) {
    for _ in 0..(count.min(self.size.height - self.scroll_top)) {
      let row0 = self.row0();
      self
        .rows
        .insert(row0 + usize::from(self.scroll_bottom) + 1, self.new_row());
      let removed = self.rows.remove(row0 + usize::from(self.scroll_top));
      if self.scrollback_len > 0 && !self.scroll_region_active() {
        if let Some(removed) = removed {
          self.rows.insert(row0, removed);
        }
        let max_rows = self.scrollback_len + self.size.height as usize;
        self.rows.drain(..self.rows.len().saturating_sub(max_rows));
        if self.scrollback_offset > 0 {
          self.scrollback_offset = (self.rows.len()
            - self.size.height as usize)
            .min(self.scrollback_offset + 1);
        }
      }
    }
  }

  pub fn scroll_down(&mut self, count: u16) {
    for _ in 0..count {
      let row0 = self.row0();
      self.rows.remove(row0 + usize::from(self.scroll_bottom));
      self
        .rows
        .insert(row0 + usize::from(self.scroll_top), self.new_row());
      // self.scroll_bottom is maintained to always be a valid row
      self.rows[row0 + usize::from(self.scroll_bottom)].wrap(false);
    }
  }

  pub fn set_scroll_region(&mut self, top: u16, bottom: u16) {
    let bottom = bottom.min(self.size().height - 1);
    if top < bottom {
      self.scroll_top = top;
      self.scroll_bottom = bottom;
    } else {
      self.scroll_top = 0;
      self.scroll_bottom = self.size().height - 1;
    }
    self.pos.row = self.scroll_top;
    self.pos.col = 0;
  }

  fn in_scroll_region(&self) -> bool {
    self.pos.row >= self.scroll_top && self.pos.row <= self.scroll_bottom
  }

  fn scroll_region_active(&self) -> bool {
    self.scroll_top != 0 || self.scroll_bottom != self.size.height - 1
  }

  pub fn set_origin_mode(&mut self, mode: bool) {
    self.origin_mode = mode;
    self.set_pos(Pos { row: 0, col: 0 });
  }

  pub fn row_inc_clamp(&mut self, count: u16) {
    let in_scroll_region = self.in_scroll_region();
    self.pos.row = self.pos.row.saturating_add(count);
    self.row_clamp_bottom(in_scroll_region);
  }

  pub fn row_inc_scroll(&mut self, count: u16) -> u16 {
    let in_scroll_region = self.in_scroll_region();
    self.pos.row = self.pos.row.saturating_add(count);
    let lines = self.row_clamp_bottom(in_scroll_region);
    if in_scroll_region {
      self.scroll_up(lines);
      lines
    } else {
      0
    }
  }

  pub fn row_dec_clamp(&mut self, count: u16) {
    let in_scroll_region = self.in_scroll_region();
    self.pos.row = self.pos.row.saturating_sub(count);
    self.row_clamp_top(in_scroll_region);
  }

  pub fn row_dec_scroll(&mut self, count: u16) {
    let in_scroll_region = self.in_scroll_region();
    // need to account for clamping by both row_clamp_top and by
    // saturating_sub
    let extra_lines = count.saturating_sub(self.pos.row);
    self.pos.row = self.pos.row.saturating_sub(count);
    let lines = self.row_clamp_top(in_scroll_region);
    self.scroll_down(lines + extra_lines);
  }

  pub fn row_set(&mut self, i: u16) {
    self.pos.row = i;
    self.row_clamp();
  }

  pub fn col_inc(&mut self, count: u16) {
    self.pos.col = self.pos.col.saturating_add(count);
  }

  pub fn col_inc_clamp(&mut self, count: u16) {
    self.pos.col = self.pos.col.saturating_add(count);
    self.col_clamp();
  }

  pub fn col_dec(&mut self, count: u16) {
    self.pos.col = self.pos.col.saturating_sub(count);
  }

  pub fn col_tab(&mut self) {
    self.pos.col -= self.pos.col % 8;
    self.pos.col += 8;
    self.col_clamp();
  }

  pub fn col_set(&mut self, i: u16) {
    self.pos.col = i;
    self.col_clamp();
  }

  /// Writes a run of single-width ASCII chars into one row starting at
  /// `pos`, without moving the cursor. The run must fit within the row.
  pub fn write_ascii_row(&mut self, pos: Pos, bytes: &[u8], attrs: Attrs) {
    if bytes.is_empty() {
      return;
    }
    // Overwriting the right half of a wide char clears its left half.
    if self.is_wide_continuation(pos) {
      if let Some(prev) = self.drawing_cell_mut(Pos {
        row: pos.row,
        col: pos.col - 1,
      }) {
        prev.clear(attrs);
      }
    }
    // Overwriting the left half of a wide char at the end of the run
    // clears the continuation cell it leaves behind.
    let last = Pos {
      row: pos.row,
      col: pos.col + bytes.len() as u16 - 1,
    };
    if self
      .drawing_cell(last)
      .is_some_and(super::cell::Cell::is_wide)
    {
      if let Some(next) = self.drawing_cell_mut(Pos {
        row: last.row,
        col: last.col + 1,
      }) {
        next.set(' ', attrs);
      }
    }
    self.used_rows = self.used_rows.max(pos.row + 1);
    if let Some(row) = self.drawing_row_mut(pos.row) {
      for (i, b) in bytes.iter().enumerate() {
        if let Some(cell) = row.get_mut(pos.col + i as u16) {
          cell.set(*b as char, attrs);
        }
      }
    }
  }

  pub fn col_wrap(&mut self, width: u16, wrap: bool) {
    if self.pos.col > self.size.width - width {
      let mut prev_pos = self.pos;
      self.pos.col = 0;
      let scrolled = self.row_inc_scroll(1);
      prev_pos.row -= scrolled;
      let new_pos = self.pos;
      self
        .drawing_row_mut(prev_pos.row)
        // we assume self.pos.row is always valid, and so prev_pos.row
        // must be valid because it is always less than or equal to
        // self.pos.row
        .unwrap()
        .wrap(wrap && prev_pos.row + 1 == new_pos.row);
    }
  }

  fn row_clamp_top(&mut self, limit_to_scroll_region: bool) -> u16 {
    if limit_to_scroll_region && self.pos.row < self.scroll_top {
      let rows = self.scroll_top - self.pos.row;
      self.pos.row = self.scroll_top;
      rows
    } else {
      0
    }
  }

  fn row_clamp_bottom(&mut self, limit_to_scroll_region: bool) -> u16 {
    let bottom = if limit_to_scroll_region {
      self.scroll_bottom
    } else {
      self.size.height - 1
    };
    if self.pos.row > bottom {
      let rows = self.pos.row - bottom;
      self.pos.row = bottom;
      rows
    } else {
      0
    }
  }

  fn row_clamp(&mut self) {
    if self.pos.row > self.size.height - 1 {
      self.pos.row = self.size.height - 1;
    }
  }

  fn col_clamp(&mut self) {
    if self.pos.col > self.size.width - 1 {
      self.pos.col = self.size.width - 1;
    }
  }

  pub(crate) fn is_wide_continuation(&self, pos: Pos) -> bool {
    self
      .rows
      .get(self.row0() + pos.row as usize)
      .is_some_and(|r| r.is_wide_continuation(pos.col))
  }
}

pub enum BorderType {
  Thick,
  Plain,
  Rounded,
}

impl BorderType {
  pub fn chars(&self) -> BorderChars {
    match self {
      BorderType::Thick => BorderChars {
        tl: '┏',
        tr: '┓',
        br: '┛',
        bl: '┗',
        hor: '━',
        ver: '┃',
        t_top: '┳',
        t_bottom: '┻',
        t_left: '┣',
        t_right: '┫',
        cross: '╋',
      },
      BorderType::Plain => BorderChars {
        tl: '┌',
        tr: '┐',
        br: '┘',
        bl: '└',
        hor: '─',
        ver: '│',
        t_top: '┬',
        t_bottom: '┴',
        t_left: '├',
        t_right: '┤',
        cross: '┼',
      },
      BorderType::Rounded => BorderChars {
        tl: '╭',
        tr: '╮',
        br: '╯',
        bl: '╰',
        hor: '─',
        ver: '│',
        t_top: '┬',
        t_bottom: '┴',
        t_left: '├',
        t_right: '┤',
        cross: '┼',
      },
    }
  }
}

pub struct BorderChars {
  pub tl: char,
  pub tr: char,
  pub br: char,
  pub bl: char,
  pub hor: char,
  pub ver: char,
  pub t_top: char,
  pub t_bottom: char,
  pub t_left: char,
  pub t_right: char,
  pub cross: char,
}

impl Grid {
  pub fn area(&self) -> Rect {
    let size = self.size();
    Rect {
      x: 0,
      y: 0,
      width: size.width,
      height: size.height,
    }
  }

  pub fn draw_block(&mut self, area: Rect, chars: &BorderChars, attrs: Attrs) {
    if area.width < 2 || area.height < 2 {
      return;
    }

    for y in [area.y, area.y + area.height - 1] {
      for x in area.x + 1..area.x + area.width - 1 {
        if let Some(cell) = self.drawing_cell_mut(Pos { col: x, row: y }) {
          cell.set(chars.hor, attrs);
        }
      }
    }
    for x in [area.x, area.x + area.width - 1] {
      for y in area.y + 1..area.y + area.height - 1 {
        if let Some(cell) = self.drawing_cell_mut(Pos { col: x, row: y }) {
          cell.set(chars.ver, attrs);
        }
      }
    }
    if let Some(cell) = self.drawing_cell_mut(Pos {
      col: area.x,
      row: area.y,
    }) {
      cell.set(chars.tl, attrs);
    }
    if let Some(cell) = self.drawing_cell_mut(Pos {
      col: area.x + area.width - 1,
      row: area.y,
    }) {
      cell.set(chars.tr, attrs);
    }
    if let Some(cell) = self.drawing_cell_mut(Pos {
      col: area.x + area.width - 1,
      row: area.y + area.height - 1,
    }) {
      cell.set(chars.br, attrs);
    }
    if let Some(cell) = self.drawing_cell_mut(Pos {
      col: area.x,
      row: area.y + area.height - 1,
    }) {
      cell.set(chars.bl, attrs);
    }
  }

  pub fn draw_text(&mut self, area: Rect, text: &str, attrs: Attrs) -> Rect {
    let mut x = area.x;
    for g in text.graphemes(true) {
      if let Some(cell) = self.drawing_cell_mut(Pos {
        col: x,
        row: area.y,
      }) {
        let w = g.width() as u16;
        if x + w - area.x <= area.width {
          cell.set_str(g);
          cell.set_attrs(attrs);
          x += w;
        } else {
          break;
        }
      }
    }
    Rect {
      x: area.x,
      y: area.y,
      width: x - area.x,
      height: 1,
    }
  }

  /// Like `draw_text`, but fills the remaining width of `area` with spaces.
  pub fn draw_line(&mut self, area: Rect, text: &str, attrs: Attrs) -> Rect {
    let used = self.draw_text(area, text, attrs);
    let remaining_x = area.x + used.width;
    let remaining_w = area.width.saturating_sub(used.width);
    if remaining_w > 0 {
      self.fill_area(
        Rect::new(remaining_x, area.y, remaining_w, 1),
        ' ',
        attrs,
      );
    }
    Rect {
      x: area.x,
      y: area.y,
      width: area.width,
      height: 1,
    }
  }

  pub fn fill_area(&mut self, area: Rect, c: char, attrs: Attrs) {
    for col in area.x..area.x + area.width {
      for row in area.y..area.y + area.height {
        if let Some(cell) = self.drawing_cell_mut(Pos { col, row }) {
          cell.set(c, attrs);
        }
      }
    }
  }
}

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct Pos {
  pub col: u16,
  pub row: u16,
}

#[derive(Copy, Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct Rect {
  pub x: u16,
  pub y: u16,
  pub width: u16,
  pub height: u16,
}

impl Rect {
  pub fn new(x: u16, y: u16, width: u16, height: u16) -> Self {
    Rect {
      x,
      y,
      width,
      height,
    }
  }
}

impl Rect {
  pub fn size(&self) -> Size {
    Size {
      height: self.height,
      width: self.width,
    }
  }

  pub fn right(&self) -> u16 {
    self.x + self.width
  }

  pub fn bottom(&self) -> u16 {
    self.y + self.height
  }

  pub fn contains(&self, x: u16, y: u16) -> bool {
    x >= self.x && x < self.right() && y >= self.y && y < self.bottom()
  }

  pub fn inner<T: Into<Margin>>(self, margin: T) -> Self {
    let margin = margin.into();
    Self {
      x: self.x + margin.left.min(self.width),
      y: self.y + margin.top.min(self.height),
      width: self.width.saturating_sub(margin.left + margin.right),
      height: self.height.saturating_sub(margin.top + margin.bottom),
    }
  }

  /// Returns a 1-height sub-rect for the `n`-th row (0-indexed) within this
  /// rect. Returns `None` if `n >= self.height`.
  pub fn row(self, n: u16) -> Option<Self> {
    if n >= self.height {
      return None;
    }
    Some(Rect {
      x: self.x,
      y: self.y + n,
      width: self.width,
      height: 1,
    })
  }

  pub fn split_h(self, len: u16) -> (Self, Self) {
    let len = len.min(self.height);
    let top = Rect {
      height: len,
      ..self
    };
    let bot = Rect {
      y: self.y + len,
      height: self.height - len,
      ..self
    };
    (top, bot)
  }

  pub fn split_v(self, len: u16) -> (Self, Self) {
    let len = len.min(self.width);
    let left = Rect { width: len, ..self };
    let right = Rect {
      x: self.x + len,
      width: self.width - len,
      ..self
    };
    (left, right)
  }

  pub fn move_left(self, offset: i32) -> Self {
    let offset = offset.min(self.width as i32);
    Rect {
      x: (self.x as i32 + offset) as u16,
      width: (self.width as i32 - offset) as u16,
      ..self
    }
  }

  #[allow(dead_code)]
  pub fn move_right(self, offset: i32) -> Self {
    let offset = offset.max(-(self.width as i32));
    Rect {
      width: (self.width as i32 + offset) as u16,
      ..self
    }
  }

  #[allow(dead_code)]
  pub fn move_top(self, offset: i32) -> Self {
    let offset = offset.min(self.height as i32);
    Rect {
      y: (self.y as i32 + offset) as u16,
      height: (self.height as i32 - offset) as u16,
      ..self
    }
  }

  #[allow(dead_code)]
  pub fn move_bottom(self, offset: i32) -> Self {
    let offset = offset.max(-(self.height as i32));
    Rect {
      height: (self.height as i32 + offset) as u16,
      ..self
    }
  }
}

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct Margin {
  pub top: u16,
  pub right: u16,
  pub bottom: u16,
  pub left: u16,
}

impl From<u16> for Margin {
  fn from(n: u16) -> Self {
    Margin {
      top: n,
      right: n,
      bottom: n,
      left: n,
    }
  }
}

impl From<(u16, u16)> for Margin {
  fn from((ver, hor): (u16, u16)) -> Self {
    Margin {
      top: ver,
      right: hor,
      bottom: ver,
      left: hor,
    }
  }
}

impl From<(u16, u16, u16, u16)> for Margin {
  fn from((top, right, bottom, left): (u16, u16, u16, u16)) -> Self {
    Margin {
      top,
      right,
      bottom,
      left,
    }
  }
}

impl Grid {
  pub fn snapshot(
    &self,
    table: &mut super::snapshot::AttrsTable,
  ) -> crate::upgrade::snapshot::Grid {
    crate::upgrade::snapshot::Grid {
      width: self.size.width,
      height: self.size.height,
      pos: (self.pos.row, self.pos.col),
      saved_pos: (self.saved_pos.row, self.saved_pos.col),
      scroll_top: self.scroll_top,
      scroll_bottom: self.scroll_bottom,
      rows: self.rows.iter().map(|row| row.snapshot(table)).collect(),
      used_rows: self.used_rows,
      origin_mode: self.origin_mode,
      saved_origin_mode: self.saved_origin_mode,
      scrollback_len: self.scrollback_len,
      scrollback_offset: self.scrollback_offset,
      cursor_pos: self.cursor_pos.map(|pos| (pos.row, pos.col)),
      cursor_style: self.cursor_style as u8,
    }
  }

  pub fn from_snapshot(
    grid: &crate::upgrade::snapshot::Grid,
    table: &[Attrs],
  ) -> anyhow::Result<Self> {
    let size = Size {
      width: grid.width.max(1),
      height: grid.height.max(1),
    };
    let mut rows = VecDeque::with_capacity(grid.rows.len());
    for row in &grid.rows {
      rows.push_back(Row::from_snapshot(row, table)?);
    }
    if rows.len() < usize::from(size.height) {
      anyhow::bail!("grid has fewer rows than its height");
    }
    // The text cursor may sit one past the last column: a pending wrap.
    let cursor = |(row, col): (u16, u16)| Pos {
      row: row.min(size.height - 1),
      col: col.min(size.width),
    };
    let clamp = |(row, col): (u16, u16)| Pos {
      row: row.min(size.height - 1),
      col: col.min(size.width - 1),
    };
    Ok(Self {
      size,
      pos: cursor(grid.pos),
      saved_pos: cursor(grid.saved_pos),
      scroll_top: grid.scroll_top.min(size.height - 1),
      scroll_bottom: grid.scroll_bottom.min(size.height - 1),
      rows,
      used_rows: grid.used_rows.min(size.height),
      origin_mode: grid.origin_mode,
      saved_origin_mode: grid.saved_origin_mode,
      scrollback_len: grid.scrollback_len,
      scrollback_offset: grid.scrollback_offset,
      cursor_pos: grid.cursor_pos.map(clamp),
      cursor_style: CursorStyle::from_u16(u16::from(grid.cursor_style)),
    })
  }
}
