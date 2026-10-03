use crate::term::grid::Rect;

/// Cursor and scroll over `len` rows owned by the caller. The view follows
/// the cursor, keeping `margin` rows around it, until `scroll_by`.
pub struct ListState {
  len: usize,
  selected: usize,
  top: usize,
  /// `top` as last drawn: a click lands on what is on screen.
  drawn_top: usize,
  area: Rect,
  margin: usize,
  follow: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ListRow {
  pub index: usize,
  pub area: Rect,
  pub selected: bool,
}

impl ListState {
  pub fn new(margin: usize) -> Self {
    ListState {
      len: 0,
      selected: 0,
      top: 0,
      drawn_top: 0,
      area: Rect::default(),
      margin,
      follow: true,
    }
  }

  /// Back to the first row, scrolled to the top.
  pub fn reset(&mut self, len: usize) {
    self.len = len;
    self.selected = 0;
    self.top = 0;
    self.follow = true;
  }

  /// Rows changed; keeps the scroll.
  pub fn set(&mut self, len: usize, selected: usize) {
    self.len = len;
    self.selected = selected.min(len.saturating_sub(1));
  }

  pub fn selected(&self) -> Option<usize> {
    (self.len > 0).then_some(self.selected)
  }

  pub fn select(&mut self, index: usize) {
    self.selected = index.min(self.len.saturating_sub(1));
    self.follow = true;
  }

  /// Wraps around.
  pub fn next(&mut self) {
    if self.len > 0 {
      self.select((self.selected + 1) % self.len);
    }
  }

  /// Wraps around.
  pub fn prev(&mut self) {
    if self.len > 0 {
      self.select((self.selected + self.len - 1) % self.len);
    }
  }

  pub fn move_by(&mut self, delta: isize) {
    self.select(self.selected.saturating_add_signed(delta));
  }

  /// Moves the view, not the cursor.
  pub fn scroll_by(&mut self, delta: isize) {
    self.top = self.top.saturating_add_signed(delta);
    self.follow = false;
  }

  /// Rows in the view.
  pub fn page(&self) -> usize {
    self.area.height as usize
  }

  /// Lays the visible rows out in `area`.
  pub fn rows(&mut self, area: Rect) -> impl Iterator<Item = ListRow> + use<> {
    self.area = area;
    let height = area.height as usize;
    if self.follow && height > 0 {
      let margin = self.margin.min((height - 1) / 2);
      self.top = self.top.min(self.selected.saturating_sub(margin));
      self.top = self
        .top
        .max((self.selected + margin + 1).saturating_sub(height));
    }
    self.top = self.top.min(self.len.saturating_sub(height));
    self.drawn_top = self.top;
    let (top, selected) = (self.top, self.selected);
    let end = (top + height).min(self.len);
    (top..end)
      .zip(area.rows())
      .map(move |(index, area)| ListRow {
        index,
        area,
        selected: index == selected,
      })
  }

  /// The row drawn at a point by the last `rows`.
  pub fn row_at(&self, x: u16, y: u16) -> Option<ListRow> {
    if !self.area.contains(x, y) {
      return None;
    }
    let index = self.drawn_top + (y - self.area.y) as usize;
    (index < self.len).then_some(ListRow {
      index,
      area: Rect {
        y,
        height: 1,
        ..self.area
      },
      selected: index == self.selected,
    })
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn visible(list: &mut ListState, height: u16) -> Vec<usize> {
    list
      .rows(Rect::new(0, 0, 10, height))
      .map(|r| r.index)
      .collect()
  }

  #[test]
  fn follows_the_cursor_with_a_margin() {
    let mut list = ListState::new(1);
    list.reset(10);
    assert_eq!(visible(&mut list, 4), [0, 1, 2, 3]);

    list.select(3);
    assert_eq!(visible(&mut list, 4), [1, 2, 3, 4], "one row below");
    list.select(2);
    assert_eq!(visible(&mut list, 4), [1, 2, 3, 4], "stays while in view");
    list.select(1);
    assert_eq!(visible(&mut list, 4), [0, 1, 2, 3], "one row above");
    list.select(100);
    assert_eq!(list.selected(), Some(9));
    assert_eq!(visible(&mut list, 4), [6, 7, 8, 9]);
  }

  #[test]
  fn scrolling_leaves_the_cursor_until_it_moves() {
    let mut list = ListState::new(0);
    list.reset(10);
    list.scroll_by(5);
    assert_eq!(visible(&mut list, 3), [5, 6, 7]);
    assert_eq!(list.selected(), Some(0));
    list.scroll_by(100);
    assert_eq!(visible(&mut list, 3), [7, 8, 9], "no room past the end");
    list.next();
    assert_eq!(visible(&mut list, 3), [1, 2, 3]);
  }

  #[test]
  fn wraps_and_empties() {
    let mut list = ListState::new(0);
    list.reset(3);
    list.prev();
    assert_eq!(list.selected(), Some(2));
    list.next();
    assert_eq!(list.selected(), Some(0));
    list.set(0, 0);
    assert_eq!(list.selected(), None);
    list.next();
    assert_eq!(visible(&mut list, 3), Vec::<usize>::new());
  }

  #[test]
  fn finds_the_row_at_a_point() {
    let mut list = ListState::new(0);
    list.reset(10);
    list.select(5);
    let area = Rect::new(2, 1, 8, 3);
    let drawn: Vec<ListRow> = list.rows(area).collect();
    assert_eq!(list.row_at(4, 2), Some(drawn[1]));
    assert_eq!(list.row_at(1, 2), None);
    assert_eq!(list.row_at(4, 4), None);

    // Until the next draw, a point is still the row drawn there.
    list.scroll_by(3);
    list.select(9);
    assert_eq!(list.row_at(4, 2).map(|row| row.index), Some(drawn[1].index));
  }
}
