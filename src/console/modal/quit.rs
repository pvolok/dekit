use crate::console::action::Action;
use crate::console::keymap::Keymap;
use crate::term::{
  Grid,
  attrs::Attrs,
  grid::{BorderType, Rect},
  key::{Key, KeyCode},
};

use super::modal::{Modal, ModalResult};

pub struct QuitModal;

impl Modal for QuitModal {
  fn handle_key(&mut self, key: &Key) -> ModalResult {
    if !key.mods.is_empty() {
      return ModalResult::Keep;
    }
    match key.code {
      KeyCode::Char('e') => ModalResult::Run(Action::Quit),
      KeyCode::Char('d') => ModalResult::Detach,
      KeyCode::Char('n') | KeyCode::Esc => ModalResult::Close,
      _ => ModalResult::Keep,
    }
  }

  fn size(&self, _frame: Rect) -> (u16, u16) {
    (36, 5)
  }

  fn render(&mut self, grid: &mut Grid, _keymap: &Keymap) {
    let area = self.area(grid.area());
    let inner = grid.block(area, BorderType::Thick).inner();
    grid.fill_area(inner, ' ', Attrs::default());
    let lines = [
      "<e>   - stop the runner",
      "<d>   - detach client",
      "<Esc> - cancel",
    ];
    for (row, line) in inner.rows().zip(lines) {
      grid.draw_text(row, line, Attrs::default());
    }
  }
}
