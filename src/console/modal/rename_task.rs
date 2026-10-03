use tui_input::Input;

use crate::console::action::Action;
use crate::console::{
  keymap::Keymap,
  widgets::text_input::{render_text_input, to_input_request},
};
use crate::term::{
  Grid,
  attrs::Attrs,
  grid::{BorderType, Rect},
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

  fn render(&mut self, grid: &mut Grid, _keymap: &Keymap) {
    let area = self.area(grid.area());
    let inner = grid
      .block(area, BorderType::Thick)
      .title("Rename task", Attrs::default())
      .inner();
    grid.cursor_pos = Some(render_text_input(&self.input, inner, grid));
  }
}
