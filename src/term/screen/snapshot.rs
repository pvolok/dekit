//! The screen in an upgrade snapshot, and back.

use std::{collections::HashMap, sync::Arc};

use crate::upgrade::snapshot as snap;

use super::{CharSet, KittyStack, Modes, Screen};
use crate::term::{
  cell::{Cell, CellText},
  color::Rgb,
  common::{CursorStyle, Size},
  frame::Tables,
  grid::{Grid, Pos},
  history::push_run,
  image::{
    Format, Image, ImageData, ImageKey, ImageStore, Placement, SrcRect,
    store::{AnimationCommand, AnimationFrame, MAX_STORE_BYTES, fresh_key},
  },
  modes::{MouseProtocolEncoding, MouseProtocolMode},
  row::Row,
  style::{Link, LinkId, LinkTable, Style, StyleId, Underline},
  unicode,
};

/// One of the dynamic colors a snapshot saved: fg, bg, cursor.
fn dynamic_color(saved: &snap::Screen, index: usize) -> Option<Rgb> {
  saved
    .dynamic_colors
    .and_then(|colors| colors[index])
    .map(|(r, g, b)| Rgb(r, g, b))
}

/// Deduplicates styles into the snapshot's attrs table.
#[derive(Default)]
struct AttrsTable {
  entries: Vec<snap::Attrs>,
  index: HashMap<snap::Attrs, usize>,
}

impl AttrsTable {
  fn index(&mut self, style: Style, links: &LinkTable) -> usize {
    let key = attrs_snapshot(style, links);
    if let Some(index) = self.index.get(&key) {
      return *index;
    }
    self.entries.push(key.clone());
    self.index.insert(key, self.entries.len() - 1);
    self.entries.len() - 1
  }
}

fn attrs_snapshot(style: Style, links: &LinkTable) -> snap::Attrs {
  let bits = style.attrs_bits();
  snap::Attrs {
    fg: color_snapshot(style.fg),
    bg: color_snapshot(style.bg),
    mode: bits,
    ul: color_snapshot(style.ul),
    link: links.get(style.link).map(|link| snap::LinkSnapshot {
      uri: link.uri.clone(),
      id: link.id.clone(),
    }),
  }
}

/// The style a snapshot's attrs describe, its link interned into `grid`
/// when it has one.
fn attrs_from_snapshot(attrs: &snap::Attrs, grid: &mut Grid) -> Style {
  let link = match &attrs.link {
    Some(link) => grid.intern_link(Link {
      uri: link.uri.clone(),
      id: link.id.clone(),
    }),
    None => LinkId::NONE,
  };
  style_from_snapshot(attrs).with_link(link)
}

/// The style a snapshot's attrs describe, without its link.
fn style_from_snapshot(attrs: &snap::Attrs) -> Style {
  let mut style = Style::DEFAULT
    .fg(color_from_snapshot(attrs.fg))
    .bg(color_from_snapshot(attrs.bg))
    .ul_color(color_from_snapshot(attrs.ul))
    .from_attrs_bits(attrs.mode);
  // An older binary's underline bit without a shape is a single one.
  if style.underlined() && style.underline() == Underline::None {
    style = style.set_underline(Underline::Single);
  }
  style
}

fn color_snapshot(color: crate::term::Color) -> snap::Color {
  match color {
    crate::term::Color::Default => snap::Color::Default,
    crate::term::Color::Idx(i) => snap::Color::Idx(i),
    crate::term::Color::Rgb(r, g, b) => snap::Color::Rgb(r, g, b),
  }
}

fn color_from_snapshot(color: snap::Color) -> crate::term::Color {
  match color {
    snap::Color::Default => crate::term::Color::Default,
    snap::Color::Idx(i) => crate::term::Color::Idx(i),
    snap::Color::Rgb(r, g, b) => crate::term::Color::Rgb(r, g, b),
  }
}

/// A row: each cell's chars, blanks and continuations as none; wrap
/// fillers are blanks named apart, so they stay padding.
fn row_snapshot(
  row: &Row,
  tables: &Tables,
  table: &mut AttrsTable,
) -> snap::Row {
  let mut text = String::with_capacity(row.cells.len());
  let mut cells: Vec<(u32, u16)> = Vec::new();
  let mut attrs: Vec<(usize, u16)> = Vec::new();
  let mut fillers: Vec<u16> = Vec::new();
  let mut buf = [0u8; 4];
  for (col, cell) in row.cells.iter().enumerate() {
    let cell_text = cell.text.as_str(&mut buf, &tables.graphemes);
    text.push_str(cell_text);
    push_run(&mut cells, cell_text.chars().count() as u32);
    push_run(
      &mut attrs,
      table.index(tables.styles.get(cell.style), &tables.links),
    );
    if cell.text.is_filler() {
      fillers.push(col as u16);
    }
  }
  let visible = |id: StyleId| tables.styles.get(id).is_visible_blank();
  snap::Row {
    text,
    cells,
    attrs,
    wrapped: row.wrapped(),
    size: row.used_len(visible),
    fillers,
  }
}

/// The cells of a snapshot row, in order.
fn cells_from_snapshot(
  row: &snap::Row,
  styles: &[Style],
  grid: &mut Grid,
) -> anyhow::Result<Vec<Cell>> {
  let mut cells = Vec::new();
  let mut rest = row.text.as_str();
  let mut attrs = row
    .attrs
    .iter()
    .flat_map(|(index, len)| std::iter::repeat_n(*index, usize::from(*len)));
  let mut prev_wide = false;
  for &(chars, len) in &row.cells {
    for _ in 0..len {
      let (text, tail) = split_chars(rest, chars as usize)
        .ok_or_else(|| anyhow::anyhow!("row text is shorter than its cells"))?;
      rest = tail;
      let index = attrs
        .next()
        .ok_or_else(|| anyhow::anyhow!("row attrs cover fewer cells"))?;
      let style = *styles
        .get(index)
        .ok_or_else(|| anyhow::anyhow!("row attrs index out of range"))?;
      let style = grid.intern_style(style);
      let cell_text = if text.is_empty() {
        if prev_wide {
          CellText::CONTINUATION
        } else if row.fillers.contains(&(cells.len() as u16)) {
          CellText::FILLER
        } else {
          CellText::EMPTY
        }
      } else {
        let width = unicode::cluster_width(text).max(1);
        grid.cell_text(text, width)
      };
      prev_wide = cell_text.is_wide();
      cells.push(Cell::new(cell_text, style));
    }
  }
  if !rest.is_empty() {
    anyhow::bail!("row text is longer than its cells");
  }
  if attrs.next().is_some() {
    anyhow::bail!("row attrs cover more cells");
  }
  Ok(cells)
}

/// The first `n` chars of `s` and the rest, or None if `s` is shorter.
fn split_chars(s: &str, n: usize) -> Option<(&str, &str)> {
  let mut chars = s.chars();
  let mut end = 0;
  for _ in 0..n {
    end += chars.next()?.len_utf8();
  }
  Some(s.split_at(end))
}

fn placement_snapshot(p: &Placement, image_id: u32) -> snap::PlacementSnapshot {
  snap::PlacementSnapshot {
    image: image_id,
    id: p.id,
    row: p.row,
    col: p.col,
    cols: p.cols,
    rows: p.rows,
    src: (p.src.x, p.src.y, p.src.w, p.src.h),
    offset: (p.offset_x, p.offset_y),
    z: p.z,
    virtual_: p.virtual_,
    cells: p.cells,
  }
}

fn placement_from_snapshot(
  p: &snap::PlacementSnapshot,
  key: ImageKey,
) -> Placement {
  Placement {
    key,
    id: p.id,
    row: p.row,
    col: p.col,
    cols: p.cols,
    rows: p.rows,
    src: SrcRect {
      x: p.src.0,
      y: p.src.1,
      w: p.src.2,
      h: p.src.3,
    },
    offset_x: p.offset.0,
    offset_y: p.offset.1,
    z: p.z,
    virtual_: p.virtual_,
    cells: p.cells,
  }
}

fn image_snapshot(image: &Image) -> snap::ImageSnapshot {
  let mut saved = snap::ImageSnapshot {
    id: image.id,
    number: image.number,
    format: image.data.format.code(),
    zlib: image.data.zlib,
    width: image.data.width,
    height: image.data.height,
    data: snap::to_base64(&image.data.bytes),
    frames: Vec::new(),
    controls: Vec::new(),
    order: Vec::new(),
  };
  let mut interleaved = false;
  for command in &image.commands {
    match command {
      AnimationCommand::Frame(frame) => {
        interleaved |= !saved.controls.is_empty();
        saved.order.push(snap::ImageCommandKind::Frame);
        saved.frames.push(snap::ImageFrameSnapshot {
          control: frame.control.clone(),
          format: frame.data.format.code(),
          zlib: frame.data.zlib,
          width: frame.data.width,
          height: frame.data.height,
          data: snap::to_base64(&frame.data.bytes),
        });
      }
      AnimationCommand::Control(control) => {
        saved.order.push(snap::ImageCommandKind::Control);
        saved.controls.push(control.clone());
      }
    }
  }
  if !interleaved {
    saved.order.clear();
  }
  saved
}

fn image_from_snapshot(saved: &snap::ImageSnapshot) -> anyhow::Result<Image> {
  let data = |format: u32, zlib: bool, width: u32, height: u32, data: &str| {
    Ok::<ImageData, anyhow::Error>(ImageData {
      format: Format::from_code(format)
        .ok_or_else(|| anyhow::anyhow!("unknown image format {format}"))?,
      zlib,
      width,
      height,
      bytes: bytes::Bytes::from(snap::from_base64(data)?),
    })
  };
  let mut frames = saved.frames.iter();
  let mut controls = saved.controls.iter();
  let legacy_order;
  let order = if saved.order.is_empty() {
    legacy_order =
      std::iter::repeat_n(snap::ImageCommandKind::Frame, saved.frames.len())
        .chain(std::iter::repeat_n(
          snap::ImageCommandKind::Control,
          saved.controls.len(),
        ))
        .collect::<Vec<_>>();
    &legacy_order
  } else {
    &saved.order
  };
  let mut commands = Vec::new();
  for kind in order {
    commands.push(match kind {
      snap::ImageCommandKind::Frame => {
        let frame = frames.next().ok_or_else(|| {
          anyhow::anyhow!("image command order has too many frames")
        })?;
        AnimationCommand::Frame(AnimationFrame {
          data: data(
            frame.format,
            frame.zlib,
            frame.width,
            frame.height,
            &frame.data,
          )?,
          control: frame.control.clone(),
        })
      }
      snap::ImageCommandKind::Control => {
        let control = controls.next().ok_or_else(|| {
          anyhow::anyhow!("image command order has too many controls")
        })?;
        AnimationCommand::Control(control.clone())
      }
    });
  }
  anyhow::ensure!(
    frames.next().is_none() && controls.next().is_none(),
    "image command order omits commands"
  );
  let image = Image {
    key: fresh_key(),
    id: saved.id,
    number: saved.number,
    data: data(
      saved.format,
      saved.zlib,
      saved.width,
      saved.height,
      &saved.data,
    )?,
    commands,
  };
  anyhow::ensure!(
    image.bytes() <= MAX_STORE_BYTES,
    "image exceeds store quota"
  );
  Ok(image)
}

fn grid_snapshot(
  grid: &Grid,
  table: &mut AttrsTable,
  cursor_style: CursorStyle,
  images: &ImageStore,
) -> snap::Grid {
  let size = grid.size();
  let pos = grid.pos();
  let saved = grid.saved_pos();
  let (top, bottom) = grid.scroll_region();
  let (left, right) = grid.margins();
  // Scrollback goes first, as rows wrapped at the width: the form every
  // version reads. Clusters they intern go into a copy of the tables.
  let history = grid.history();
  let mut tables = grid.tables().clone();
  let mut rows: Vec<snap::Row> = Vec::new();
  for i in 0..history.len() {
    let Some(line) = history.get(i) else {
      continue;
    };
    let open = history.is_open() && i + 1 == history.len();
    let mut line_rows = line.rows(size.width, &mut tables);
    if open && let Some(last) = line_rows.last_mut() {
      last.wrap(true);
    }
    rows.extend(
      line_rows
        .iter()
        .map(|row| row_snapshot(row, &tables, table)),
    );
  }
  rows.extend(
    grid
      .rows()
      .iter()
      .map(|row| row_snapshot(row, &tables, table)),
  );
  let used_rows = grid.used_rows() as u16;
  snap::Grid {
    width: size.width,
    height: size.height,
    pos: (pos.row, pos.col),
    saved_pos: (saved.row, saved.col),
    scroll_top: top,
    scroll_bottom: bottom,
    rows,
    used_rows: used_rows.min(size.height),
    origin_mode: grid.origin_mode(),
    saved_origin_mode: grid.saved_origin_mode(),
    scrollback_len: grid.scrollback_len(),
    scrollback_offset: grid.scrollback(),
    cursor_pos: grid.cursor_pos.map(|pos| (pos.row, pos.col)),
    cursor_style: cursor_style as u8,
    tabs: (!grid.tabs_are_default()).then(|| grid.tab_stops()),
    margins: (left != 0 || right != size.width - 1).then_some((left, right)),
    // Only placements are anchored to it.
    row_base: if grid.placements().is_empty() {
      0
    } else {
      grid.row_base()
    },
    placements: grid
      .placements()
      .iter()
      .filter_map(|p| {
        images
          .get_by_key(p.key)
          .map(|image| placement_snapshot(p, image.id))
      })
      .collect(),
    placeholder_slots: grid.placeholder_slots().to_vec(),
    image_cells: grid.image_cells(),
  }
}

fn grid_from_snapshot(
  saved: &snap::Grid,
  attrs: &[snap::Attrs],
  images: &ImageStore,
) -> anyhow::Result<Grid> {
  let size = Size {
    width: saved.width.max(1),
    height: saved.height.max(1),
  };
  let mut grid = Grid::new(size, saved.scrollback_len);
  let styles: Vec<Style> = attrs
    .iter()
    .map(|a| attrs_from_snapshot(a, &mut grid))
    .collect();
  let height = usize::from(size.height);
  if saved.rows.len() < height {
    anyhow::bail!("grid has fewer rows than its height");
  }
  // Scrollback rows come before the screen rows.
  let (old_history, screen_rows) =
    saved.rows.split_at(saved.rows.len() - height);
  let mut rows: Vec<Arc<Row>> = Vec::with_capacity(height);
  for row in screen_rows {
    let cells = cells_from_snapshot(row, &styles, &mut grid)?;
    let mut live = Row::from_cells(cells, size.width);
    live.wrap(row.wrapped);
    rows.push(Arc::new(live));
  }
  let mut history_rows: Vec<Row> = Vec::new();
  for row in old_history {
    let cells = cells_from_snapshot(row, &styles, &mut grid)?;
    let mut live = Row::from_cells(cells, size.width);
    live.wrap(row.wrapped);
    history_rows.push(live);
  }
  grid.restore(rows, history_rows);
  let cursor = |(row, col): (u16, u16)| Pos {
    row: row.min(size.height - 1),
    // The text cursor may sit one past the last column: a pending wrap.
    col: col.min(size.width),
  };
  let clamp = |(row, col): (u16, u16)| Pos {
    row: row.min(size.height - 1),
    col: col.min(size.width - 1),
  };
  grid.restore_cursor_state(
    cursor(saved.pos),
    cursor(saved.saved_pos),
    saved.scroll_top.min(size.height - 1),
    saved.scroll_bottom.min(size.height - 1),
    saved.margins,
    saved.origin_mode,
    saved.saved_origin_mode,
  );
  if let Some(tabs) = &saved.tabs {
    grid.set_tab_stops(tabs);
  }
  grid.cursor_pos = saved.cursor_pos.map(clamp);
  grid.cursor_style = CursorStyle::from_u16(u16::from(saved.cursor_style));
  let placements = saved
    .placements
    .iter()
    .filter_map(|p| {
      images
        .get(p.image)
        .map(|image| placement_from_snapshot(p, image.key))
    })
    .collect();
  grid.restore_images(
    saved.row_base,
    placements,
    saved.placeholder_slots.clone(),
    &saved.image_cells,
  );
  grid.set_scrollback(saved.scrollback_offset);
  Ok(grid)
}

impl Screen {
  pub fn snapshot(&self) -> snap::Screen {
    let mut table = AttrsTable::default();
    let grid =
      grid_snapshot(&self.grid, &mut table, self.cursor_style, &self.images);
    let alt_grid = grid_snapshot(
      &self.alt_grid,
      &mut table,
      self.cursor_style,
      &self.images,
    );
    let mut images: Vec<snap::ImageSnapshot> = self
      .images
      .keys()
      .filter_map(|key| self.images.get_by_key(key))
      .map(|image| image_snapshot(&image))
      .collect();
    images.sort_by_key(|image| image.id);
    let no_links = LinkTable::default();
    let m = &self.modes;
    snap::Screen {
      grid,
      alt_grid,
      attrs: attrs_snapshot(self.style, &no_links),
      saved_attrs: attrs_snapshot(self.saved_style, &no_links),
      modes: m.to_bits(),
      mouse_mode: self.mouse_mode.name().to_string(),
      mouse_encoding: self.mouse_encoding.name().to_string(),
      g0: self.charsets[0].name().to_string(),
      g1: self.charsets[1].name().to_string(),
      g2: self.charsets[2].name().to_string(),
      g3: self.charsets[3].name().to_string(),
      shift_out: self.gl == 1,
      // `shift_out` already says G1; the field is for G2 and G3.
      gl: if self.gl >= 2 { self.gl } else { 0 },
      insert: m.insert,
      kitty_flags: self.kitty.entries().to_vec(),
      alt_kitty_flags: self.alt_kitty.entries().to_vec(),
      title: self.title.to_string(),
      title_stack: self.title_stack.iter().map(|t| t.to_string()).collect(),
      cwd: self.cwd.clone(),
      link: self.link.as_ref().map(|link| snap::LinkSnapshot {
        uri: link.uri.clone(),
        id: link.id.clone(),
      }),
      cell_pixels: (self.cell_pixels != super::DEFAULT_CELL_PIXELS)
        .then_some(self.cell_pixels),
      dynamic_colors: {
        let rgb = |c: Option<Rgb>| c.map(|Rgb(r, g, b)| (r, g, b));
        let colors = [
          rgb(self.dynamic_fg),
          rgb(self.dynamic_bg),
          rgb(self.dynamic_cursor),
        ];
        colors.iter().any(Option::is_some).then_some(colors)
      },
      images,
      pending_input: snap::to_base64(&self.scanner.pending()),
      attrs_table: table.entries,
    }
  }

  pub fn from_snapshot(saved: &snap::Screen) -> anyhow::Result<Self> {
    let mut images = ImageStore::default();
    for image in &saved.images {
      images.insert_restored(image_from_snapshot(image)?);
    }
    let grid = grid_from_snapshot(&saved.grid, &saved.attrs_table, &images)?;
    let alt_grid =
      grid_from_snapshot(&saved.alt_grid, &saved.attrs_table, &images)?;
    let mut modes = Modes::from_bits(saved.modes);
    modes.insert = saved.insert;
    // A synchronized update does not survive a restart.
    modes.sync = false;
    // The active link is kept apart from the style and interned per grid
    // as cells are written.
    let style = style_from_snapshot(&saved.attrs);
    let saved_style = style_from_snapshot(&saved.saved_attrs);
    let active_cursor_style = if modes.alt_screen {
      saved.alt_grid.cursor_style
    } else {
      saved.grid.cursor_style
    };
    let mut restored = Screen {
      scanner: Default::default(),
      grid,
      alt_grid,
      style,
      saved_style,
      link: saved.link.as_ref().map(|link| Link {
        uri: link.uri.clone(),
        id: link.id.clone(),
      }),
      link_ids: [None, None],
      sgr_stack: Vec::new(),
      modes,
      mouse_mode: MouseProtocolMode::from_name(&saved.mouse_mode),
      mouse_encoding: MouseProtocolEncoding::from_name(&saved.mouse_encoding),
      cursor_style: CursorStyle::from_u16(u16::from(active_cursor_style)),
      charsets: [
        CharSet::from_name(&saved.g0),
        CharSet::from_name(&saved.g1),
        CharSet::from_name(&saved.g2),
        CharSet::from_name(&saved.g3),
      ],
      gl: if saved.gl != 0 {
        saved.gl
      } else {
        u8::from(saved.shift_out)
      },
      single_shift: None,
      kitty: KittyStack::from_entries(&saved.kitty_flags),
      alt_kitty: KittyStack::from_entries(&saved.alt_kitty_flags),
      title: Arc::from(saved.title.as_str()),
      title_stack: saved
        .title_stack
        .iter()
        .map(|t| Arc::from(t.as_str()))
        .collect(),
      cwd: saved.cwd.clone(),
      palette: crate::term::Palette::dark(),
      dynamic_fg: dynamic_color(saved, 0),
      dynamic_bg: dynamic_color(saved, 1),
      dynamic_cursor: dynamic_color(saved, 2),
      shown_styles: Default::default(),
      cell_pixels: saved.cell_pixels.unwrap_or(super::DEFAULT_CELL_PIXELS),
      last_char: None,
      placeholder_marks: 0,
      images,
      pending_image: None,
      events: Vec::new(),
    };
    // Lines above the cursor's were logged before the restart.
    restored.grid.enable_log();
    // An unfinished sequence cannot complete or emit anything on its own,
    // so replaying it only rebuilds the parser state.
    let pending = snap::from_base64(&saved.pending_input)?;
    restored.process(&pending);
    restored.events.clear();
    Ok(restored)
  }
}
