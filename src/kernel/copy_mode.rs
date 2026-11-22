use crate::term::Grid;

/// A frozen copy of a screen's grid (cheap: rows and history are shared
/// until written) with a selection over it.
pub struct CopyState {
  snapshot: Grid,
  start: Pos,
  end: Option<Pos>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Pos {
  pub y: i32,
  pub x: i32,
}

#[derive(Clone, Copy, Debug)]
pub enum CopyMove {
  Up,
  Down,
  Left,
  Right,
}

impl CopyState {
  /// Freezes `grid` and starts copy mode with the cursor on the bottom row.
  pub fn new(grid: Grid) -> Self {
    let y = (grid.size().height as i32 - 1).max(0);
    CopyState {
      snapshot: grid,
      start: Pos { y, x: 0 },
      end: None,
    }
  }

  pub fn snapshot(&self) -> &Grid {
    &self.snapshot
  }

  /// The frozen grid, to follow the observers' size.
  pub fn snapshot_mut(&mut self) -> &mut Grid {
    &mut self.snapshot
  }

  pub fn start(&self) -> Pos {
    self.start
  }

  pub fn end(&self) -> Option<Pos> {
    self.end
  }

  /// The position the cursor is currently driving: the selection cursor once a
  /// selection is started, otherwise the lone anchor.
  pub fn cursor(&self) -> Pos {
    self.end.unwrap_or(self.start)
  }

  fn cursor_mut(&mut self) -> &mut Pos {
    self.end.as_mut().unwrap_or(&mut self.start)
  }

  pub fn move_cursor(&mut self, dir: CopyMove) {
    let width = self.snapshot.size().width as i32;
    let height = self.snapshot.size().height as i32;
    let history_rows = self.snapshot.max_scrollback() as i32;
    let pos = self.cursor_mut();
    match dir {
      CopyMove::Up => {
        if pos.y > -history_rows {
          pos.y -= 1;
        }
      }
      CopyMove::Down => {
        if pos.y + 1 < height {
          pos.y += 1;
        }
      }
      CopyMove::Left => {
        if pos.x > 0 {
          pos.x -= 1;
        }
      }
      CopyMove::Right => {
        if pos.x + 1 < width {
          pos.x += 1;
        }
      }
    }
  }

  /// Anchors the selection at the current cursor and begins extending it.
  pub fn begin_selection(&mut self) {
    if self.end.is_none() {
      self.end = Some(self.start);
    }
  }

  /// Converts a present-screen cell (row, col) to a copy-mode position,
  /// accounting for the current scroll offset.
  pub fn pos_at(&self, row: u16, col: u16) -> Pos {
    Pos {
      y: row as i32 - self.snapshot.scrollback() as i32,
      x: col as i32,
    }
  }

  /// Places the selection anchor at `pos`, clearing any active selection.
  pub fn set_anchor(&mut self, pos: Pos) {
    self.start = pos;
    self.end = None;
  }

  /// Extends the selection to `pos`.
  pub fn set_extent(&mut self, pos: Pos) {
    self.end = Some(pos);
  }

  pub fn scroll_up(&mut self, n: usize) {
    let offset = self.snapshot.scrollback().saturating_add(n);
    self.snapshot.set_scrollback(offset);
  }

  pub fn scroll_down(&mut self, n: usize) {
    let offset = self.snapshot.scrollback().saturating_sub(n);
    self.snapshot.set_scrollback(offset);
  }

  /// Scrolls to the bottom and puts the cursor on the bottom row, in the
  /// same column: a started selection extends to it.
  pub fn scroll_to_bottom(&mut self) {
    self.snapshot.set_scrollback(0);
    let y = (self.snapshot.size().height as i32 - 1).max(0);
    self.cursor_mut().y = y;
  }

  /// Current scroll offset (rows above the live bottom).
  pub fn scrollback(&self) -> usize {
    self.snapshot.scrollback()
  }

  /// How far back the view can go.
  pub fn max_scrollback(&self) -> usize {
    self.snapshot.max_scrollback()
  }

  /// Extracts the selected text, or `None` if no selection has been started.
  pub fn selected_text(&self) -> Option<String> {
    let end = self.end?;
    let (low, high) = Pos::to_low_high(self.start, end);
    // Selection coordinates are relative to the live screen's top row;
    // the snapshot reads in view coordinates.
    let offset = self.snapshot.scrollback() as i32;
    Some(self.snapshot.get_selected_text(
      low.x,
      low.y + offset,
      high.x,
      high.y + offset,
    ))
  }
}

impl Pos {
  pub fn to_low_high(a: Pos, b: Pos) -> (Pos, Pos) {
    if a.y < b.y || (a.y == b.y && a.x < b.x) {
      (a, b)
    } else {
      (b, a)
    }
  }

  /// Whether `target` falls inside the inclusive selection rectangle (in
  /// reading order) spanned by `start`..`end`.
  pub fn within(start: Pos, end: Pos, target: Pos) -> bool {
    let (low, high) = Pos::to_low_high(start, end);
    let Pos { y, x } = target;
    if y > low.y {
      y < high.y || (y == high.y && x <= high.x)
    } else if y == low.y {
      if y < high.y {
        x >= low.x
      } else if y == high.y {
        x >= low.x && x <= high.x
      } else {
        false
      }
    } else {
      false
    }
  }
}
