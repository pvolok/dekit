use unicode_width::UnicodeWidthStr;

use crate::console::{
  state::State,
  task_tree::{Group, Node},
  theme::{BlockStyle, Theme},
};
use crate::kernel::task_path::TaskPath;
use crate::term::{
  Grid, Palette, Screen,
  attrs::Attrs,
  grid::{Pos, Rect},
};

pub fn render_term(area: Rect, grid: &mut Grid, state: &State, theme: &Theme) {
  if area.width < 3 || area.height < 3 {
    return;
  }

  let active = state.scope.is_term();
  let style = theme.block.panel(active);

  let task = match state.tasks.selected() {
    Some((_, Node::Task(task))) => task,
    Some((path, Node::Group(group))) => {
      return render_group(area, grid, &style, theme, path, group);
    }
    None => {
      style.draw(grid, area);
      return;
    }
  };

  let handle = task.present.as_ref().unwrap_or(&task.vt);
  let Ok(screen) = handle.read() else {
    return;
  };
  let screen = &*screen;

  let mut block = style.draw(grid, area);
  block.title(" Terminal ", style.title);
  let title = screen.title();
  if !title.is_empty() {
    block.title(title, style.dim).title(" ", style.dim);
  }

  let inner = block.inner();
  render_screen(screen, inner, grid, &theme.palette);

  if active && !screen.hide_cursor() {
    let (row, col) = screen.cursor_position();
    grid.cursor_pos = Some(Pos {
      col: inner.x + col,
      row: inner.y + row,
    });
    grid.cursor_style = screen.cursor_style();
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
      let attrs = Attrs {
        fgcolor: theme.tasks.failed.into(),
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

fn render_screen(
  screen: &Screen,
  area: Rect,
  grid: &mut Grid,
  palette: &Palette,
) {
  for row in 0..area.height {
    for col in 0..area.width {
      let Some(to_cell) = grid.drawing_cell_mut(Pos {
        col: area.x + col,
        row: area.y + row,
      }) else {
        continue;
      };
      if let Some(cell) = screen.cell(row, col) {
        *to_cell = cell.clone();
        if !cell.has_contents() {
          to_cell.set_str(" ");
        }
        let attrs = cell.attrs();
        to_cell.set_attrs(Attrs {
          fgcolor: palette.fg(attrs.fgcolor).into(),
          bgcolor: palette.bg(attrs.bgcolor).into(),
          ..*attrs
        });
      }
    }
  }
}
