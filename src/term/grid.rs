//! The live screen of a terminal: its rows, cursor, margins and tab
//! stops, plus the history the rows scroll into and the view over it.
//!
//! Rows are `Arc`s so a published frame shares them and the next write
//! copies the row (`Arc::make_mut`); the interned tables are shared the
//! same way.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::{
  cell::{Cell, CellText, TextKind},
  common::{CursorStyle, Size},
  frame::Tables,
  history::{History, Line},
  image::{Image, ImageKey, ImageTable, Placement, VisiblePlacement},
  row::Row,
  style::{Link, LinkId, Style, StyleId, StyleTable},
};

#[derive(Clone, Debug)]
pub struct Grid {
  size: Size,
  rows: Vec<Arc<Row>>,
  history: History,
  /// Rows scrolled back into history; 0 shows the live screen.
  view_offset: usize,
  /// The history rows in view, materialized at the current width.
  view_above: Vec<Arc<Row>>,
  /// Rows scrolled into history since `view_above` was built; it is
  /// rebuilt once the output chunk is processed (`settle_view`).
  view_stale: bool,
  tables: Tables,
  pos: Pos,
  saved_pos: Pos,
  scroll_top: u16,
  scroll_bottom: u16,
  /// Inclusive left and right margins (DECLRMM); the full width unless
  /// a program set them.
  margin_left: u16,
  margin_right: u16,
  origin_mode: bool,
  saved_origin_mode: bool,
  tabs: Vec<bool>,
  /// The last style interned, so a run of one style costs one lookup.
  style_cache: (Style, StyleId),
  /// Rows that ever scrolled off the top: the absolute row of row 0.
  row_base: u64,
  /// Image placements, anchored to absolute rows.
  placements: Vec<Placement>,
  /// `(image id, placement id)` pairs placeholder cells refer to.
  placeholder_slots: Vec<(u32, u32)>,
  /// Images whose cell-bound placements were written over, for the
  /// screen to free (`take_dropped_images`).
  dropped_images: Vec<ImageKey>,
  /// Images composed into this grid from other frames (a UI grid),
  /// shared with the frames published of it.
  extra_images: Arc<ImageTable>,
  /// The lines this grid logs (`dekit logs`); only a task's main screen
  /// has one.
  log: Option<LineLog>,

  /// For a grid drawn as a UI: where its cursor shows.
  pub cursor_pos: Option<Pos>,
  pub cursor_style: CursorStyle,
}

impl Grid {
  pub fn new(size: Size, scrollback_len: usize) -> Self {
    let size = Size {
      height: size.height.max(1),
      width: size.width.max(1),
    };
    let rows = (0..size.height)
      .map(|_| Arc::new(Row::new(size.width)))
      .collect();
    Grid {
      size,
      rows,
      history: {
        let mut history = History::new(scrollback_len);
        history.set_width(size.width);
        history
      },
      view_offset: 0,
      view_above: Vec::new(),
      view_stale: false,
      tables: Tables::default(),
      pos: Pos::default(),
      saved_pos: Pos::default(),
      scroll_top: 0,
      scroll_bottom: size.height - 1,
      margin_left: 0,
      margin_right: size.width - 1,
      origin_mode: false,
      saved_origin_mode: false,
      tabs: default_tabs(size.width),
      style_cache: (Style::DEFAULT, StyleId::DEFAULT),
      row_base: 0,
      placements: Vec::new(),
      placeholder_slots: Vec::new(),
      dropped_images: Vec::new(),
      extra_images: Arc::default(),
      log: None,
      cursor_pos: None,
      cursor_style: CursorStyle::Default,
    }
  }

  pub fn size(&self) -> Size {
    self.size
  }

  pub fn area(&self) -> Rect {
    Rect::new(0, 0, self.size.width, self.size.height)
  }

  pub fn tables(&self) -> &Tables {
    &self.tables
  }

  pub fn history(&self) -> &History {
    &self.history
  }

  // Tables

  pub fn intern_style(&mut self, style: Style) -> StyleId {
    if self.style_cache.0 == style {
      return self.style_cache.1;
    }
    let id = StyleTable::intern_in(&mut self.tables.styles, style);
    self.style_cache = (style, id);
    id
  }

  pub fn style_of(&self, id: StyleId) -> Style {
    self.tables.styles.get(id)
  }

  pub fn intern_link(&mut self, link: Link) -> LinkId {
    Arc::make_mut(&mut self.tables.links).intern(link)
  }

  pub fn text_of<'a>(&'a self, cell: Cell, buf: &'a mut [u8; 4]) -> &'a str {
    cell.text.as_str(buf, &self.tables.graphemes)
  }

  /// A cell for `text` (one scalar or a cluster) at `width`.
  pub fn cell_text(&mut self, text: &str, width: u8) -> CellText {
    CellText::of(text, width, &mut self.tables.graphemes)
  }

  // Rows

  /// The live rows, whatever the view shows.
  pub fn rows(&self) -> &[Arc<Row>] {
    &self.rows
  }

  /// The rows in view: history rows when scrolled back, then live rows.
  pub fn visible_rows(&self) -> Vec<Arc<Row>> {
    if self.view_offset == 0 {
      return self.rows.clone();
    }
    let mut rows = self.view_above.clone();
    let live = usize::from(self.size.height).saturating_sub(rows.len());
    rows.extend(self.rows.iter().take(live).cloned());
    rows
  }

  pub fn visible_row(&self, row: u16) -> Option<&Row> {
    let row = usize::from(row);
    if self.view_offset == 0 {
      return self.rows.get(row).map(Arc::as_ref);
    }
    match row.checked_sub(self.view_above.len()) {
      None => self.view_above.get(row).map(Arc::as_ref),
      Some(live) => self.rows.get(live).map(Arc::as_ref),
    }
  }

  pub fn visible_cell(&self, pos: Pos) -> Option<Cell> {
    self.visible_row(pos.row).and_then(|r| r.get(pos.col))
  }

  pub fn drawing_row(&self, row: u16) -> Option<&Row> {
    self.rows.get(usize::from(row)).map(Arc::as_ref)
  }

  pub fn drawing_row_mut(&mut self, row: u16) -> Option<&mut Row> {
    self.rows.get_mut(usize::from(row)).map(Arc::make_mut)
  }

  fn row_mut(&mut self, row: u16) -> &mut Row {
    Arc::make_mut(&mut self.rows[usize::from(row)])
  }

  fn current_row_mut(&mut self) -> &mut Row {
    let row = self.pos.row;
    self.row_mut(row)
  }

  pub fn drawing_cell(&self, pos: Pos) -> Option<Cell> {
    self.drawing_row(pos.row).and_then(|r| r.get(pos.col))
  }

  /// Puts text at `pos`, keeping wide cells whole on both sides. A wide
  /// text at the last column is written narrow-blanked: the caller wraps
  /// before that.
  pub fn write_cell(&mut self, pos: Pos, text: CellText, style: StyleId) {
    let width = self.size.width;
    if pos.col >= width || pos.row >= self.size.height {
      return;
    }
    self.touch(
      pos.row,
      pos.col,
      pos.col + if text.is_wide() { 2 } else { 1 },
    );
    let row = self.row_mut(pos.row);
    row.set(pos.col, Cell::new(text, style));
    if text.is_wide() {
      if pos.col + 1 < width {
        row.set(pos.col + 1, Cell::new(CellText::CONTINUATION, style));
      } else {
        row.set(pos.col, Cell::filler(style));
      }
    }
  }

  /// Writes a run of single-width ASCII chars into one row starting at
  /// `pos`, without moving the cursor. The run must fit within the row.
  pub fn write_ascii_row(&mut self, pos: Pos, bytes: &[u8], style: StyleId) {
    if bytes.is_empty() || pos.row >= self.size.height {
      return;
    }
    let end =
      (usize::from(pos.col) + bytes.len()).min(usize::from(self.size.width));
    self.touch(pos.row, pos.col, end as u16);
    let row = self.row_mut(pos.row);
    // The halves of wide cells cut at either end of the run keep their
    // own style, as `Row::set` leaves them.
    let own = |row: &Row, col: usize| row.cells[col].style;
    row.clear_wide(pos.col, own(row, usize::from(pos.col)));
    if end > 0 {
      row.clear_wide(end as u16 - 1, own(row, end - 1));
    }
    for (i, b) in bytes.iter().enumerate() {
      let col = usize::from(pos.col) + i;
      if col >= end {
        break;
      }
      row.cells[col] = Cell::new(CellText::scalar(*b as char, 1), style);
    }
  }

  pub fn cell_mut(&mut self, pos: Pos) -> Option<&mut Cell> {
    if pos.row >= self.size.height {
      return None;
    }
    self.row_mut(pos.row).get_mut(pos.col)
  }

  /// Rewrites the styles of every cell in `area`; `f` runs once per
  /// distinct style.
  pub fn map_styles(&mut self, area: Rect, mut f: impl FnMut(Style) -> Style) {
    let mut mapped: Vec<Option<StyleId>> = vec![None; self.tables.styles.len()];
    for row in area.y..area.bottom().min(self.size.height) {
      for col in area.x..area.right().min(self.size.width) {
        let old = self.rows[usize::from(row)].cells[usize::from(col)].style;
        let new = match mapped.get(old.0 as usize).copied().flatten() {
          Some(new) => new,
          None => {
            let new = self.intern_style(f(self.style_of(old)));
            if let Some(slot) = mapped.get_mut(old.0 as usize) {
              *slot = Some(new);
            }
            new
          }
        };
        if new != old {
          self.row_mut(row).cells[usize::from(col)].style = new;
        }
      }
    }
  }

  // Cursor

  pub fn pos(&self) -> Pos {
    self.pos
  }

  /// Puts the cursor at `pos` of the screen, origin mode or not.
  pub fn set_pos_absolute(&mut self, pos: Pos) {
    self.pos = Pos {
      row: pos.row.min(self.size.height - 1),
      col: pos.col.min(self.size.width - 1),
    };
  }

  pub fn set_pos(&mut self, mut pos: Pos) {
    if self.origin_mode {
      pos.row = pos.row.saturating_add(self.scroll_top);
      pos.col = pos.col.saturating_add(self.margin_left);
    }
    self.pos = pos;
    self.row_clamp_top(self.origin_mode);
    self.row_clamp_bottom(self.origin_mode);
    if self.origin_mode {
      self.pos.col = self.pos.col.clamp(self.margin_left, self.margin_right);
    } else {
      self.col_clamp();
    }
  }

  pub fn save_cursor(&mut self) {
    self.saved_pos = self.pos;
    self.saved_origin_mode = self.origin_mode;
  }

  pub fn restore_cursor(&mut self) {
    self.pos = self.saved_pos;
    self.origin_mode = self.saved_origin_mode;
    self.row_clamp();
    self.col_clamp();
  }

  pub fn row_set(&mut self, row: u16) {
    self.pos.row = row;
    if self.origin_mode {
      self.pos.row = self.pos.row.saturating_add(self.scroll_top);
      self.row_clamp_top(true);
      self.row_clamp_bottom(true);
    } else {
      self.row_clamp();
    }
  }

  pub fn col_set(&mut self, col: u16) {
    self.pos.col = col;
    if self.origin_mode {
      self.pos.col = self.pos.col.saturating_add(self.margin_left);
      self.pos.col = self.pos.col.clamp(self.margin_left, self.margin_right);
    } else {
      self.col_clamp();
    }
  }

  pub fn row_inc_clamp(&mut self, count: u16) {
    let in_region = self.in_scroll_region();
    self.pos.row = self.pos.row.saturating_add(count);
    self.row_clamp_bottom(in_region);
  }

  /// Moves down, scrolling the region when the cursor leaves it at the
  /// bottom; returns how many lines scrolled.
  pub fn row_inc_scroll(&mut self, count: u16, style: StyleId) -> u16 {
    let in_region = self.in_scroll_region();
    self.pos.row = self.pos.row.saturating_add(count);
    let lines = self.row_clamp_bottom(in_region);
    if in_region && lines > 0 {
      self.scroll_up(lines, style);
      lines
    } else {
      0
    }
  }

  pub fn row_dec_clamp(&mut self, count: u16) {
    let in_region = self.in_scroll_region();
    self.pos.row = self.pos.row.saturating_sub(count);
    self.row_clamp_top(in_region);
  }

  pub fn row_dec_scroll(&mut self, count: u16, style: StyleId) {
    let in_region = self.in_scroll_region();
    let extra = count.saturating_sub(self.pos.row);
    self.pos.row = self.pos.row.saturating_sub(count);
    let lines = self.row_clamp_top(in_region);
    if in_region {
      self.scroll_down(lines + extra, style);
    }
  }

  pub fn col_inc(&mut self, count: u16) {
    self.pos.col = self.pos.col.saturating_add(count);
  }

  pub fn col_inc_clamp(&mut self, count: u16) {
    let in_margins = self.in_margins();
    self.pos.col = self.pos.col.saturating_add(count);
    if in_margins {
      self.pos.col = self.pos.col.min(self.margin_right);
    } else {
      self.col_clamp();
    }
  }

  pub fn col_dec(&mut self, count: u16) {
    let in_margins = self.in_margins();
    // A pending wrap is undone by the move.
    self.pos.col = self.pos.col.min(self.size.width - 1);
    self.pos.col = self.pos.col.saturating_sub(count);
    if in_margins {
      self.pos.col = self.pos.col.max(self.margin_left);
    }
  }

  /// Column for the next cell of `width`, wrapping to the next line (and
  /// scrolling) when it does not fit. The line is marked as wrapped (so
  /// it rejoins on reflow) when it continues straight onto the next row.
  pub fn col_wrap(&mut self, width: u16, style: StyleId) {
    let right = self.margin_right;
    if self.pos.col + width > right + 1 {
      let prev_row = self.pos.row;
      // Blank room a wide character skips at the end of the row is
      // padding, not text: it is dropped when the line is wrapped again.
      // A glyph already there stays where it is.
      if self.pos.col <= right {
        let (from, to) = (self.pos.col, right);
        self.touch(prev_row, from, to + 1);
        let row = self.row_mut(prev_row);
        for col in from..=to {
          if row
            .get(col)
            .is_some_and(|cell| !cell.has_contents() && !cell.is_continuation())
          {
            row.set(col, Cell::filler(style));
          }
        }
      }
      self.pos.col = self.margin_left;
      let scrolled = self.row_inc_scroll(1, style);
      let prev_row = prev_row.saturating_sub(scrolled);
      let new_row = self.pos.row;
      let full_width = !self.margins_active();
      if let Some(row) = self.drawing_row_mut(prev_row) {
        row.wrap(full_width && prev_row + 1 == new_row);
      }
    }
  }

  /// When autowrap is off, the cursor stays on the last column.
  pub fn col_stay(&mut self) {
    self.pos.col = self.pos.col.min(self.margin_right);
  }

  fn row_clamp_top(&mut self, limit_to_region: bool) -> u16 {
    if limit_to_region && self.pos.row < self.scroll_top {
      let rows = self.scroll_top - self.pos.row;
      self.pos.row = self.scroll_top;
      rows
    } else {
      0
    }
  }

  fn row_clamp_bottom(&mut self, limit_to_region: bool) -> u16 {
    let bottom = if limit_to_region {
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

  fn in_scroll_region(&self) -> bool {
    self.pos.row >= self.scroll_top && self.pos.row <= self.scroll_bottom
  }

  fn in_margins(&self) -> bool {
    self.pos.col >= self.margin_left && self.pos.col <= self.margin_right
  }

  fn scroll_region_active(&self) -> bool {
    self.scroll_top != 0 || self.scroll_bottom != self.size.height - 1
  }

  pub fn margins_active(&self) -> bool {
    self.margin_left != 0 || self.margin_right != self.size.width - 1
  }

  // Tabs

  pub fn col_tab(&mut self) {
    self.tab_forward(1);
  }

  pub fn tab_forward(&mut self, count: u16) {
    let right = self.margin_right.min(self.size.width - 1);
    let mut col = self.pos.col.min(right);
    for _ in 0..count.max(1) {
      let next = (col + 1..=right).find(|c| self.tabs[usize::from(*c)]);
      col = next.unwrap_or(right);
    }
    self.pos.col = col;
  }

  pub fn tab_back(&mut self, count: u16) {
    let left = self.margin_left;
    let mut col = self.pos.col.min(self.size.width - 1);
    for _ in 0..count.max(1) {
      let prev = (left..col).rev().find(|c| self.tabs[usize::from(*c)]);
      col = prev.unwrap_or(left);
    }
    self.pos.col = col;
  }

  pub fn set_tab(&mut self) {
    let col = self.pos.col.min(self.size.width - 1);
    self.tabs[usize::from(col)] = true;
  }

  pub fn clear_tab(&mut self) {
    let col = self.pos.col.min(self.size.width - 1);
    self.tabs[usize::from(col)] = false;
  }

  pub fn clear_all_tabs(&mut self) {
    self.tabs.iter_mut().for_each(|t| *t = false);
  }

  pub fn tab_stops(&self) -> Vec<u16> {
    self
      .tabs
      .iter()
      .enumerate()
      .filter(|(_, set)| **set)
      .map(|(col, _)| col as u16)
      .collect()
  }

  pub fn set_tab_stops(&mut self, stops: &[u16]) {
    self.tabs.iter_mut().for_each(|t| *t = false);
    for col in stops {
      if let Some(tab) = self.tabs.get_mut(usize::from(*col)) {
        *tab = true;
      }
    }
  }

  // Regions

  pub fn set_scroll_region(&mut self, top: u16, bottom: u16) {
    let bottom = bottom.min(self.size.height - 1);
    if top < bottom {
      self.scroll_top = top;
      self.scroll_bottom = bottom;
    } else {
      self.scroll_top = 0;
      self.scroll_bottom = self.size.height - 1;
    }
    self.set_pos(Pos { row: 0, col: 0 });
  }

  pub fn scroll_region(&self) -> (u16, u16) {
    (self.scroll_top, self.scroll_bottom)
  }

  pub fn set_margins(&mut self, left: u16, right: u16) {
    let right = right.min(self.size.width - 1);
    if left < right {
      self.margin_left = left;
      self.margin_right = right;
    } else {
      self.margin_left = 0;
      self.margin_right = self.size.width - 1;
    }
    self.set_pos(Pos { row: 0, col: 0 });
  }

  pub fn margins(&self) -> (u16, u16) {
    (self.margin_left, self.margin_right)
  }

  pub fn set_origin_mode(&mut self, mode: bool) {
    self.origin_mode = mode;
    self.set_pos(Pos { row: 0, col: 0 });
  }

  pub fn origin_mode(&self) -> bool {
    self.origin_mode
  }

  // Erasing

  pub fn erase_all(&mut self, style: StyleId) {
    let width = self.size.width;
    for row in &mut self.rows {
      blank_in_place(row, width, style);
    }
    // Images go with the screen they were on, as in kitty.
    let dropped = &mut self.dropped_images;
    self.placements.retain(|p| {
      if p.cells {
        dropped.push(p.key);
      }
      p.virtual_
    });
    if !self.extra_images.is_empty() {
      self.extra_images = Arc::default();
    }
  }

  pub fn erase_all_forward(&mut self, style: StyleId) {
    let width = self.size.width;
    let from = usize::from(self.pos.row) + 1;
    for r in from..self.rows.len() {
      self.touch(r as u16, 0, width);
      blank_in_place(&mut self.rows[r], width, style);
    }
    self.erase_row_forward(style);
  }

  pub fn erase_all_backward(&mut self, style: StyleId) {
    let width = self.size.width;
    let to = usize::from(self.pos.row).min(self.rows.len());
    for r in 0..to {
      self.touch(r as u16, 0, width);
      blank_in_place(&mut self.rows[r], width, style);
    }
    self.erase_row_backward(style);
  }

  pub fn erase_row(&mut self, style: StyleId) {
    self.touch(self.pos.row, 0, self.size.width);
    self.current_row_mut().clear(style);
  }

  pub fn erase_row_forward(&mut self, style: StyleId) {
    let width = self.size.width;
    let col = self.pos.col.min(width - 1);
    self.touch(self.pos.row, col, width);
    let row = self.current_row_mut();
    for c in col..width {
      row.erase(c, style);
    }
  }

  pub fn erase_row_backward(&mut self, style: StyleId) {
    let width = self.size.width;
    let col = self.pos.col.min(width - 1);
    self.touch(self.pos.row, 0, col + 1);
    let row = self.current_row_mut();
    for c in 0..=col {
      row.erase(c, style);
    }
  }

  pub fn erase_cells(&mut self, count: u16, style: StyleId) {
    let width = self.size.width;
    let col = self.pos.col.min(width - 1);
    let end = col.saturating_add(count.max(1)).min(width);
    self.touch(self.pos.row, col, end);
    let row = self.current_row_mut();
    for c in col..end {
      row.erase(c, style);
    }
  }

  // Editing within the row

  pub fn insert_cells(&mut self, count: u16, style: StyleId) {
    let (left, right) = (self.margin_left, self.margin_right);
    let col = self.pos.col.min(self.size.width - 1);
    if col < left || col > right {
      return;
    }
    self.touch(self.pos.row, col, right + 1);
    self
      .current_row_mut()
      .insert_blanks(col, count.max(1), right, style);
  }

  pub fn delete_cells(&mut self, count: u16, style: StyleId) {
    let (left, right) = (self.margin_left, self.margin_right);
    let col = self.pos.col.min(self.size.width - 1);
    if col < left || col > right {
      return;
    }
    self.touch(self.pos.row, col, right + 1);
    self
      .current_row_mut()
      .delete_cells(col, count.max(1), right, style);
  }

  // Lines

  /// Scrolls the region up: the top row leaves (into history when the
  /// region is the whole screen), a blank row enters at the bottom.
  pub fn scroll_up(&mut self, count: u16, style: StyleId) {
    let (top, bottom) = (
      usize::from(self.scroll_top),
      usize::from(self.scroll_bottom),
    );
    let count = usize::from(count).min(bottom - top + 1);
    if count == 0 {
      return;
    }
    if self.margins_active() {
      self.shift_cells_up(top, bottom, count, style);
      return;
    }
    let full_screen = !self.scroll_region_active();
    let to_history = full_screen && self.history.capacity() > 0;
    let width = self.size.width;
    for _ in 0..count {
      self.log_scroll_off(top, bottom);
      // The top row moves to the bottom and is blanked there, reused when
      // nothing else holds it.
      self.rows[top..=bottom].rotate_left(1);
      if to_history {
        self.history.commit(&self.rows[bottom], &self.tables);
        if self.view_offset > 0 {
          self.view_offset += 1;
        }
      }
      blank_in_place(&mut self.rows[bottom], width, style);
    }
    if full_screen {
      self.row_base += count as u64;
      self.drop_scrolled_placements();
    } else {
      self.shift_placements(top as u16, bottom as u16, -(count as i64));
    }
    if self.view_offset > 0 {
      // History may have dropped lines at the top.
      self.view_offset = self.view_offset.min(self.max_scrollback());
      self.view_stale = true;
    }
  }

  /// Rebuilds the view of history if output scrolled under it.
  pub fn settle_view(&mut self) {
    if self.view_stale {
      self.refresh_view();
    }
  }

  pub fn scroll_down(&mut self, count: u16, style: StyleId) {
    let (top, bottom) = (
      usize::from(self.scroll_top),
      usize::from(self.scroll_bottom),
    );
    let count = usize::from(count).min(bottom - top + 1);
    if count == 0 {
      return;
    }
    if self.margins_active() {
      self.shift_cells_down(top, bottom, count, style);
      return;
    }
    self.rotate_down(top, bottom, count, style);
    self.shift_placements(top as u16, bottom as u16, count as i64);
    // Logged rows inside the region move down with it.
    if let Some(log) = &mut self.log {
      let frontier = usize::from(log.frontier);
      if frontier > top && frontier <= bottom {
        log.frontier = (frontier + count).min(bottom + 1) as u16;
      }
    }
  }

  /// Moves rows `top..=bottom` down by `count`, blank rows entering at
  /// the top: the rows that leave at the bottom are reused for them.
  fn rotate_down(
    &mut self,
    top: usize,
    bottom: usize,
    count: usize,
    style: StyleId,
  ) {
    let width = self.size.width;
    for _ in 0..count {
      self.rows[top..=bottom].rotate_right(1);
      blank_in_place(&mut self.rows[top], width, style);
    }
    self.row_mut(self.scroll_bottom).wrap(false);
  }

  pub fn insert_lines(&mut self, count: u16, style: StyleId) {
    if !self.in_scroll_region() {
      return;
    }
    let (top, bottom) =
      (usize::from(self.pos.row), usize::from(self.scroll_bottom));
    let count = usize::from(count.max(1)).min(bottom - top + 1);
    if self.margins_active() {
      self.shift_cells_down(top, bottom, count, style);
      return;
    }
    self.rotate_down(top, bottom, count, style);
    self.shift_placements(top as u16, bottom as u16, count as i64);
  }

  pub fn delete_lines(&mut self, count: u16, style: StyleId) {
    if !self.in_scroll_region() {
      return;
    }
    let (top, bottom) =
      (usize::from(self.pos.row), usize::from(self.scroll_bottom));
    let count = usize::from(count.max(1)).min(bottom - top + 1);
    if self.margins_active() {
      self.shift_cells_up(top, bottom, count, style);
      return;
    }
    let width = self.size.width;
    for _ in 0..count {
      self.rows[top..=bottom].rotate_left(1);
      blank_in_place(&mut self.rows[bottom], width, style);
    }
    self.shift_placements(top as u16, bottom as u16, -(count as i64));
  }

  /// Shifts the cells between the margins of rows `top..=bottom` up by
  /// `count` rows, blanking the rows that enter at the bottom.
  fn shift_cells_up(
    &mut self,
    top: usize,
    bottom: usize,
    count: usize,
    style: StyleId,
  ) {
    let (left, right) = (
      usize::from(self.margin_left),
      usize::from(self.margin_right),
    );
    for row in top..=bottom {
      self.touch(row as u16, left as u16, right as u16 + 1);
      let from = row + count;
      let cells: Vec<Cell> = if from <= bottom {
        self.rows[from].cells[left..=right].to_vec()
      } else {
        vec![Cell::blank(style); right - left + 1]
      };
      let dst = Arc::make_mut(&mut self.rows[row]);
      dst.cells[left..=right].copy_from_slice(&cells);
      dst.wrap(false);
    }
  }

  fn shift_cells_down(
    &mut self,
    top: usize,
    bottom: usize,
    count: usize,
    style: StyleId,
  ) {
    let (left, right) = (
      usize::from(self.margin_left),
      usize::from(self.margin_right),
    );
    for row in (top..=bottom).rev() {
      self.touch(row as u16, left as u16, right as u16 + 1);
      let cells: Vec<Cell> = if row >= top + count {
        self.rows[row - count].cells[left..=right].to_vec()
      } else {
        vec![Cell::blank(style); right - left + 1]
      };
      let dst = Arc::make_mut(&mut self.rows[row]);
      dst.cells[left..=right].copy_from_slice(&cells);
      dst.wrap(false);
    }
  }

  /// DECIC: inserts blank columns at the cursor within the margins.
  pub fn insert_columns(&mut self, count: u16, style: StyleId) {
    let (left, right) = (self.margin_left, self.margin_right);
    let col = self.pos.col.min(self.size.width - 1);
    if col < left || col > right {
      return;
    }
    for row in self.scroll_top..=self.scroll_bottom {
      self.touch(row, col, right + 1);
      self
        .row_mut(row)
        .insert_blanks(col, count.max(1), right, style);
    }
  }

  /// DECDC: deletes columns at the cursor within the margins.
  pub fn delete_columns(&mut self, count: u16, style: StyleId) {
    let (left, right) = (self.margin_left, self.margin_right);
    let col = self.pos.col.min(self.size.width - 1);
    if col < left || col > right {
      return;
    }
    for row in self.scroll_top..=self.scroll_bottom {
      self.touch(row, col, right + 1);
      self
        .row_mut(row)
        .delete_cells(col, count.max(1), right, style);
    }
  }

  // History and the view

  fn used_len(&self, row: &Row) -> u16 {
    let styles = &self.tables.styles;
    row.used_len(|id| styles.get(id).is_visible_blank())
  }

  // Log lines

  /// Starts logging lines, from the cursor's line on.
  pub fn enable_log(&mut self) {
    self.log = Some(LineLog::default());
    self.anchor_log();
  }

  /// After rows moved under the log (a reflow, a restore): what is above
  /// the cursor's line counts as logged, and the part of that line in
  /// history is its prefix.
  fn anchor_log(&mut self) {
    let first = self.line_start(self.pos.row);
    let in_history = (first == 0 && self.history.is_open())
      .then(|| self.history.last().map(|line| line.text.clone()))
      .flatten();
    let Some(log) = &mut self.log else {
      return;
    };
    log.frontier = first;
    if let Some(text) = in_history {
      log.prefix = text;
    } else if first > 0 {
      log.prefix.clear();
    }
  }

  /// The cursor leaves its row with a newline: the line it ends is
  /// logged, unless it already was.
  pub fn log_newline(&mut self) {
    let row = self.pos.row;
    let text = match &self.log {
      Some(log) if row >= log.frontier => self.log_text(&log.prefix, row),
      _ => return,
    };
    if let Some(log) = &mut self.log {
      log.push(text);
      log.frontier = row + 1;
    }
  }

  /// The screen was cleared: everything on it is new.
  pub fn log_restart(&mut self) {
    if let Some(log) = &mut self.log {
      log.frontier = 0;
      log.prefix.clear();
    }
  }

  /// The cursor's line, when it is not logged yet and has text.
  pub fn open_log_line(&self) -> Option<String> {
    let log = self.log.as_ref()?;
    let row = self.pos.row;
    if row < log.frontier {
      return None;
    }
    let text = self.log_text(&log.prefix, row);
    (!text.is_empty()).then_some(text)
  }

  /// Logs the open line as it is (the program exited).
  pub fn finish_log_line(&mut self) {
    let Some(text) = self.open_log_line() else {
      return;
    };
    if let Some(log) = &mut self.log {
      log.push(text);
      log.frontier = self.pos.row + 1;
    }
  }

  /// Looks for `needle` in each line logged from now on, and in the line
  /// under way now and whenever `log_check_open` is called, until it is
  /// seen (`take_found`). `None` stops looking.
  pub fn look_for(&mut self, needle: Option<String>) {
    if let Some(log) = &mut self.log {
      log.wanted = needle.filter(|needle| !needle.is_empty());
      log.found = false;
    }
    self.log_check_open();
  }

  /// Checks the line under way for the needle: a carriage return may be
  /// about to overwrite it, or the output paused there.
  pub fn log_check_open(&mut self) {
    if self.log.as_ref().is_none_or(|log| log.wanted.is_none()) {
      return;
    }
    if let Some(text) = self.open_log_line()
      && let Some(log) = &mut self.log
    {
      log.check(&text);
    }
  }

  /// Whether the needle was seen since the last take.
  pub fn take_found(&mut self) -> bool {
    self
      .log
      .as_mut()
      .is_some_and(|log| std::mem::take(&mut log.found))
  }

  /// Moves the lines logged so far to `out`.
  pub fn take_log_lines(&mut self, out: &mut Vec<String>) {
    if let Some(log) = &mut self.log {
      out.append(&mut log.lines);
    }
  }

  /// Takes over what the log of `old`, the grid this one replaces (a
  /// reset), still owes: its lines not taken yet, and what it looks for.
  pub fn carry_log(&mut self, old: &mut Grid) {
    if let (Some(log), Some(old)) = (&mut self.log, &mut old.log) {
      log.lines = std::mem::take(&mut old.lines);
      log.wanted = old.wanted.take();
      log.found = old.found;
    }
  }

  /// Row `top` is about to leave the region: logged when no one logged
  /// it, else the frontier moves up with the rows.
  fn log_scroll_off(&mut self, top: usize, bottom: usize) {
    let Grid {
      log: Some(log),
      rows,
      tables,
      ..
    } = self
    else {
      return;
    };
    let frontier = usize::from(log.frontier);
    if frontier > top {
      if frontier <= bottom + 1 {
        log.frontier -= 1;
      }
      return;
    }
    let row = &rows[top];
    if row.wrapped() {
      // The line goes on below: keep its start.
      if log.prefix.len() < MAX_LOG_LINE {
        row.push_text(0..row.cols(), &tables.graphemes, &mut log.prefix);
      }
      return;
    }
    let mut text = std::mem::take(&mut log.prefix);
    row.push_text(0..row.cols(), &tables.graphemes, &mut text);
    trim_log_text(&mut text);
    // Blank rows no one wrote to are not lines.
    if !text.is_empty() {
      log.push(text);
    }
  }

  /// The first row of the logical line live row `row` is on.
  fn line_start(&self, row: u16) -> u16 {
    let mut first = usize::from(row).min(self.rows.len() - 1);
    while first > 0 && self.rows[first - 1].wrapped() {
      first -= 1;
    }
    first as u16
  }

  /// The text of the logical line live row `row` ends: `prefix` (its
  /// start, scrolled off) when the line starts at the top, then the rows
  /// that wrapped into it and the row itself.
  fn log_text(&self, prefix: &str, row: u16) -> String {
    let row = row.min(self.size.height - 1);
    let first = self.line_start(row);
    let mut out = String::new();
    if first == 0 {
      out.push_str(prefix);
    }
    for r in first..=row {
      if out.len() >= MAX_LOG_LINE {
        break;
      }
      let row = &self.rows[usize::from(r)];
      row.push_text(0..row.cols(), &self.tables.graphemes, &mut out);
    }
    trim_log_text(&mut out);
    out
  }

  /// Whether the tab stops are the default every eight columns.
  pub fn tabs_are_default(&self) -> bool {
    self.tabs == default_tabs(self.size.width)
  }

  /// Rows scrolled back into history.
  pub fn scrollback(&self) -> usize {
    self.view_offset
  }

  pub fn set_scrollback(&mut self, rows: usize) {
    let max = self.max_scrollback();
    self.view_offset = rows.min(max);
    self.refresh_view();
  }

  /// The most the view can scroll back: every history row at this width.
  pub fn max_scrollback(&self) -> usize {
    self.history.rows_at(self.size.width)
  }

  /// Lines of history the grid keeps.
  pub fn scrollback_len(&self) -> usize {
    self.history.capacity()
  }

  pub fn clear_history(&mut self) {
    self.history.clear();
    self.view_offset = 0;
    self.view_above.clear();
    let base = self.row_base;
    self
      .placements
      .retain(|p| p.virtual_ || p.row + u64::from(p.rows) > base);
  }

  fn refresh_view(&mut self) {
    self.view_stale = false;
    if self.view_offset == 0 {
      self.view_above.clear();
      return;
    }
    let count = self.view_offset.min(usize::from(self.size.height));
    let rows = self.history.rows_above(
      self.view_offset,
      count,
      self.size.width,
      &mut self.tables,
    );
    self.view_above = rows.into_iter().map(Arc::new).collect();
  }

  // Images

  /// Rows that ever scrolled off the top: the absolute row of row 0.
  pub fn row_base(&self) -> u64 {
    self.row_base
  }

  pub fn placements(&self) -> &[Placement] {
    &self.placements
  }

  /// Cells `cols` of live row `row` are about to be written: a placement
  /// drawn into its cells (sixel) goes if it is one of them.
  fn touch(&mut self, row: u16, start: u16, end: u16) {
    if self.placements.is_empty() || start >= end {
      return;
    }
    let abs = self.row_base + u64::from(row);
    let dropped = &mut self.dropped_images;
    self.placements.retain(|p| {
      let hit = p.cells
        && p.row <= abs
        && abs < p.row + u64::from(p.rows)
        && p.col < end
        && start < p.col.saturating_add(p.cols);
      if hit {
        dropped.push(p.key);
      }
      !hit
    });
  }

  /// Images whose cell-bound placements were written over since the last
  /// call; the screen frees those nothing shows any more.
  pub fn take_dropped_images(&mut self) -> Vec<ImageKey> {
    std::mem::take(&mut self.dropped_images)
  }

  pub fn add_placement(&mut self, placement: Placement) {
    self.placements.push(placement);
  }

  pub fn retain_placements(&mut self, f: impl FnMut(&Placement) -> bool) {
    self.placements.retain(f);
  }

  /// Removes the placements `pick` picks; returns their image keys.
  pub fn remove_placements(
    &mut self,
    mut pick: impl FnMut(&Placement) -> bool,
  ) -> Vec<ImageKey> {
    let mut keys = Vec::new();
    self.placements.retain(|p| {
      if pick(p) {
        keys.push(p.key);
        false
      } else {
        true
      }
    });
    keys
  }

  /// Images this grid shows that are not its screen's own (a UI grid
  /// composing other frames).
  pub fn images(&self) -> &ImageTable {
    &self.extra_images
  }

  /// The images, to share with a frame.
  pub fn image_table(&self) -> &Arc<ImageTable> {
    &self.extra_images
  }

  pub fn add_image(&mut self, image: Arc<Image>) {
    if !self.extra_images.contains_key(&image.key) {
      Arc::make_mut(&mut self.extra_images).insert(image.key, image);
    }
  }

  /// A placement as another frame shows it, put on this grid at the view
  /// row it names.
  pub fn put_placement(&mut self, shown: VisiblePlacement) {
    self.placements.push(Placement {
      key: shown.key,
      id: 0,
      cells: false,
      row: self.row_base + u64::from(shown.row),
      col: shown.col,
      cols: shown.cols,
      rows: shown.rows,
      src: shown.src,
      offset_x: shown.offset_x,
      offset_y: shown.offset_y,
      z: shown.z,
      virtual_: false,
    });
  }

  /// Moves placements within rows `top..=bottom` by `delta` rows; those
  /// leaving the range go.
  fn shift_placements(&mut self, top: u16, bottom: u16, delta: i64) {
    if self.placements.is_empty() {
      return;
    }
    let base = self.row_base;
    let (top, bottom) = (base + u64::from(top), base + u64::from(bottom));
    self.placements.retain_mut(|p| {
      if p.virtual_ || p.row < top || p.row > bottom {
        return true;
      }
      let moved = p.row as i64 + delta;
      if moved < top as i64 || moved > bottom as i64 {
        return false;
      }
      p.row = moved as u64;
      true
    });
  }

  /// Placements that scrolled out of what history keeps are gone.
  fn drop_scrolled_placements(&mut self) {
    if self.placements.iter().all(|p| p.virtual_) {
      return;
    }
    let kept = self.max_scrollback() as u64;
    let first = self.row_base.saturating_sub(kept);
    self
      .placements
      .retain(|p| p.virtual_ || p.row + u64::from(p.rows) > first);
  }

  /// The slot placeholder cells of image `image_id` (placement
  /// `placement_id`, 0 for any) refer to.
  pub fn placeholder_slot(
    &mut self,
    image_id: u32,
    placement_id: u32,
  ) -> Option<u32> {
    let pair = (image_id, placement_id);
    if let Some(i) = self.placeholder_slots.iter().position(|s| *s == pair) {
      return Some(i as u32);
    }
    if self.placeholder_slots.len() as u32 >= super::cell::MAX_PLACEHOLDER_SLOTS
    {
      return None;
    }
    self.placeholder_slots.push(pair);
    Some(self.placeholder_slots.len() as u32 - 1)
  }

  pub fn placeholder_ids(&self, slot: u32) -> Option<(u32, u32)> {
    self.placeholder_slots.get(slot as usize).copied()
  }

  pub fn placeholder_slots(&self) -> &[(u32, u32)] {
    &self.placeholder_slots
  }

  /// Puts back what a snapshot saved of the image layer.
  pub fn restore_images(
    &mut self,
    row_base: u64,
    placements: Vec<Placement>,
    placeholder_slots: Vec<(u32, u32)>,
    image_cells: &[(u16, u16, u32, u16, u16)],
  ) {
    self.row_base = row_base;
    self.placements = placements;
    self.placeholder_slots = placeholder_slots;
    for &(row, col, slot, img_row, img_col) in image_cells {
      let pos = Pos { row, col };
      if let Some(cell) = self.drawing_cell(pos) {
        self.write_cell(
          pos,
          CellText::image(slot, img_row, img_col),
          cell.style,
        );
      }
    }
  }

  /// The placeholder cells of the screen rows, for snapshots.
  pub fn image_cells(&self) -> Vec<(u16, u16, u32, u16, u16)> {
    let mut out = Vec::new();
    if self.placeholder_slots.is_empty() {
      return out;
    }
    for (r, row) in self.rows.iter().enumerate() {
      for (c, cell) in row.cells.iter().enumerate() {
        if let TextKind::Image {
          slot,
          row: ir,
          col: ic,
        } = cell.text.kind()
        {
          out.push((r as u16, c as u16, slot, ir, ic));
        }
      }
    }
    out
  }

  /// Everything images show in the view (`rows`, as `visible_rows`
  /// gives them): classic placements on screen, and the rectangles
  /// placeholder cells draw of virtual ones.
  pub fn visible_placements(
    &self,
    rows: &[Arc<Row>],
    image_of: &dyn Fn(u32) -> Option<ImageKey>,
  ) -> Vec<VisiblePlacement> {
    let mut out = Vec::new();
    let height = u64::from(self.size.height);
    // The absolute row shown on view row 0.
    let view_top = self.row_base.saturating_sub(self.view_offset as u64);
    for p in &self.placements {
      if p.virtual_ || p.col >= self.size.width {
        continue;
      }
      let top = p.row as i64 - view_top as i64;
      let rows = u64::from(p.rows.max(1));
      if top >= height as i64 || top + rows as i64 <= 0 {
        continue;
      }
      let mut shown = VisiblePlacement {
        key: p.key,
        row: top.max(0) as u16,
        col: p.col,
        cols: p.cols,
        rows: p.rows,
        src: p.src,
        offset_x: p.offset_x,
        offset_y: p.offset_y,
        z: p.z,
      };
      if top < 0 {
        // Starts above the view: the rows above are cut off, and the
        // source in proportion.
        let cut = (-top) as u32;
        let kept = (rows as u32) - cut;
        shown.rows = kept as u16;
        shown.src = p.src.part(p.cols, p.rows, 0, cut, u32::from(p.cols), kept);
        shown.offset_y = 0;
      }
      out.push(shown);
    }
    if !self.placeholder_slots.is_empty() {
      self.placeholder_rects(rows, image_of, &mut out);
    }
    out
  }

  /// Groups placeholder cells into rectangles of the virtual placements
  /// they belong to.
  fn placeholder_rects(
    &self,
    rows: &[Arc<Row>],
    image_of: &dyn Fn(u32) -> Option<ImageKey>,
    out: &mut Vec<VisiblePlacement>,
  ) {
    #[derive(Clone)]
    struct Run {
      slot: u32,
      img_row: u16,
      img_col: u16,
      row: u16,
      col: u16,
      cols: u16,
      rows: u16,
    }
    let mut open: Vec<Run> = Vec::new();
    let mut done: Vec<Run> = Vec::new();
    for (r, row) in rows.iter().enumerate() {
      let mut runs: Vec<Run> = Vec::new();
      for (c, cell) in row.cells.iter().enumerate() {
        let TextKind::Image {
          slot,
          row: ir,
          col: ic,
        } = cell.text.kind()
        else {
          continue;
        };
        if let Some(last) = runs.last_mut()
          && last.slot == slot
          && last.img_row == ir
          && last.img_col + last.cols == ic
          && last.col + last.cols == c as u16
        {
          last.cols += 1;
          continue;
        }
        runs.push(Run {
          slot,
          img_row: ir,
          img_col: ic,
          row: r as u16,
          col: c as u16,
          cols: 1,
          rows: 1,
        });
      }
      // Runs continuing the ones above grow them; the rest are done.
      let mut next_open: Vec<Run> = Vec::new();
      for run in runs {
        let joined = open.iter().position(|o| {
          o.slot == run.slot
            && o.col == run.col
            && o.cols == run.cols
            && o.img_col == run.img_col
            && o.img_row + o.rows == run.img_row
            && o.row + o.rows == run.row
        });
        match joined {
          Some(i) => {
            let mut o = open.remove(i);
            o.rows += 1;
            next_open.push(o);
          }
          None => next_open.push(run),
        }
      }
      done.append(&mut open);
      open = next_open;
    }
    done.append(&mut open);
    for run in done {
      let Some((image_id, placement_id)) = self.placeholder_ids(run.slot)
      else {
        continue;
      };
      let Some(key) = image_of(image_id) else {
        continue;
      };
      let Some(p) = self.placements.iter().find(|p| {
        p.virtual_
          && p.key == key
          && (placement_id == 0 || p.id == placement_id)
      }) else {
        continue;
      };
      let src = p.src.part(
        p.cols,
        p.rows,
        u32::from(run.img_col),
        u32::from(run.img_row),
        u32::from(run.cols),
        u32::from(run.rows),
      );
      out.push(VisiblePlacement {
        key,
        row: run.row,
        col: run.col,
        cols: run.cols,
        rows: run.rows,
        src,
        offset_x: 0,
        offset_y: 0,
        z: p.z,
      });
    }
  }

  // Resizing

  /// Reflows the live rows to the new size: wrapped rows rejoin and wrap
  /// again at the new width, rows that no longer fit go into history,
  /// and the cursor follows its text.
  pub fn set_size(&mut self, size: Size) {
    let size = Size {
      height: size.height.max(1),
      width: size.width.max(1),
    };
    if size == self.size {
      return;
    }
    let old_width = self.size.width;

    // Logical lines of the live rows, each as cells.
    let used_rows = self.used_rows();
    let mut lines: Vec<Vec<Cell>> = Vec::new();
    let mut open: Option<Arc<Line>> = None;
    if self.history.is_open() {
      open = self.history.pop_open();
    }
    let mut current: Vec<Cell> = Vec::new();
    // Rows of the open line pulled back from history, at the old width.
    let mut pulled_rows = 0usize;
    if let Some(line) = &open {
      let rows = line.rows(old_width, &mut self.tables);
      pulled_rows = rows.len();
      // The line's cells, and only those: fillers before a wrapped wide
      // character are padding, and so is what `Row::from_cells` added
      // after the last row's cells. Blanks within the count are real.
      let mut remaining = line.cell_count();
      for row in rows.iter() {
        let cells = row
          .cells
          .iter()
          .filter(|c| !c.text.is_filler())
          .take(remaining)
          .copied();
        let before = current.len();
        current.extend(cells);
        remaining -= current.len() - before;
      }
    }
    // Where each live row's cells start: its line and the offset in it.
    let mut row_starts: Vec<(usize, usize)> = Vec::with_capacity(used_rows);
    for (i, row) in self.rows.iter().enumerate().take(used_rows) {
      row_starts.push((lines.len(), current.len()));
      let wrapped = row.wrapped() && i + 1 < used_rows;
      let len = if wrapped {
        usize::from(old_width)
      } else {
        usize::from(self.used_len(row))
      };
      // A wide cell's other half comes along: column offsets into the
      // row are then offsets into its cells.
      let len = if row.cells.get(len).is_some_and(|c| c.is_continuation()) {
        len + 1
      } else {
        len
      };
      current.extend(
        row.cells[..len.min(row.cells.len())]
          .iter()
          .filter(|c| !c.text.is_filler()),
      );
      if !wrapped {
        lines.push(std::mem::take(&mut current));
      }
    }
    if !current.is_empty() {
      lines.push(current);
    }
    // The cursor's place: its row is among the used rows, and a pending
    // wrap (one past the last column) is the offset after the row's cells.
    let (cursor_line, cursor_offset) = {
      let (line, start) = row_starts[usize::from(self.pos.row)];
      (line, start + usize::from(self.pos.col.min(old_width)))
    };

    // Image placements follow their text: each one's top-left cell is
    // found again after the reflow by its place in the logical lines.
    let (mut anchors, gone) = self.placement_anchors(
      old_width,
      used_rows,
      pulled_rows,
      open.as_deref(),
      &row_starts,
    );
    // Positions found in the wrapped rows, one slot per anchor and the
    // cursor's last.
    let cursor_slot = anchors.len();
    let mut found: Vec<Option<(usize, usize)>> = vec![None; cursor_slot + 1];

    // Wrap each line at the new width.
    let width = usize::from(size.width);
    let mut rows: Vec<Row> = Vec::new();
    for (li, cells) in lines.iter().enumerate() {
      // Cell offsets to find on this line, and their slots.
      let mut wanted: Vec<(usize, usize)> = Vec::new();
      for (slot, (_, anchor)) in anchors.iter().enumerate() {
        if let Anchor::Line(line, offset) = anchor
          && *line == li
        {
          wanted.push((*offset, slot));
        }
      }
      if cursor_line == li {
        wanted.push((cursor_offset, cursor_slot));
      }
      let first_row = rows.len();
      let mut line_rows: Vec<Vec<Cell>> = vec![Vec::with_capacity(width)];
      let mut i = 0;
      while i < cells.len() {
        let cell = cells[i];
        let w = usize::from(cell.width()).max(1);
        let row_index = line_rows.len() - 1;
        if line_rows[row_index].len() + w > width
          && !line_rows[row_index].is_empty()
        {
          // The room a wide cell skips is padding.
          let pad = line_rows[row_index]
            .last()
            .map_or(StyleId::DEFAULT, |c| c.style);
          while line_rows[row_index].len() < width {
            line_rows[row_index].push(Cell::filler(pad));
          }
          line_rows.push(Vec::with_capacity(width));
          continue;
        }
        let row = &mut line_rows[row_index];
        note(&wanted, &mut found, i, (first_row + row_index, row.len()));
        row.push(cell);
        i += 1;
        if cell.is_wide() {
          // Its continuation follows in the source.
          let cont = if i < cells.len() && cells[i].is_continuation() {
            note(&wanted, &mut found, i, (first_row + row_index, row.len()));
            i += 1;
            cells[i - 1]
          } else {
            Cell::new(CellText::CONTINUATION, cell.style)
          };
          row.push(cont);
        }
      }
      // Offsets past the line's cells: blanks lead to them. The cursor
      // at the end of a full row is a pending wrap, as after writing its
      // last cell; further on it wraps onto rows of its own, which then
      // have to exist. An image's anchor stays on the line's last row,
      // past its right edge if need be (not shown until the screen is
      // wide enough), so the way back finds the same offset.
      let last = line_rows.len() - 1;
      // A one-column row may hold a wide cell and its half: no room.
      let row_len = line_rows[last].len();
      let room = width.saturating_sub(row_len);
      let mut extend = 0usize;
      for (offset, slot) in &wanted {
        if found[*slot].is_some() {
          continue;
        }
        let index = offset.saturating_sub(cells.len());
        found[*slot] = Some(if index < room || *slot != cursor_slot {
          (first_row + last, row_len + index)
        } else if index == room {
          (first_row + last, width)
        } else {
          let index = index - room;
          let extra_rows = index / width + 1;
          extend = extra_rows;
          (first_row + last + extra_rows, index % width)
        });
      }
      for _ in 0..extend {
        line_rows.push(Vec::new());
      }
      let last = line_rows.len() - 1;
      for (ri, cells) in line_rows.into_iter().enumerate() {
        let mut row = Row::from_cells(cells, size.width);
        row.wrap(ri < last);
        rows.push(row);
      }
    }
    // Lines history holds come back while the screen has room: what a
    // narrower or shorter screen pushed out returns when it grows, the
    // last of them split at the top as an open line if it does not fit
    // whole. Each is laid out at the new width and put before `rows`.
    let height = usize::from(size.height);
    let mut back_rows = 0usize;
    // `(history index, rows kept in history, live rows, line)`, in the
    // order pulled: the last is at the top of the screen.
    let mut back: Vec<(usize, usize, usize, Arc<Line>)> = Vec::new();
    while rows.len() < height {
      let Some(index) = self.history.len().checked_sub(1) else {
        break;
      };
      let Some(line) = self.history.pop_last() else {
        break;
      };
      let mut line_rows = line.rows(size.width, &mut self.tables);
      let room = height - rows.len();
      let keep = line_rows.len().saturating_sub(room);
      for row in &line_rows[..keep] {
        self.history.commit(row, &self.tables);
      }
      line_rows.drain(..keep);
      let live = line_rows.len();
      back_rows += live;
      line_rows.append(&mut rows);
      rows = line_rows;
      back.push((index, keep, live, line));
      if keep > 0 {
        break;
      }
    }
    if back_rows > 0 {
      for slot in found.iter_mut().flatten() {
        slot.0 += back_rows;
      }
      // Anchors in the lines that came back are live now.
      let mut first_row = 0usize;
      let mut starts: Vec<(usize, usize, usize, &Arc<Line>)> = Vec::new();
      for (index, keep, live, line) in back.iter().rev() {
        starts.push((*index, *keep, first_row, line));
        first_row += live;
      }
      for (slot, (_, anchor)) in anchors.iter_mut().enumerate() {
        let Anchor::History(index, offset) = *anchor else {
          continue;
        };
        let Some((_, keep, first_row, line)) =
          starts.iter().find(|(i, ..)| *i == index)
        else {
          continue;
        };
        let (row, col) = line.row_col_of(size.width, offset);
        if row < *keep {
          // Still on the part kept in history, at the same index.
          continue;
        }
        found[slot] = Some((first_row + row - keep, usize::from(col)));
        *anchor = Anchor::Line(0, 0);
      }
    }
    let cursor_pos = found[cursor_slot];

    // The absolute row of the first reflowed row: the history above it
    // is as it was.
    let bottom = self
      .row_base
      .saturating_sub((pulled_rows + back_rows) as u64);
    self.place_anchors(&anchors, &found, bottom, rows.len(), size.width, gone);

    // Replace the live rows; what does not fit goes into history.
    self.size = size;
    self.tabs = default_tabs_like(&self.tabs, size.width);
    self.margin_left = 0;
    self.margin_right = size.width - 1;
    let height = usize::from(size.height);
    let overflow = rows.len().saturating_sub(height);
    let mut new_rows: Vec<Arc<Row>> = Vec::with_capacity(height);
    for (i, row) in rows.into_iter().enumerate() {
      if i < overflow {
        self.history.commit(&row, &self.tables);
      } else {
        new_rows.push(Arc::new(row));
      }
    }
    while new_rows.len() < height {
      new_rows.push(Arc::new(Row::new(size.width)));
    }
    self.rows = new_rows;
    self.history.set_width(size.width);
    self.row_base = bottom + overflow as u64;
    self.drop_scrolled_placements();

    // The column may be one past the last: a pending wrap.
    let (row, col) = cursor_pos.unwrap_or((0, 0));
    self.pos = Pos {
      row: row.saturating_sub(overflow).min(height - 1) as u16,
      col: col.min(width) as u16,
    };
    self.saved_pos = Pos {
      row: self.saved_pos.row.min(size.height - 1),
      col: self.saved_pos.col.min(size.width - 1),
    };
    self.scroll_top = 0;
    self.scroll_bottom = size.height - 1;
    if let Some(cursor) = &mut self.cursor_pos {
      cursor.row = cursor.row.min(size.height - 1);
      cursor.col = cursor.col.min(size.width - 1);
    }
    self.view_offset = self.view_offset.min(self.max_scrollback());
    self.refresh_view();
    self.anchor_log();
  }

  /// Where each classic placement's top-left cell is in the logical
  /// lines before a reflow: on a live line (`row_starts` says where each
  /// live row's cells begin), in a history line, or below the content.
  /// Placements nowhere in history any more are returned to be dropped.
  fn placement_anchors(
    &self,
    old_width: u16,
    used_rows: usize,
    pulled_rows: usize,
    open: Option<&Line>,
    row_starts: &[(usize, usize)],
  ) -> (Vec<(usize, Anchor)>, Vec<usize>) {
    let mut anchors = Vec::new();
    let mut gone = Vec::new();
    if self.placements.iter().all(|p| p.virtual_) {
      return (anchors, gone);
    }
    let base = self.row_base;
    for (pi, p) in self.placements.iter().enumerate() {
      if p.virtual_ {
        continue;
      }
      let col = usize::from(p.col);
      let anchor = if p.row >= base {
        let i = (p.row - base) as usize;
        match row_starts.get(i) {
          Some(&(line, start)) => Anchor::Line(line, start + col),
          None => Anchor::Below(i.saturating_sub(used_rows)),
        }
      } else {
        let above = (base - p.row) as usize;
        if above <= pulled_rows {
          // In the open line, now the first live line.
          let row = pulled_rows - above;
          let start = open.map_or(row * usize::from(old_width), |line| {
            line.offset_of_row(old_width, row)
          });
          Anchor::Line(0, start + col)
        } else {
          match self.history.locate(above - pulled_rows, old_width) {
            Some((index, row)) => {
              let start = self
                .history
                .get(index)
                .map_or(0, |line| line.offset_of_row(old_width, row));
              Anchor::History(index, start + col)
            }
            None => {
              gone.push(pi);
              continue;
            }
          }
        }
      };
      anchors.push((pi, anchor));
    }
    (anchors, gone)
  }

  /// Moves the placements to where their anchors landed. `found` has a
  /// row (an index into the reflowed rows) and a column for each line
  /// anchor; `bottom` is the absolute row of the first reflowed row, and
  /// history anchors are placed above it, wrapped at `width`. The
  /// placements in `gone`, and those whose anchor is nowhere, are
  /// removed.
  fn place_anchors(
    &mut self,
    anchors: &[(usize, Anchor)],
    found: &[Option<(usize, usize)>],
    bottom: u64,
    reflowed_rows: usize,
    width: u16,
    mut gone: Vec<usize>,
  ) {
    for (slot, (pi, anchor)) in anchors.iter().enumerate() {
      let placed: Option<(u64, u16)> = match anchor {
        Anchor::Line(..) => found[slot].map(|(row, col)| {
          (bottom + row as u64, col.min(usize::from(u16::MAX)) as u16)
        }),
        Anchor::Below(k) => {
          let col = self.placements[*pi].col;
          Some((bottom + (reflowed_rows + k) as u64, col))
        }
        Anchor::History(index, offset) => {
          let Some(line) = self.history.get(*index) else {
            gone.push(*pi);
            continue;
          };
          let (row, col) = line.row_col_of(width, *offset);
          let below = self.history.rows_below(*index, row, width);
          bottom.checked_sub(1 + below as u64).map(|row| (row, col))
        }
      };
      match placed {
        Some((row, col)) => {
          let p = &mut self.placements[*pi];
          p.row = row;
          p.col = col;
        }
        None => gone.push(*pi),
      }
    }
    gone.sort_unstable();
    gone.dedup();
    for pi in gone.into_iter().rev() {
      self.placements.remove(pi);
    }
  }

  /// Rows from the top up to the last one with content or the cursor.
  pub(crate) fn used_rows(&self) -> usize {
    let last_content = self
      .rows
      .iter()
      .rposition(|row| self.used_len(row) > 0)
      .map_or(0, |i| i + 1);
    last_content.max(usize::from(self.pos.row) + 1)
  }

  pub fn saved_pos(&self) -> Pos {
    self.saved_pos
  }

  pub fn saved_origin_mode(&self) -> bool {
    self.saved_origin_mode
  }

  /// Puts back what a snapshot saved: the live rows, rows an older
  /// binary kept as scrollback (joined into lines here), and lines.
  pub fn restore(&mut self, rows: Vec<Arc<Row>>, history_rows: Vec<Row>) {
    self.history = History::new(self.history.capacity());
    self.history.set_width(self.size.width);
    for row in &history_rows {
      self.history.commit(row, &self.tables);
    }
    let height = usize::from(self.size.height);
    let mut rows = rows;
    rows.truncate(height);
    while rows.len() < height {
      rows.push(Arc::new(Row::new(self.size.width)));
    }
    self.rows = rows;
  }

  #[allow(clippy::too_many_arguments)]
  pub fn restore_cursor_state(
    &mut self,
    pos: Pos,
    saved_pos: Pos,
    scroll_top: u16,
    scroll_bottom: u16,
    margins: Option<(u16, u16)>,
    origin_mode: bool,
    saved_origin_mode: bool,
  ) {
    self.pos = pos;
    self.saved_pos = saved_pos;
    self.scroll_top = scroll_top.min(self.size.height - 1);
    self.scroll_bottom = scroll_bottom.min(self.size.height - 1);
    if self.scroll_bottom < self.scroll_top {
      self.scroll_top = 0;
      self.scroll_bottom = self.size.height - 1;
    }
    if let Some((left, right)) = margins {
      let right = right.min(self.size.width - 1);
      if left < right {
        self.margin_left = left;
        self.margin_right = right;
      }
    }
    self.origin_mode = origin_mode;
    self.saved_origin_mode = saved_origin_mode;
  }

  /// A UI grid's reset: everything blank, cursor home, no regions.
  pub fn clear(&mut self) {
    self.pos = Pos::default();
    self.saved_pos = Pos::default();
    self.erase_all(StyleId::DEFAULT);
    self.placements.clear();
    self.placeholder_slots.clear();
    self.scroll_top = 0;
    self.scroll_bottom = self.size.height - 1;
    self.margin_left = 0;
    self.margin_right = self.size.width - 1;
    self.origin_mode = false;
    self.saved_origin_mode = false;
  }

  /// The text of the cells from `(low_x, low_y)` to `(high_x, high_y)`
  /// inclusive, in view coordinates; rows end in a newline unless they
  /// wrapped.
  pub fn get_selected_text(
    &self,
    low_x: i32,
    low_y: i32,
    high_x: i32,
    high_y: i32,
  ) -> String {
    let mut contents = String::new();
    let rows = self.visible_rows();
    let mut buf = [0u8; 4];
    let lines_len = high_y - low_y + 1;
    for i in 0..lines_len {
      let y = low_y + i;
      let Some(row) = usize::try_from(y).ok().and_then(|y| rows.get(y)) else {
        continue;
      };
      let start = if i == 0 { low_x.max(0) } else { 0 } as u16;
      let width: i32 = row.cols().into();
      let end = if i == lines_len - 1 {
        width.min(high_x + 1)
      } else {
        width
      };
      let end = end.max(start as i32) as u16;
      let mut pending_blanks = 0;
      for col in start..end {
        let Some(cell) = row.get(col) else {
          break;
        };
        if cell.is_continuation() {
          continue;
        }
        let text = self.text_of(cell, &mut buf);
        if text.is_empty() {
          pending_blanks += 1;
        } else {
          for _ in 0..pending_blanks {
            contents.push(' ');
          }
          pending_blanks = 0;
          contents.push_str(text);
        }
      }
      if i != lines_len - 1 && !row.wrapped() {
        contents.push('\n');
      }
    }
    contents
  }
}

fn default_tabs(width: u16) -> Vec<bool> {
  (0..width).map(|col| col % 8 == 0 && col != 0).collect()
}

/// Tab stops at a new width: the old ones kept, defaults beyond them.
fn default_tabs_like(old: &[bool], width: u16) -> Vec<bool> {
  (0..width)
    .map(|col| match old.get(usize::from(col)) {
      Some(set) => *set,
      None => col % 8 == 0,
    })
    .collect()
}

/// What a grid logs: each line once, as it was when the cursor left it
/// with a newline, or as it scrolled off when no one had logged it.
#[derive(Clone, Debug, Default)]
struct LineLog {
  /// Rows above this one are logged; edits there do not reach the log.
  frontier: u16,
  /// The start of the open line, already scrolled off the top.
  prefix: String,
  lines: Vec<String>,
  /// What a ready check waits to see in a line.
  wanted: Option<String>,
  /// The needle was seen; it is no longer looked for.
  found: bool,
}

impl LineLog {
  fn push(&mut self, text: String) {
    self.check(&text);
    self.prefix.clear();
    self.lines.push(text);
  }

  fn check(&mut self, text: &str) {
    if self
      .wanted
      .as_ref()
      .is_some_and(|needle| text.contains(needle))
    {
      self.wanted = None;
      self.found = true;
    }
  }
}

/// The most of a logical line the log keeps.
const MAX_LOG_LINE: usize = 16 * 1024;

/// Cuts a log line at `MAX_LOG_LINE` bytes and trims trailing blanks.
fn trim_log_text(text: &mut String) {
  if text.len() > MAX_LOG_LINE {
    let mut cut = MAX_LOG_LINE;
    while !text.is_char_boundary(cut) {
      cut -= 1;
    }
    text.truncate(cut);
  }
  text.truncate(text.trim_end().len());
}

/// Blanks a row in place when nothing else holds it, else replaces it:
/// a row shared with a frame would be copied only to be overwritten.
fn blank_in_place(row: &mut Arc<Row>, width: u16, style: StyleId) {
  match Arc::get_mut(row) {
    Some(row) => row.clear(style),
    None => *row = Arc::new(Row::blank(width, style)),
  }
}

/// Where a placement's top-left cell was before a reflow.
enum Anchor {
  /// On a live logical line, at a cell offset in it.
  Line(usize, usize),
  /// In history line `index`, at a cell offset in it.
  History(usize, usize),
  /// On a blank row below the content, `k` rows down.
  Below(usize),
}

/// Records the position of every wanted offset equal to `offset`.
fn note(
  wanted: &[(usize, usize)],
  found: &mut [Option<(usize, usize)>],
  offset: usize,
  pos: (usize, usize),
) {
  for (off, slot) in wanted {
    if *off == offset {
      found[*slot] = Some(pos);
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

  pub fn rows(self) -> impl Iterator<Item = Self> {
    (self.y..self.bottom()).map(move |y| Rect {
      y,
      height: 1,
      ..self
    })
  }

  /// Splits off up to `height` rows from the top; `self` keeps the rest.
  pub fn take_top(&mut self, height: u16) -> Self {
    let height = height.min(self.height);
    let top = Rect { height, ..*self };
    self.y += height;
    self.height -= height;
    top
  }

  pub fn take_bottom(&mut self, height: u16) -> Self {
    let height = height.min(self.height);
    self.height -= height;
    Rect {
      y: self.y + self.height,
      height,
      ..*self
    }
  }

  pub fn take_left(&mut self, width: u16) -> Self {
    let width = width.min(self.width);
    let left = Rect { width, ..*self };
    self.x += width;
    self.width -= width;
    left
  }

  pub fn take_right(&mut self, width: u16) -> Self {
    let width = width.min(self.width);
    self.width -= width;
    Rect {
      x: self.x + self.width,
      width,
      ..*self
    }
  }

  pub fn centered(self, width: u16, height: u16) -> Self {
    let width = width.min(self.width);
    let height = height.min(self.height);
    Rect {
      x: self.x + (self.width - width) / 2,
      y: self.y + (self.height - height) / 2,
      width,
      height,
    }
  }

  /// The part of `self` inside `other`.
  pub fn intersect(self, other: Rect) -> Rect {
    let x = self.x.max(other.x);
    let y = self.y.max(other.y);
    let right = self.right().min(other.right());
    let bottom = self.bottom().min(other.bottom());
    Rect {
      x,
      y,
      width: right.saturating_sub(x),
      height: bottom.saturating_sub(y),
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

#[cfg(test)]
mod tests {
  use super::*;
  use crate::term::image::SrcRect;

  fn text(grid: &Grid, row: u16) -> String {
    let mut out = String::new();
    let mut buf = [0u8; 4];
    let Some(row) = grid.visible_row(row) else {
      return out;
    };
    for cell in &row.cells {
      if cell.is_continuation() {
        continue;
      }
      let s = grid.text_of(*cell, &mut buf);
      out.push_str(if s.is_empty() { " " } else { s });
    }
    out.trim_end().to_string()
  }

  fn write(grid: &mut Grid, row: u16, text: &str) {
    let mut col = 0;
    for c in text.chars() {
      let w = super::super::unicode::scalar_width(c).unwrap_or(1).max(1);
      grid.write_cell(
        Pos { row, col },
        CellText::scalar(c, w),
        StyleId::DEFAULT,
      );
      col += u16::from(w);
    }
  }

  #[test]
  fn scroll_commits_to_history_and_views_it() {
    let mut grid = Grid::new(
      Size {
        width: 6,
        height: 2,
      },
      10,
    );
    write(&mut grid, 0, "one");
    write(&mut grid, 1, "two");
    grid.scroll_up(1, StyleId::DEFAULT);
    assert_eq!(text(&grid, 0), "two");
    assert_eq!(text(&grid, 1), "");
    assert_eq!(grid.history().len(), 1);
    assert_eq!(grid.max_scrollback(), 1);
    grid.set_scrollback(1);
    assert_eq!(text(&grid, 0), "one");
    assert_eq!(text(&grid, 1), "two");
    // New output keeps the view where it was; the view is rebuilt once
    // the chunk is processed (the screen settles it).
    write(&mut grid, 1, "three!");
    grid.scroll_up(1, StyleId::DEFAULT);
    grid.settle_view();
    assert_eq!(grid.scrollback(), 2);
    assert_eq!(text(&grid, 0), "one");
    assert_eq!(text(&grid, 1), "two");
    grid.set_scrollback(0);
    assert_eq!(text(&grid, 0), "three!");
  }

  #[test]
  fn reflow_rejoins_wrapped_rows() {
    let mut grid = Grid::new(
      Size {
        width: 4,
        height: 3,
      },
      10,
    );
    write(&mut grid, 0, "abcd");
    grid.row_mut(0).wrap(true);
    write(&mut grid, 1, "ef");
    grid.pos = Pos { row: 1, col: 2 };
    grid.set_size(Size {
      width: 8,
      height: 3,
    });
    assert_eq!(text(&grid, 0), "abcdef");
    assert_eq!(text(&grid, 1), "");
    assert_eq!(grid.pos(), Pos { row: 0, col: 6 });
    grid.set_size(Size {
      width: 3,
      height: 3,
    });
    assert_eq!(text(&grid, 0), "abc");
    assert_eq!(text(&grid, 1), "def");
    // After the last cell of a full row: a pending wrap, not a row of
    // its own.
    assert_eq!(grid.pos(), Pos { row: 1, col: 3 });
  }

  #[test]
  fn a_pending_wrap_cursor_keeps_its_row_through_a_resize() {
    let mut grid = Grid::new(
      Size {
        width: 4,
        height: 3,
      },
      10,
    );
    write(&mut grid, 0, "abcd");
    // As after writing the last cell: one past it, nothing wrapped yet.
    grid.pos = Pos { row: 0, col: 4 };
    grid.set_size(Size {
      width: 4,
      height: 2,
    });
    assert_eq!(text(&grid, 0), "abcd");
    assert_eq!(text(&grid, 1), "");
    assert_eq!(grid.pos(), Pos { row: 0, col: 4 });
    assert!(!grid.drawing_row(0).unwrap().wrapped());
    grid.set_size(Size {
      width: 8,
      height: 2,
    });
    assert_eq!(text(&grid, 0), "abcd");
    assert_eq!(grid.pos(), Pos { row: 0, col: 4 });
    grid.set_size(Size {
      width: 3,
      height: 2,
    });
    assert_eq!(text(&grid, 0), "abc");
    assert_eq!(text(&grid, 1), "d");
    assert_eq!(grid.pos(), Pos { row: 1, col: 1 });
  }

  #[test]
  fn wrapping_a_wide_cell_pads_blank_room_only() {
    let size = Size {
      width: 4,
      height: 2,
    };
    // Blank room at the end of the row becomes padding...
    let mut grid = Grid::new(size, 0);
    write(&mut grid, 0, "abc");
    grid.set_pos(Pos { row: 0, col: 3 });
    grid.col_wrap(2, StyleId::DEFAULT);
    assert_eq!(grid.pos(), Pos { row: 1, col: 0 });
    assert!(
      grid
        .drawing_cell(Pos { row: 0, col: 3 })
        .unwrap()
        .text
        .is_filler()
    );
    assert!(grid.drawing_row(0).unwrap().wrapped());
    assert_eq!(grid.log_text("", 1), "abc");
    // ...a glyph there stays, and the line goes on past it.
    let mut grid = Grid::new(size, 0);
    write(&mut grid, 0, "abcd");
    grid.set_pos(Pos { row: 0, col: 3 });
    grid.col_wrap(2, StyleId::DEFAULT);
    assert_eq!(grid.pos(), Pos { row: 1, col: 0 });
    assert_eq!(text(&grid, 0), "abcd");
    assert!(grid.drawing_row(0).unwrap().wrapped());
    assert_eq!(grid.log_text("", 1), "abcd");
  }

  fn placement(row: u64, col: u16) -> Placement {
    Placement {
      key: 1,
      id: 0,
      row,
      col,
      cols: 2,
      rows: 1,
      src: SrcRect {
        x: 0,
        y: 0,
        w: 2,
        h: 1,
      },
      offset_x: 0,
      offset_y: 0,
      z: 0,
      virtual_: false,
      cells: false,
    }
  }

  fn shown(grid: &Grid) -> Vec<(u16, u16)> {
    grid
      .visible_placements(&grid.visible_rows(), &|_| None)
      .iter()
      .map(|p| (p.row, p.col))
      .collect()
  }

  #[test]
  fn reflow_keeps_images_with_their_text() {
    let mut grid = Grid::new(
      Size {
        width: 8,
        height: 4,
      },
      10,
    );
    write(&mut grid, 0, "first");
    write(&mut grid, 1, "abcdefgh");
    grid.row_mut(1).wrap(true);
    write(&mut grid, 2, "ij");
    // An image on "ij", the wrapped line's second row.
    grid.add_placement(placement(2, 0));
    grid.pos = Pos { row: 3, col: 0 };

    // "firs", "t", "abcd", "efgh", "ij": the image's row is now 4.
    grid.set_size(Size {
      width: 4,
      height: 6,
    });
    assert_eq!(text(&grid, 4), "ij");
    assert_eq!(shown(&grid), vec![(4, 0)]);

    grid.set_size(Size {
      width: 10,
      height: 6,
    });
    assert_eq!(text(&grid, 1), "abcdefghij");
    assert_eq!(shown(&grid), vec![(1, 8)]);

    // Shrinking the height sends the top rows to history; the image
    // keeps its line and shows when scrolled back.
    grid.set_size(Size {
      width: 10,
      height: 1,
    });
    assert_eq!(grid.history().len(), 2);
    assert_eq!(shown(&grid), vec![]);
    grid.set_scrollback(1);
    assert_eq!(text(&grid, 0), "abcdefghij");
    assert_eq!(shown(&grid), vec![(0, 8)]);

    // A width change rewraps history too; the image follows its cell.
    grid.set_size(Size {
      width: 5,
      height: 1,
    });
    assert_eq!(grid.max_scrollback(), 3);
    grid.set_scrollback(1);
    assert_eq!(text(&grid, 0), "fghij");
    assert_eq!(shown(&grid), vec![(0, 3)]);
    grid.set_scrollback(2);
    assert_eq!(text(&grid, 0), "abcde");
    assert_eq!(shown(&grid), vec![]);
  }

  #[test]
  fn scrolling_at_the_top_of_full_history_keeps_the_offset_in_range() {
    let mut grid = Grid::new(
      Size {
        width: 4,
        height: 1,
      },
      2,
    );
    for text in ["a", "b", "c"] {
      write(&mut grid, 0, text);
      grid.scroll_up(1, StyleId::DEFAULT);
    }
    grid.set_scrollback(usize::MAX);
    assert_eq!(grid.scrollback(), 2);
    assert_eq!(text(&grid, 0), "b");
    // Three more lines: history drops as many; the view stays at its top.
    for text in ["d", "e", "f"] {
      write(&mut grid, 0, text);
      grid.scroll_up(1, StyleId::DEFAULT);
    }
    grid.settle_view();
    assert_eq!(grid.scrollback(), 2);
    assert_eq!(grid.max_scrollback(), 2);
    assert_eq!(text(&grid, 0), "e");
  }

  #[test]
  fn logical_line_text_joins_wrapped_rows() {
    let mut grid = Grid::new(
      Size {
        width: 4,
        height: 2,
      },
      10,
    );
    write(&mut grid, 0, "abcd");
    grid.row_mut(0).wrap(true);
    write(&mut grid, 1, "ef");
    assert_eq!(grid.log_text("", 1), "abcdef");
    assert_eq!(grid.log_text("", 0), "abcd");
    // The start of a line that scrolled off still counts.
    grid.enable_log();
    grid.scroll_up(1, StyleId::DEFAULT);
    assert!(grid.history().is_open());
    assert_eq!(grid.open_log_line().as_deref(), Some("abcdef"));
  }

  /// Every logical line (history and screen rows joined by their wrap
  /// flags), trailing blanks trimmed.
  fn logical_lines(grid: &Grid) -> Vec<String> {
    let mut lines = Vec::new();
    let history = grid.history();
    let open = history.is_open();
    for i in 0..history.len() {
      let line = history.get(i).unwrap();
      // The open line's trailing blanks lead into its continuation.
      if open && i + 1 == history.len() {
        lines.push(line.text.clone());
      } else {
        lines.push(line.text.trim_end().to_string());
      }
    }
    let mut current = String::new();
    let mut continuing = open;
    let used = grid.used_rows();
    for (i, row) in grid.rows().iter().enumerate().take(used) {
      let mut text = String::new();
      row.push_text(0..row.cols(), &grid.tables().graphemes, &mut text);
      if row.wrapped() && i + 1 < used {
        current.push_str(&text);
      } else {
        current.push_str(text.trim_end());
        if continuing {
          let last = lines.pop().unwrap_or_default();
          lines.push(last + &current);
          continuing = false;
        } else {
          lines.push(std::mem::take(&mut current));
        }
        current.clear();
      }
    }
    if !current.is_empty() {
      lines.push(current);
    }
    lines
      .into_iter()
      .map(|l| l.trim_end().to_string())
      .collect()
  }

  proptest::proptest! {
    /// Resizing narrow and back keeps every logical line, and the
    /// placements stay on their lines.
    #[test]
    fn reflow_round_trip_keeps_lines(
      lines in proptest::collection::vec("[a-z\u{6d4b}\u{1F600} ]{0,120}", 1..12),
      narrow in 1u16..40,
      height in 2u16..8,
    ) {
      let width = 60u16;
      let mut grid = Grid::new(Size { width, height }, 100);
      let style = StyleId::DEFAULT;
      // Type the lines through the wrapping rules a screen uses.
      for line in &lines {
        for c in line.chars() {
          let w = super::super::unicode::scalar_width(c).unwrap_or(1).max(1);
          if grid.pos().col + u16::from(w) > width {
            grid.col_wrap(u16::from(w), style);
          }
          let pos = grid.pos();
          grid.write_cell(pos, CellText::scalar(c, w), style);
          grid.col_inc(u16::from(w));
        }
        grid.col_set(0);
        grid.row_inc_scroll(1, style);
      }
      let before = logical_lines(&grid);
      grid.set_size(Size { width: narrow, height });
      grid.set_size(Size { width, height });
      let after = logical_lines(&grid);
      proptest::prop_assert_eq!(before, after);
    }
  }

  #[test]
  fn growing_pulls_history_back_onto_the_screen() {
    // Taller: lines that scrolled off come back above.
    let mut grid = Grid::new(
      Size {
        width: 10,
        height: 4,
      },
      10,
    );
    for text in ["l1", "l2", "l3", "l4", "l5", "l6"] {
      let row = grid.pos().row;
      write(&mut grid, row, text);
      grid.row_inc_scroll(1, StyleId::DEFAULT);
    }
    assert_eq!(grid.history().len(), 3);
    grid.set_size(Size {
      width: 10,
      height: 6,
    });
    let rows: Vec<String> = (0..6).map(|r| text(&grid, r)).collect();
    assert_eq!(rows, ["l2", "l3", "l4", "l5", "l6", ""]);
    assert_eq!(grid.history().len(), 1);
    assert_eq!(grid.pos(), Pos { row: 5, col: 0 });

    // Narrow and back: what wrapping pushed out returns, and a line
    // that does not fit whole is split at the top.
    let mut grid = Grid::new(
      Size {
        width: 6,
        height: 2,
      },
      10,
    );
    write(&mut grid, 0, "abcdef");
    grid.row_mut(0).wrap(true);
    write(&mut grid, 1, "gh");
    grid.pos = Pos { row: 1, col: 2 };
    grid.set_size(Size {
      width: 3,
      height: 2,
    });
    assert_eq!(
      (text(&grid, 0), text(&grid, 1)),
      ("def".into(), "gh".into())
    );
    assert_eq!(grid.history().len(), 1);
    assert!(grid.history().is_open());
    grid.set_size(Size {
      width: 6,
      height: 2,
    });
    assert_eq!(
      (text(&grid, 0), text(&grid, 1)),
      ("abcdef".into(), "gh".into())
    );
    assert_eq!(grid.history().len(), 0);
    assert_eq!(grid.pos(), Pos { row: 1, col: 2 });
  }

  #[test]
  fn an_image_after_its_text_stays_there_through_a_narrow_resize() {
    let mut grid = Grid::new(
      Size {
        width: 60,
        height: 8,
      },
      100,
    );
    write(&mut grid, 2, "image after this text: ");
    grid.add_placement(placement(2, 23));
    grid.pos = Pos { row: 7, col: 0 };
    for narrow in [5u16, 2, 1] {
      grid.set_size(Size {
        width: narrow,
        height: 8,
      });
      grid.set_size(Size {
        width: 60,
        height: 8,
      });
      // The row the text ends on; the column within a cell of where it
      // was when the screen was too narrow for any room after the text.
      let (row, col) = shown(&grid)[0];
      assert_eq!(
        text(&grid, row),
        "image after this text:",
        "narrow {narrow}"
      );
      assert_eq!(col, 23, "narrow {narrow}");
    }
  }

  #[test]
  fn wide_glyphs_survive_a_one_column_screen() {
    let mut grid = Grid::new(
      Size {
        width: 10,
        height: 3,
      },
      100,
    );
    write(&mut grid, 0, "a\u{6d4b}b\u{8bd5}");
    grid.pos = Pos { row: 2, col: 0 };
    for narrow in [1u16, 2] {
      grid.set_size(Size {
        width: narrow,
        height: 3,
      });
      grid.set_size(Size {
        width: 10,
        height: 3,
      });
      assert_eq!(text(&grid, 0), "a\u{6d4b}b\u{8bd5}", "narrow {narrow}");
    }
  }

  #[test]
  fn placements_cut_at_the_view_top() {
    let mut grid = Grid::new(
      Size {
        width: 4,
        height: 2,
      },
      10,
    );
    let mut tall = placement(0, 0);
    tall.rows = 3;
    tall.src.h = 30;
    grid.add_placement(tall);
    write(&mut grid, 0, "a");
    grid.scroll_up(1, StyleId::DEFAULT);
    let shown = grid.visible_placements(&grid.visible_rows(), &|_| None);
    assert_eq!((shown[0].row, shown[0].rows), (0, 2));
    assert_eq!((shown[0].src.y, shown[0].src.h), (10, 20));
    grid.set_scrollback(1);
    let shown = grid.visible_placements(&grid.visible_rows(), &|_| None);
    assert_eq!((shown[0].row, shown[0].rows, shown[0].src.y), (0, 3, 0));
  }

  #[test]
  fn reflow_overflow_goes_to_history() {
    let mut grid = Grid::new(
      Size {
        width: 4,
        height: 2,
      },
      10,
    );
    write(&mut grid, 0, "abcd");
    grid.row_mut(0).wrap(true);
    write(&mut grid, 1, "efgh");
    grid.pos = Pos { row: 1, col: 3 };
    grid.set_size(Size {
      width: 2,
      height: 2,
    });
    // "abcdefgh" is four rows of two; the last two stay.
    assert_eq!(text(&grid, 0), "ef");
    assert_eq!(text(&grid, 1), "gh");
    assert_eq!(grid.pos(), Pos { row: 1, col: 1 });
    assert_eq!(grid.history().len(), 1);
    assert!(grid.history().is_open());
    // Growing again pulls the open line back.
    grid.set_size(Size {
      width: 8,
      height: 2,
    });
    assert_eq!(text(&grid, 0), "abcdefgh");
    assert_eq!(grid.history().len(), 0);
    assert_eq!(grid.pos(), Pos { row: 0, col: 7 });
  }

  #[test]
  fn wide_cells_stay_whole() {
    let mut grid = Grid::new(
      Size {
        width: 6,
        height: 1,
      },
      0,
    );
    write(&mut grid, 0, "a\u{6d4b}b");
    assert_eq!(text(&grid, 0), "a\u{6d4b}b");
    assert!(
      grid
        .drawing_cell(Pos { row: 0, col: 2 })
        .unwrap()
        .is_continuation()
    );
    // Overwriting the right half blanks the left.
    grid.write_cell(
      Pos { row: 0, col: 2 },
      CellText::scalar('X', 1),
      StyleId::DEFAULT,
    );
    assert_eq!(text(&grid, 0), "a Xb");
    write(&mut grid, 0, "\u{6d4b}");
    grid.write_ascii_row(Pos { row: 0, col: 1 }, b"yz", StyleId::DEFAULT);
    assert_eq!(text(&grid, 0), " yzb");
  }

  #[test]
  fn tabs_and_margins() {
    let mut grid = Grid::new(
      Size {
        width: 20,
        height: 2,
      },
      0,
    );
    grid.col_tab();
    assert_eq!(grid.pos().col, 8);
    grid.col_tab();
    assert_eq!(grid.pos().col, 16);
    grid.col_tab();
    assert_eq!(grid.pos().col, 19);
    grid.tab_back(1);
    assert_eq!(grid.pos().col, 16);
    grid.col_set(3);
    grid.set_tab();
    grid.col_set(0);
    grid.tab_forward(1);
    assert_eq!(grid.pos().col, 3);

    grid.set_margins(2, 9);
    assert!(grid.margins_active());
    grid.set_pos(Pos { row: 0, col: 9 });
    grid.col_wrap(1, StyleId::DEFAULT);
    assert_eq!(grid.pos(), Pos { row: 0, col: 9 });
    grid.col_inc(1);
    grid.col_wrap(1, StyleId::DEFAULT);
    assert_eq!(grid.pos(), Pos { row: 1, col: 2 });
  }

  #[test]
  fn selection_text() {
    let mut grid = Grid::new(
      Size {
        width: 6,
        height: 2,
      },
      0,
    );
    write(&mut grid, 0, "ab  cd");
    write(&mut grid, 1, "ef");
    assert_eq!(grid.get_selected_text(0, 0, 5, 1), "ab  cd\nef");
    assert_eq!(grid.get_selected_text(2, 0, 1, 1), "  cd\nef");
  }
}
