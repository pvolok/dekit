use crate::console::action::Action;
use crate::console::keymap::{Keymap, KeymapGroup};
use crate::term::{Color, Grid, attrs::Attrs, grid::Rect};

pub fn render_zoom_tip(area: Rect, grid: &mut Grid, keymap: &Keymap) {
  if area.height == 0 {
    return;
  }

  let key = [Action::FocusTerm, Action::ToggleFocus, Action::FocusTasks]
    .iter()
    .find_map(|action| keymap.key(KeymapGroup::Term, action));

  let text = match key {
    Some(key) => format!(" To exit zoom mode press {}", key.spec()),
    None => " No key bound to exit the zoom mode".to_string(),
  };
  let attrs = Attrs::default().fg(Color::BLACK).bg(Color::YELLOW);
  grid.draw_line(area, &text, attrs);
}
