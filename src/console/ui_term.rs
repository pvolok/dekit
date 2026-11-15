use std::collections::HashMap;

use unicode_width::UnicodeWidthStr;

use crate::console::{
  state::State,
  task_tree::{Group, Node},
  theme::{BlockStyle, Theme},
};
use crate::kernel::task_path::TaskPath;
use crate::term::{
  Frame, Grid, Palette,
  cell::{CellText, TextKind},
  grid::{Pos, Rect},
  style::{LinkId, Style, StyleId},
};

/// `shown` is the selected task's screen, once it is attached.
pub fn render_term(
  area: Rect,
  grid: &mut Grid,
  state: &State,
  shown: Option<&Frame>,
  theme: &Theme,
) {
  if area.width < 3 || area.height < 3 {
    return;
  }

  let active = state.scope.is_term();
  let style = theme.block.panel(active);

  match state.tasks.selected() {
    Some((_, Node::Task(_))) => (),
    Some((path, Node::Group(group))) => {
      return render_group(area, grid, &style, theme, path, group);
    }
    None => {
      style.draw(grid, area);
      return;
    }
  };

  let Some(frame) = shown else {
    style.draw(grid, area).title(" Terminal ", style.title);
    return;
  };

  let mut block = style.draw(grid, area);
  block.title(" Terminal ", style.title);
  let title = frame.title.as_ref();
  if !title.is_empty() {
    block.title(title, style.dim).title(" ", style.dim);
  }

  let inner = block.inner();
  render_frame(frame, inner, grid, &theme.palette);

  if active
    && let Some(cursor) = frame.cursor
    && cursor.col < inner.width
    && cursor.row < inner.height
  {
    grid.cursor_pos = Some(Pos {
      col: inner.x + cursor.col,
      row: inner.y + cursor.row,
    });
    grid.cursor_style = frame.cursor_style;
  }
}

/// In place of a terminal, what a group row stands for.
fn render_group(
  area: Rect,
  grid: &mut Grid,
  style: &BlockStyle,
  theme: &Theme,
  path: &TaskPath,
  group: &Group,
) {
  let inner = style
    .draw(grid, area)
    .title(&format!(" {} ", path.as_str()), style.title)
    .inner()
    .inner((0, 1));
  let mut lines = inner.rows();
  if let Some(mut line) = lines.next() {
    let noun = if group.len() == 1 { "task" } else { "tasks" };
    let summary = format!("{} {}, {} up", group.len(), noun, group.up);
    let area = line.take_left(summary.width() as u16);
    grid.draw_text(area, &summary, style.text);
    if group.failed > 0 {
      let failed = format!(", {} failed", group.failed);
      let attrs = Style {
        fg: theme.tasks.failed.into(),
        ..style.text
      };
      grid.draw_text(line, &failed, attrs);
    }
  }
  if let Some(line) = lines.nth(1) {
    grid.draw_text(
      line,
      "Start, stop, and restart act on every task in the group.",
      style.dim,
    );
  }
}

/// Copies a frame into `area` of the grid, the frame's default colors
/// resolved through `palette` so the console looks the same whatever
/// the terminal's own colors are. Cells are carried over whole: text,
/// style, and link.
pub fn render_frame(
  frame: &Frame,
  area: Rect,
  grid: &mut Grid,
  palette: &Palette,
) {
  // Styles, links and clusters are interned per grid: each of the
  // frame's is mapped once. Scalars, blanks and continuations carry over
  // as they are.
  let mut styles: Vec<Option<StyleId>> = vec![None; frame.tables.styles.len()];
  let mut links: HashMap<LinkId, LinkId> = HashMap::new();
  let mut graphemes: HashMap<u32, CellText> = HashMap::new();
  let rows = area.height.min(frame.size.height);
  let cols = area.width.min(frame.size.width);
  for row in 0..rows {
    let Some(src) = frame.row(row) else {
      break;
    };
    for col in 0..cols {
      let Some(cell) = src.get(col) else {
        break;
      };
      if cell.is_continuation() {
        continue;
      }
      let width = cell.width().max(1);
      let pos = Pos {
        col: area.x + col,
        row: area.y + row,
      };
      if col + width > cols {
        grid.write_cell(pos, CellText::EMPTY, StyleId::DEFAULT);
        continue;
      }
      let style = match styles.get(cell.style.0 as usize).copied().flatten() {
        Some(id) => id,
        None => {
          let source = frame.style(cell);
          let link = if source.link.is_none() {
            LinkId::NONE
          } else {
            *links.entry(source.link).or_insert_with(|| {
              frame
                .link(&source)
                .map_or(LinkId::NONE, |link| grid.intern_link(link.clone()))
            })
          };
          let id = grid.intern_style(Style {
            fg: palette.fg(source.fg).into(),
            bg: palette.bg(source.bg).into(),
            link,
            ..source
          });
          if let Some(slot) = styles.get_mut(cell.style.0 as usize) {
            *slot = Some(id);
          }
          id
        }
      };
      let text = match cell.text.kind() {
        TextKind::Grapheme(index) => {
          *graphemes.entry(index).or_insert_with(|| {
            grid.cell_text(frame.tables.graphemes.get(index), width as u8)
          })
        }
        // Placeholder cells belong to the frame's own image slots; its
        // placements come along below.
        TextKind::Image { .. } => CellText::EMPTY,
        _ => cell.text,
      };
      grid.write_cell(pos, text, style);
    }
  }
  // Images come along, cut to the area.
  let pane = Rect::new(area.x, area.y, cols, rows);
  for placement in &frame.placements {
    let Some(shown) = placement.clipped(pane, area.x, area.y) else {
      continue;
    };
    if let Some(image) = frame.images.get(&placement.key) {
      grid.add_image(image.clone());
      grid.put_placement(shown);
    }
  }
}
