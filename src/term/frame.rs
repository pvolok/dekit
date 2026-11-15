//! What observers of a screen read: an immutable picture of it.
//!
//! The screen's owner publishes a `Frame` whenever observers should
//! paint. Rows and tables are shared `Arc`s: publishing is O(rows), and
//! the owner's next write to a row copies it (`Arc::make_mut`), so a
//! frame never changes under a reader. Styles read from a frame are the
//! styles as shown: screen-wide colors (OSC 10/11, reverse video) are
//! already applied to its style table.

use std::sync::{Arc, Mutex};

use super::{
  cell::{Cell, GraphemeTable},
  color::{Color, Rgb},
  common::{CursorStyle, Size},
  grid::{Grid, Pos},
  image::{ImageTable, VisiblePlacement},
  row::Row,
  style::{Link, LinkTable, Style, StyleTable},
};

/// How a screen's cells are colored beyond their own styles: the default
/// colors a program set (OSC 10/11) and reverse video (DECSCNM). Cells
/// keep their interned styles; the screen publishes a style table with
/// these applied.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct FrameColors {
  /// What `Color::Default` foregrounds show as, when a program set it.
  pub fg: Option<Rgb>,
  pub bg: Option<Rgb>,
  /// `?5`: every cell shows with its colors swapped.
  pub reverse: bool,
}

impl FrameColors {
  pub const PLAIN: FrameColors = FrameColors {
    fg: None,
    bg: None,
    reverse: false,
  };

  pub fn is_plain(&self) -> bool {
    *self == FrameColors::PLAIN
  }

  /// Every style of a table as it shows under these colors; ids are
  /// kept, so the same cells read as the shown styles.
  pub fn apply_table(&self, table: &StyleTable) -> StyleTable {
    table.map(|style| self.apply(style))
  }

  /// A style as it shows under these colors.
  #[inline]
  pub fn apply(&self, style: Style) -> Style {
    if self.is_plain() {
      return style;
    }
    let mut shown = style;
    if let Some(fg) = self.fg
      && shown.fg == Color::Default
    {
      shown.fg = fg.into();
    }
    if let Some(bg) = self.bg
      && shown.bg == Color::Default
    {
      shown.bg = bg.into();
    }
    if self.reverse {
      shown = shown.set_inverse(!shown.inverse());
    }
    shown
  }
}

/// The interned tables one grid's cells refer to.
#[derive(Clone, Debug, Default)]
pub struct Tables {
  pub styles: Arc<StyleTable>,
  pub graphemes: Arc<GraphemeTable>,
  pub links: Arc<LinkTable>,
}

#[derive(Clone, Debug)]
pub struct Frame {
  pub size: Size,
  /// `size.height` rows, the ones in view.
  pub rows: Vec<Arc<Row>>,
  pub tables: Tables,
  /// Where the cursor shows; `None` when hidden.
  pub cursor: Option<Pos>,
  pub cursor_style: CursorStyle,
  pub title: Arc<str>,
  /// The images placements refer to.
  pub images: Arc<ImageTable>,
  /// Image placements in view, in view coordinates.
  pub placements: Vec<VisiblePlacement>,
  /// The mouse reports the program asked for.
  pub mouse: crate::term::MouseProtocolMode,
}

impl Frame {
  /// A frame of a grid drawn directly (a UI), not a VT screen.
  pub fn of_grid(grid: &Grid) -> Frame {
    let rows = grid.visible_rows();
    let placements = grid.visible_placements(&rows, &|_| None);
    Frame {
      size: grid.size(),
      rows,
      tables: grid.tables().clone(),
      cursor: grid.cursor_pos,
      cursor_style: grid.cursor_style,
      title: Arc::from(""),
      images: grid.image_table().clone(),
      placements,
      mouse: Default::default(),
    }
  }

  pub fn empty(size: Size) -> Frame {
    Frame::of_grid(&Grid::new(size, 0))
  }

  pub fn row(&self, row: u16) -> Option<&Row> {
    self.rows.get(usize::from(row)).map(Arc::as_ref)
  }

  pub fn cell(&self, row: u16, col: u16) -> Option<Cell> {
    self.row(row).and_then(|r| r.get(col))
  }

  /// The cell's style as shown.
  pub fn style(&self, cell: Cell) -> Style {
    self.tables.styles.get(cell.style)
  }

  pub fn text<'a>(&'a self, cell: Cell, buf: &'a mut [u8; 4]) -> &'a str {
    cell.text.as_str(buf, &self.tables.graphemes)
  }

  pub fn link(&self, style: &Style) -> Option<&Link> {
    self.tables.links.get(style.link)
  }

  /// The plain text of `width` cells of a row from `start`: blanks are
  /// spaces, the right halves of wide cells nothing.
  pub fn row_text(&self, row: u16, start: u16, width: u16) -> String {
    let mut out = String::new();
    if let Some(row) = self.row(row) {
      let cols = start..start.saturating_add(width);
      row.push_text(cols, &self.tables.graphemes, &mut out);
    }
    out
  }
}

/// Where a screen's owner publishes frames and its observers read them.
#[derive(Clone)]
pub struct ScreenHandle(Arc<Mutex<Arc<Frame>>>);

impl std::fmt::Debug for ScreenHandle {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_tuple("ScreenHandle").finish()
  }
}

impl ScreenHandle {
  pub fn new(frame: Frame) -> Self {
    ScreenHandle(Arc::new(Mutex::new(Arc::new(frame))))
  }

  /// The latest frame.
  pub fn load(&self) -> Arc<Frame> {
    match self.0.lock() {
      Ok(frame) => frame.clone(),
      Err(poisoned) => poisoned.into_inner().clone(),
    }
  }

  pub fn publish(&self, frame: Frame) {
    let frame = Arc::new(frame);
    match self.0.lock() {
      Ok(mut slot) => *slot = frame,
      Err(poisoned) => *poisoned.into_inner() = frame,
    }
  }
}
