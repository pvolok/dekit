use unicode_width::UnicodeWidthStr;

use crate::console::action::Action;
use crate::console::keymap::{Keymap, KeymapGroup};
use crate::console::theme::Theme;
use crate::term::{Grid, attrs::Attrs, grid::Rect};

pub fn render_zoom_tip(
  area: Rect,
  grid: &mut Grid,
  keymap: &Keymap,
  theme: &Theme,
) {
  if area.height == 0 {
    return;
  }

  let key = [Action::FocusTerm, Action::ToggleFocus, Action::FocusTasks]
    .iter()
    .find_map(|action| keymap.key(KeymapGroup::Term, action));

  let bg = theme.block.selected_bg.into();
  let text = Attrs::default().bg(bg).fg(theme.block.text.into());
  grid.fill_area(area, ' ', text);
  let mut line = area;
  match key {
    Some(key) => {
      let prompt = " To exit zoom mode press ";
      grid.draw_text(line.take_left(prompt.width() as u16), prompt, text);
      let key_attrs = Attrs::default().bg(bg).fg(theme.block.key.into());
      grid.draw_text(line, &key.spec().to_string(), key_attrs);
    }
    None => {
      grid.draw_text(line, " No key bound to exit the zoom mode", text);
    }
  }
}
