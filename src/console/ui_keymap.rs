use crate::console::action::Action;
use crate::console::{
  keymap::{Keymap, KeymapGroup},
  state::State,
};
use crate::term::{
  Color, Grid,
  attrs::Attrs,
  grid::{BorderType, Rect},
};

pub fn render_keymap(
  area: Rect,
  grid: &mut Grid,
  state: &State,
  keymap: &Keymap,
) {
  if area.width <= 3 || area.height < 3 {
    return;
  }

  grid.draw_block(area, &BorderType::Plain.chars(), Attrs::default());
  grid.draw_text(
    Rect::new(area.x + 1, area.y, area.width - 2, 1),
    "Help",
    Attrs::default(),
  );

  let group = state.keymap_group();
  let items: &[Action] = match group {
    KeymapGroup::Tasks => &[
      Action::ToggleFocus,
      Action::Detach,
      Action::NextTask,
      Action::PrevTask,
      Action::StartTask,
      Action::StopTask,
      Action::RestartTask,
      Action::Zoom,
      Action::ShowCommandsMenu,
      Action::ToggleKeymapWindow,
    ],
    KeymapGroup::Term => &[Action::ToggleFocus],
    KeymapGroup::Copy => &[
      Action::CopyModeEnd,
      Action::CopyModeCopy,
      Action::CopyModeLeave,
    ],
  };

  let mut line = area.inner(1);
  let plain = Attrs::default();
  let yellow = Attrs::default().fg(Color::YELLOW);
  for action in items {
    let Some(key) = keymap.key(group, action) else {
      continue;
    };
    for (text, attrs) in [
      (" <".to_string(), plain),
      (key.to_string(), yellow),
      (format!(": {}> ", action.desc()), plain),
    ] {
      line = line.move_left(grid.draw_text(line, &text, attrs).width as i32);
    }
  }
}
