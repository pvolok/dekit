use tui_input::Input;

use crate::console::action::Action;
use crate::console::{
  keymap::Keymap,
  theme::Theme,
  widgets::text_input::{render_text_input, to_input_request},
};
use crate::term::{
  Grid,
  grid::Rect,
  key::{Key, KeyCode},
};

use super::modal::{Modal, ModalResult};

#[derive(Default)]
pub struct RenameTaskModal {
  input: Input,
}

impl Modal for RenameTaskModal {
  fn handle_key(&mut self, key: &Key) -> ModalResult {
    match key.code {
      KeyCode::Enter if key.mods.is_empty() => {
        return ModalResult::Run(Action::RenameTask {
          name: self.input.value().to_string(),
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

  fn size(&self, _frame: Rect) -> (u16, u16) {
    (42, 3)
  }

  fn render(&mut self, grid: &mut Grid, _keymap: &Keymap, theme: &Theme) {
    let area = self.area(grid.area());
    let style = theme.block.modal();
    let inner = style
      .draw(grid, area)
      .title(" Rename task ", style.title)
      .inner()
      .inner((0, 1));
    grid.cursor_pos =
      Some(render_text_input(&self.input, inner, grid, style.text));
  }
}
