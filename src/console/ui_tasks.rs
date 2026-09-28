use std::borrow::Cow;

use unicode_width::UnicodeWidthStr;

use crate::config::config::Config;
use crate::console::state::{Scope, State};
use crate::kernel::task::{ExitInfo, TaskKind, TaskState};
use crate::term::{
  Color, Grid,
  attrs::Attrs,
  grid::{BorderType, Rect},
};

pub fn render_tasks(
  area: Rect,
  grid: &mut Grid,
  state: &mut State,
  config: &Config,
) {
  state.tasks_list.fit(area.inner(1), state.tasks.len());

  if area.width <= 2 {
    return;
  }

  let active = state.scope == Scope::Tasks;

  grid.draw_block(
    area,
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
    config.tui.sidebar.title.as_str(),
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

  let range = state.tasks_list.visible_range();
  for (row, index) in range.enumerate() {
    let Some(task) = state.tasks.get(index) else {
      continue;
    };

    let selected = index == state.selected();
    let attrs = if selected {
      Attrs::default().bg(Color::Idx(240))
    } else {
      Attrs::default()
    };
    let mut row_area = Rect {
      x: area.x + 1,
      y: area.y + 1 + row as u16,
      width: area.width.saturating_sub(2),
      height: 1,
    };

    let r = grid.draw_text(row_area, if selected { "•" } else { " " }, attrs);
    row_area.x += r.width;
    row_area.width = row_area.width.saturating_sub(r.width);

    let r = grid.draw_text(row_area, &task.name(), attrs);
    row_area.x += r.width;
    row_area.width = row_area.width.saturating_sub(r.width);

    let (status_text, status_attrs) = if task.is_up() {
      (
        Cow::from(" UP "),
        attrs.clone().set_bold(true).fg(Color::BRIGHT_GREEN),
      )
    } else if let TaskState::Exited(ExitInfo {
      ready_timeout: true,
      ..
    })
    | TaskState::Backoff(ExitInfo {
      ready_timeout: true,
      ..
    }) = task.status
    {
      (
        Cow::from(" NOT READY "),
        attrs.clone().fg(Color::BRIGHT_RED),
      )
    } else {
      match (task.kind, task.exit_code()) {
        (TaskKind::Service, Some(0)) => {
          (Cow::from(" DOWN (0)"), attrs.clone().fg(Color::BRIGHT_BLUE))
        }
        (TaskKind::Service, Some(exit_code)) => (
          Cow::from(format!(" DOWN ({})", exit_code)),
          attrs.clone().fg(Color::BRIGHT_RED),
        ),
        (TaskKind::Job, Some(0)) => {
          (Cow::from(" DONE"), attrs.clone().fg(Color::BRIGHT_BLUE))
        }
        (TaskKind::Job, Some(exit_code)) => (
          Cow::from(format!(" FAILED ({})", exit_code)),
          attrs.clone().fg(Color::BRIGHT_RED),
        ),
        (TaskKind::Service | TaskKind::Job, None) => {
          (Cow::from(" DOWN "), attrs.clone().fg(Color::BRIGHT_BLACK))
        }
      }
    };
    let status_width = status_text.width() as u16;
    let r = grid.draw_text(
      Rect {
        x: (row_area.x + row_area.width)
          .saturating_sub(status_width)
          .max(row_area.x),
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

/// Task index under a point inside the sidebar block.
pub fn task_at(area: Rect, x: u16, y: u16, state: &State) -> Option<usize> {
  let inner = area.inner(1);
  if !inner.contains(x, y) {
    return None;
  }
  state.tasks_list.index_at((y - inner.y) as usize)
}
