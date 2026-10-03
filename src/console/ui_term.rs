use unicode_width::UnicodeWidthStr;

use crate::console::{
  state::State,
  task_tree::{Group, Node},
};
use crate::kernel::task_path::TaskPath;
use crate::term::{
  Color, Grid, Screen,
  attrs::Attrs,
  grid::{BorderType, Pos, Rect},
};

pub fn render_term(area: Rect, grid: &mut Grid, state: &State) {
  if area.width < 3 || area.height < 3 {
    return;
  }

  let active = state.scope.is_term();
  let border = if active {
    BorderType::Thick
  } else {
    BorderType::Plain
  };

  let task = match state.tasks.selected() {
    Some((_, Node::Task(task))) => task,
    Some((path, Node::Group(group))) => {
      return render_group(area, grid, border, active, path, group);
    }
    None => return,
  };

  let handle = task.present.as_ref().unwrap_or(&task.vt);
  let Ok(screen) = handle.read() else {
    return;
  };
  let screen = &*screen;

  let mut block = grid.block(area, border);
  block.title("Terminal", Attrs::default().set_bold(active));
  let title = screen.title();
  if !title.is_empty() {
    block
      .title(" ", Attrs::default())
      .title(title, Attrs::default().fg(Color::BRIGHT_BLACK));
  }

  let inner = block.inner();
  render_screen(screen, inner, grid);

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
  border: BorderType,
  active: bool,
  path: &TaskPath,
  group: &Group,
) {
  let inner = grid
    .block(area, border)
    .title(path.as_str(), Attrs::default().set_bold(active))
    .inner();
  let mut lines = inner.rows();
  if let Some(mut line) = lines.next() {
    let noun = if group.len() == 1 { "task" } else { "tasks" };
    let summary = format!("{} {}, {} up", group.len(), noun, group.up);
    let area = line.take_left(summary.width() as u16);
    grid.draw_text(area, &summary, Attrs::default());
    if group.failed > 0 {
      let failed = format!(", {} failed", group.failed);
      grid.draw_text(line, &failed, Attrs::default().fg(Color::BRIGHT_RED));
    }
  }
  if let Some(line) = lines.nth(1) {
    grid.draw_text(
      line,
      "Start, stop, and restart act on every task in the group.",
      Attrs::default().fg(Color::BRIGHT_BLACK),
    );
  }
}

fn render_screen(screen: &Screen, area: Rect, grid: &mut Grid) {
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
      }
    }
  }
}
