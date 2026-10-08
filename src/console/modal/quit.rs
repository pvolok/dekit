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
      KeyCode::Char('q') => ModalResult::Run(Action::Down),
      KeyCode::Char('x') => ModalResult::Run(Action::Quit),
      KeyCode::Char('d') => ModalResult::Detach,
      KeyCode::Esc => ModalResult::Close,
      _ => ModalResult::Keep,
    }
  }

  fn size(&self, _frame: Rect) -> (u16, u16) {
    (45, 6)
  }

  fn render(&mut self, grid: &mut Grid, _keymap: &Keymap) {
    let area = self.area(grid.area());
    let inner = grid
      .block(area, BorderType::Thick)
      .title("Quit", Attrs::default())
      .inner();
    grid.fill_area(inner, ' ', Attrs::default());
    let lines = [
      "<q>   - stop all, save for the next start",
      "<x>   - stop all, don't save",
      "<d>   - detach, leave everything running",
      "<Esc> - cancel",
    ];
    for (row, line) in inner.rows().zip(lines) {
      grid.draw_text(row, line, Attrs::default());
    }
  }
}
