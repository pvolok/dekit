use crate::console::state::{Scope, State};
use crate::term::{Color, Grid, Screen, attrs::Attrs, grid::Rect};

pub fn render_term(
  area: Rect,
  grid: &mut Grid,
  state: &mut State,
  frame: bool,
) {
  if area.width == 0 || area.height == 0 {
    return;
  }

  if frame && (area.width < 3 || area.height < 3) {
    return;
  }

  let active = match state.scope {
    Scope::Procs => false,
    Scope::Term | Scope::TermZoom => true,
  };

  let Some(proc) = state.get_current_proc() else {
    return;
  };

  if frame {
    let chars = match active {
      true => crate::term::grid::BorderType::Thick,
      false => crate::term::grid::BorderType::Plain,
    }
    .chars();
    grid.draw_block(area, &chars, Attrs::default());
  }

  let handle = proc.present.as_ref().unwrap_or(&proc.vt);
  let Ok(parser) = handle.read() else {
    return;
  };
  let screen = parser.screen();

  if frame {
    let mut top_line = Rect {
      x: area.x + 1,
      y: area.y,
      width: area.width - 2,
      height: 1,
    };
    let r =
      grid.draw_text(top_line, "Terminal", Attrs::default().set_bold(active));
    top_line = top_line.move_left(r.width as i32);
    let title = screen.title();
    if !title.is_empty() {
      let r = grid.draw_text(top_line, " ", Attrs::default());
      top_line = top_line.move_left(r.width as i32);
      let _r = grid.draw_text(
        top_line,
        title,
        Attrs::default().fg(Color::BRIGHT_BLACK),
      );
    }
  }

  let inner = if frame { area.inner(1) } else { area };
  render_screen(screen, inner, grid);

  if active && !screen.hide_cursor() {
    let (row, col) = screen.cursor_position();
    grid.cursor_pos = Some(crate::term::grid::Pos {
      col: inner.x + col,
      row: inner.y + row,
    });
    grid.cursor_style = screen.cursor_style();
  }
}

fn render_screen(screen: &Screen, area: Rect, grid: &mut Grid) {
  for row in 0..area.height {
    for col in 0..area.width {
      let to_cell = if let Some(cell) =
        grid.drawing_cell_mut(crate::term::grid::Pos {
          col: area.x + col,
          row: area.y + row,
        }) {
        cell
      } else {
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

pub fn term_check_hit(area: Rect, x: u16, y: u16) -> bool {
  area.x <= x
    && area.x + area.width >= x + 1
    && area.y <= y
    && area.y + area.height >= y + 1
}
