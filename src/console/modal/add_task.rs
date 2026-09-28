use tui_input::Input;
use unicode_width::UnicodeWidthStr;

use crate::console::action::Action;
use crate::console::{
  keymap::Keymap,
  widgets::text_input::{render_text_input, to_input_request},
};
use crate::help::layout::{Span, wrap};
use crate::term::{
  Color, Grid,
  attrs::Attrs,
  grid::{BorderType, Rect},
  key::{Key, KeyCode},
};

use super::modal::{Modal, ModalResult};

#[derive(Default)]
pub struct AddTaskModal {
  input: Input,
  /// Why the last line was not added.
  error: Option<String>,
}

impl AddTaskModal {
  pub fn with_error(cmd: String, error: String) -> Self {
    Self {
      input: Input::new(cmd),
      error: Some(error),
    }
  }

  fn error_lines(&self, width: u16) -> Vec<String> {
    let Some(error) = &self.error else {
      return Vec::new();
    };
    let text = vec![Span {
      text: error.clone(),
      attrs: Attrs::default(),
    }];
    wrap(text, width.into(), Vec::new(), Vec::new())
      .into_iter()
      .map(|line| line.into_iter().map(|span| span.text).collect())
      .collect()
  }
}

impl Modal for AddTaskModal {
  fn handle_key(&mut self, key: &Key) -> ModalResult {
    match key.code {
      KeyCode::Enter if key.mods.is_empty() => {
        return ModalResult::Run(Action::AddTask {
          cmd: self.input.value().to_string(),
          name: None,
        });
      }
      KeyCode::Esc if key.mods.is_empty() => return ModalResult::Close,
      _ => (),
    }
    if let Some(req) = to_input_request(key) {
      self.input.handle(req);
    }
    ModalResult::Keep
  }

  fn size(&self, frame: Rect) -> (u16, u16) {
    match &self.error {
      // Wide enough for the error, up to 80 columns; it wraps below the
      // input.
      Some(error) => {
        let width = ((error.width() + 2).clamp(42, 80) as u16).min(frame.width);
        let lines = self.error_lines(width.saturating_sub(2)).len() as u16;
        (width, 3 + lines)
      }
      None => (42, 3),
    }
  }

  fn render(&mut self, grid: &mut Grid, _keymap: &Keymap) {
    let area = self.area(grid.area());
    grid.draw_block(area, &BorderType::Plain.chars(), Attrs::default());
    grid.draw_text(
      Rect::new(area.x + 1, area.y, area.width.saturating_sub(2), 1),
      "Add task",
      Attrs::default(),
    );
    let inner = area.inner(1);
    grid.fill_area(inner, ' ', Attrs::default());
    let (input, error) = inner.split_h(1);
    for (i, line) in self.error_lines(inner.width).iter().enumerate() {
      if let Some(row) = error.row(i as u16) {
        grid.draw_text(row, line, Attrs::default().fg(Color::BRIGHT_RED));
      }
    }
    grid.cursor_pos = Some(render_text_input(&self.input, input, grid));
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::term::Size;

  #[test]
  fn error_rows_are_cleared_and_wrapped() {
    let mut grid = Grid::new(
      Size {
        width: 100,
        height: 12,
      },
      0,
    );
    grid.fill_area(grid.area(), '#', Attrs::default());
    let error = crate::parse_shell::split_argv("a && b")
      .unwrap_err()
      .to_string();
    let mut modal = AddTaskModal::with_error("a && b".into(), error.clone());
    modal.render(&mut grid, &Keymap::new());

    let inner = modal.area(grid.area()).inner(1);
    let rows: Vec<String> = (inner.y..inner.bottom())
      .map(|y| {
        let (x, y) = (inner.x as i32, y as i32);
        grid.get_selected_text(x, y, x + inner.width as i32 - 1, y)
      })
      .collect();
    assert!(rows.iter().all(|row| !row.contains('#')), "{rows:#?}");
    assert_eq!(rows[0].trim_end(), "a && b");
    let shown: Vec<&str> = rows[1..].iter().map(|row| row.trim_end()).collect();
    assert_eq!(shown.join(" "), error);
  }
}
