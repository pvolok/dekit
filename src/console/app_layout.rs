use crate::{config::config::Config, term::grid::Rect};

pub struct AppLayout {
  pub sidebar: Rect,
  pub term: Rect,
  pub keymap: Rect,
  pub zoom_banner: Rect,
}

impl AppLayout {
  pub fn new(
    area: Rect,
    zoom: bool,
    hide_keymap_window: bool,
    config: &Config,
  ) -> Self {
    let keymap_h = if zoom || hide_keymap_window { 0 } else { 3 };
    let sidebar_w = if zoom {
      0
    } else {
      config.tui.sidebar.width as u16
    };
    let zoom_banner_h = if zoom && config.tui.zoom_tip { 1 } else { 0 };
    let (top, keymap) = area.split_h(area.height.saturating_sub(keymap_h));
    let (sidebar, term) = top.split_v(sidebar_w);
    let (zoom_banner, term) = term.split_h(zoom_banner_h);

    Self {
      sidebar,
      term,
      keymap,
      zoom_banner,
    }
  }

  pub fn term_area(&self) -> Rect {
    self.term.inner(1)
  }
}
