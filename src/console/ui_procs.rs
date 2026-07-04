use std::borrow::Cow;

use unicode_width::UnicodeWidthStr;

use crate::config::config::Config;
use crate::console::state::{Scope, State};
use crate::term::{
  Color, Grid,
  attrs::Attrs,
  grid::{BorderType, Rect},
};

pub fn render_procs(
  area: Rect,
  grid: &mut Grid,
  state: &mut State,
  config: &Config,
  frame: bool,
) {
  let inner = if frame { area.inner(1) } else { area };
  state.procs_list.fit(inner, state.procs.len());

  if area.width == 0 || area.height == 0 {
    return;
  }

  let active = state.scope == Scope::Procs;

  if frame {
    if area.width <= 2 {
      return;
    }

    grid.draw_block(
      area.into(),
      &if active {
        BorderType::Thick
      } else {
        BorderType::Plain
      }
      .chars(),
      Attrs::default(),
    );
    let title_area = Rect {
      x: area.x + 1,
      y: area.y,
      width: area.width - 2,
      height: 1,
    };
    let r = grid.draw_text(
      title_area,
      config.tui.procs.title.as_str(),
      if active {
        Attrs::default().set_bold(true)
      } else {
        Attrs::default()
      },
    );
    if state.quitting {
      let area = title_area.inner((0, 0, 0, r.width + 1));
      grid.draw_text(
        area,
        "QUITTING",
        Attrs::default()
          .fg(Color::BLACK)
          .bg(Color::RED)
          .set_bold(true),
      );
    }
  }

  let range = state.procs_list.visible_range();
  for (row, index) in range.enumerate() {
    let proc = if let Some(proc) = state.procs.get(index) {
      proc
    } else {
      continue;
    };

    let selected = index == state.selected();
    let attrs = if selected {
      Attrs::default().bg(crate::term::Color::Idx(240))
    } else {
      Attrs::default()
    };
    let mut row_area = Rect {
      x: inner.x,
      y: inner.y + row as u16,
      width: inner.width,
      height: 1,
    };

    let show_marker = selected && (frame || active);
    let r =
      grid.draw_text(row_area, if show_marker { "•" } else { " " }, attrs);
    row_area.x += r.width;
    row_area.width = row_area.width.saturating_sub(r.width);

    let r = grid.draw_text(row_area, proc.name(), attrs);
    row_area.x += r.width;
    row_area.width = row_area.width.saturating_sub(r.width);

    let (status_text, status_attrs) = if proc.is_up() {
      (
        Cow::from(" UP "),
        attrs.clone().set_bold(true).fg(Color::BRIGHT_GREEN),
      )
    } else {
      match proc.exit_code() {
        Some(0) => {
          (Cow::from(" DOWN (0)"), attrs.clone().fg(Color::BRIGHT_BLUE))
        }
        Some(exit_code) => (
          Cow::from(format!(" DOWN ({})", exit_code)),
          attrs.clone().fg(Color::BRIGHT_RED),
        ),
        None => (Cow::from(" DOWN "), attrs.clone().fg(Color::BRIGHT_BLACK)),
      }
    };
    let status_width = status_text.width() as u16;
    let r = grid.draw_text(
      Rect {
        x: row_area.x.max(row_area.x + row_area.width - status_width),
        width: status_width.min(row_area.width),
        ..row_area
      },
      &status_text,
      status_attrs,
    );
    row_area.width = row_area.width.saturating_sub(r.width);

    grid.fill_area(row_area, ' ', attrs);
  }
}

pub fn procs_get_clicked_index(
  area: Rect,
  x: u16,
  y: u16,
  state: &State,
  frame: bool,
) -> Option<usize> {
  let inner = if frame { area.inner(1) } else { area };
  if procs_check_hit(area, x, y, frame) {
    let index = y - inner.y;
    let scroll = (state.selected() + 1).saturating_sub(inner.height as usize);
    let index = index as usize + scroll;
    if index < state.procs.len() {
      return Some(index);
    }
  }
  None
}

pub fn procs_check_hit(area: Rect, x: u16, y: u16, frame: bool) -> bool {
  if frame {
    area.x < x
      && area.x + area.width > x + 1
      && area.y < y
      && area.y + area.height > y + 1
  } else {
    area.x <= x
      && area.x + area.width >= x + 1
      && area.y <= y
      && area.y + area.height >= y + 1
  }
}
