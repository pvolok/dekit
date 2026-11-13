use unicode_width::UnicodeWidthStr;

use crate::console::action::Action;
use crate::console::keymap::{Keymap, KeymapGroup, keys_label};
use crate::console::theme::Theme;
use crate::term::{Grid, grid::Rect, style::Style};

pub fn render_zoom_tip(
  area: Rect,
  grid: &mut Grid,
  keymap: &Keymap,
  theme: &Theme,
) {
  if area.height == 0 {
    return;
  }

  let keys = [Action::ToggleFocus, Action::FocusTerm, Action::FocusTasks]
    .iter()
    .find_map(|action| keymap.keys(KeymapGroup::Term, action));

  let bg = theme.block.selected_bg.into();
  let text = Style::default().bg(bg).fg(theme.block.text.into());
  grid.fill_area(area, ' ', text);
  let mut line = area;
  match keys {
    Some(keys) => {
      let prompt = " To exit zoom mode press ";
      grid.draw_text(line.take_left(prompt.width() as u16), prompt, text);
      let key_attrs = Style::default().bg(bg).fg(theme.block.key.into());
      grid.draw_text(line, &keys_label(keys), key_attrs);
    }
    None => {
      grid.draw_text(line, " No key bound to exit the zoom mode", text);
    }
  }
}
