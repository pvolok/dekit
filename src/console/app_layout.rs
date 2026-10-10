use crate::{config::config::Config, term::grid::Rect};

pub struct AppLayout {
  /// The bar across the top: logo, runner, versions. Gone in zoom.
  pub header: Rect,
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
    let mut term = area;
    let header = term.take_top(if zoom { 0 } else { 1 });
    let keymap = term.take_bottom(keymap_h);
    let sidebar = term.take_left(sidebar_w);
    let zoom_banner = term.take_top(zoom_banner_h);

    Self {
      header,
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
