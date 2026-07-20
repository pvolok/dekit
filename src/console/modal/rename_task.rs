use tui_input::Input;

use crate::console::action::Action;
use crate::console::{
  app::LoopAction,
  state::State,
  widgets::text_input::{render_text_input, to_input_request},
};
use crate::kernel::kernel_message::TaskContext;
use crate::term::{
  Grid, TermEvent,
  attrs::Attrs,
  grid::{BorderType, Pos, Rect},
  key::{Key, KeyCode},
};

use super::modal::Modal;

pub struct RenameTaskModal {
  pc: TaskContext,
  input: Input,
}

impl RenameTaskModal {
  pub fn new(pc: TaskContext) -> Self {
    RenameTaskModal {
      pc,
      input: Input::default(),
    }
  }
}

impl Modal for RenameTaskModal {
  fn handle_input(
    &mut self,
    _state: &mut State,
    loop_action: &mut LoopAction,
    event: &TermEvent,
  ) -> bool {
    match event {
      TermEvent::Key(Key {
        code: KeyCode::Enter,
        mods,
        ..
      }) if mods.is_empty() => {
        self.pc.send_self_custom(Action::CloseCurrentModal);
        self.pc.send_self_custom(Action::RenameTask {
          name: self.input.value().to_string(),
        });
        // Skip because RenameTask event will immediately rerender.
        return true;
      }
      TermEvent::Key(Key {
        code: KeyCode::Esc,
        mods,
        ..
      }) if mods.is_empty() => {
        self.pc.send_self_custom(Action::CloseCurrentModal);
        loop_action.render();
        return true;
      }
      _ => (),
    }

    let req = to_input_request(event);
    if let Some(req) = req {
      self.input.handle(req);
      loop_action.render();
      return true;
    }

    match event {
      TermEvent::FocusGained => false,
      TermEvent::FocusLost => false,
      // Block keys
      TermEvent::Key(_) => true,
      // Block mouse
      TermEvent::Mouse(_) => true,
      // Block paste
      TermEvent::Paste(_) => true,
      TermEvent::Resize(_, _) => false,
    }
  }

  fn get_size(&mut self, _: Rect) -> (u16, u16) {
    (42, 3)
  }

  fn render(&mut self, grid: &mut Grid) {
    let area = self.area(grid.area());

    grid.draw_block(area, &BorderType::Thick.chars(), Attrs::default());
    grid.draw_text(
      Rect {
        x: area.x + 1,
        y: area.y,
        width: area.width.saturating_sub(2),
        height: 1,
      },
      "Rename task",
      Attrs::default(),
    );

    let inner = area.inner(1);

    let mut cursor = (0u16, 0u16);
    render_text_input(&mut self.input, inner, grid, &mut cursor);

    grid.cursor_pos = Some(Pos {
      col: cursor.0,
      row: cursor.1,
    });
  }
}
