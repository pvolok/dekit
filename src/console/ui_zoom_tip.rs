use crate::console::action::Action;
use crate::console::keymap::{Keymap, KeymapGroup};
use crate::term::{Color, Grid, attrs::Attrs, grid::Rect};

pub fn render_zoom_tip(area: Rect, grid: &mut Grid, keymap: &Keymap) {
  if area.height == 0 {
    return;
  }

  let events = vec![Action::FocusTerm, Action::ToggleFocus, Action::FocusTasks];
  let key = events
    .into_iter()
    .find_map(|event| keymap.resolve_key(KeymapGroup::Term, &event));

  let attrs = Attrs::default().fg(Color::BLACK).bg(Color::YELLOW);
  let r = if let Some(key) = key {
    grid.draw_text(
      area,
      format!(" To exit zoom mode press {}", key.spec()).as_str(),
      attrs,
    )
  } else {
    grid.draw_text(area, " No key bound to exit the zoom mode", attrs)
  };

  grid.fill_area(area.inner((0, 0, 0, r.width)), ' ', attrs);
}
