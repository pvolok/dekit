use unicode_width::UnicodeWidthStr;

use crate::console::action::{Action, RunnerTab};
use crate::console::{
  keymap::{Keymap, KeymapGroup, keys_label},
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
  let mut hint = |key: String, desc: String| {
    for (text, attrs) in [
      (" <".to_string(), style.dim),
      (key, style.key),
      (": ".to_string(), style.dim),
      (desc, style.text),
      ("> ".to_string(), style.dim),
    ] {
      grid.draw_text(line.take_left(text.width() as u16), &text, attrs);
    }
  };

  let group = state.keymap_group();
  let pending = &state.pending_keys;
  if !pending.is_empty() {
    // Half of a binding is typed: the keys that can follow.
    for (key, action) in keymap.next_keys(group, pending) {
      let desc = action.map_or_else(|| "...".to_string(), Action::desc);
      hint(key.to_string(), desc);
    }
    return;
  }
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
      Action::ShowRunnerSelect {
        tab: RunnerTab::Switch,
      },
      Action::ToggleKeymapWindow,
    ],
    KeymapGroup::Term | KeymapGroup::Global => &[Action::ToggleFocus],
    KeymapGroup::Copy => &[
      Action::CopyModeEnd,
      Action::CopyModeCopy,
      Action::CopyModeLeave,
    ],
  };

  for action in items {
    if let Some(keys) = keymap.keys(group, action) {
      hint(keys_label(keys), action.desc());
    }
  }
  // And the keys that more keys follow, the leader for one.
  for key in keymap.prefix_keys(group) {
    hint(key.to_string(), "...".to_string());
  }
}
