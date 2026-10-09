use unicode_width::UnicodeWidthStr;

use crate::console::action::Action;
use crate::console::{
  keymap::{Keymap, KeymapGroup},
  state::State,
  theme::Theme,
};
use crate::term::{Grid, grid::Rect};

pub fn render_keymap(
  area: Rect,
  grid: &mut Grid,
  state: &State,
  keymap: &Keymap,
  theme: &Theme,
) {
  if area.width <= 3 || area.height < 3 {
    return;
  }

  let style = theme.block.panel(false);
  let mut line = style.draw(grid, area).title(" Help ", style.title).inner();

  let group = state.keymap_group();
  let items: &[Action] = match group {
    KeymapGroup::Tasks => &[
      Action::ToggleFocus,
      Action::ShowQuit,
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

  for action in items {
    let Some(key) = keymap.key(group, action) else {
      continue;
    };
    for (text, attrs) in [
      (" <".to_string(), style.dim),
      (key.to_string(), style.key),
      (": ".to_string(), style.dim),
      (action.desc(), style.text),
      ("> ".to_string(), style.dim),
    ] {
      grid.draw_text(line.take_left(text.width() as u16), &text, attrs);
    }
  }
}
