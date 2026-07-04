use crate::{config::config::Config, term::grid::Rect};

pub struct AppLayout {
  pub procs: Rect,
  pub term: Rect,
  pub keymap: Rect,
  pub zoom_banner: Rect,
  pub proc_frame: bool,
}

impl AppLayout {
  pub fn new(
    area: Rect,
    zoom: bool,
    hide_keymap_window: bool,
    config: &Config,
  ) -> Self {
    let keymap_h = if zoom || hide_keymap_window { 0 } else { 3 };
    let procs_w = if zoom {
      0
    } else {
      config.tui.procs.width as u16
    };
    let proc_frame = !config.tui.hide_proc_frame;
    let zoom_banner_h = if zoom && !config.tui.hide_zen_message {
      1
    } else {
      0
    };
    let (top, keymap) = area.split_h(area.height.saturating_sub(keymap_h));
    let (procs, term) = top.split_v(procs_w);
    let (zoom_banner, term) = term.split_h(zoom_banner_h);

    Self {
      procs,
      term,
      keymap,
      zoom_banner,
      proc_frame,
    }
  }

  pub fn term_area(&self) -> Rect {
    if self.proc_frame {
      self.term.inner(1)
    } else {
      self.term
    }
  }
}
