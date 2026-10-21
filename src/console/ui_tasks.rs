use std::borrow::Cow;

use unicode_width::UnicodeWidthStr;

use crate::config::config::Config;
use crate::console::action::Action;
use crate::console::keymap::{Keymap, KeymapGroup};
use crate::console::state::{Scope, State};
use crate::console::task_tree::{Group, Node};
use crate::console::task_view::TaskView;
use crate::console::theme::{TaskListTheme, Theme};
use crate::console::widgets::wrap::wrap;
use crate::kernel::task::{ExitInfo, TaskKind, TaskState};
use crate::kernel::task_path::is_at_or_under;
use crate::term::{Grid, Rgb, grid::Rect, style::Style};

/// Blank cells between the border and each row.
pub(crate) const PAD: u16 = 1;
/// Indent per path level.
pub(crate) const INDENT: u16 = 2;

pub fn render_tasks(
  area: Rect,
  grid: &mut Grid,
  state: &mut State,
  config: &Config,
  keymap: &Keymap,
  theme: &Theme,
) {
  let style = theme.block.panel(state.scope == Scope::Tasks);
  let mut block = style.draw(grid, area);
  block.title(&format!(" {} ", config.tui.sidebar.title), style.title);
  if state.quitting {
    block.title(" QUITTING ", style.alert);
  }
  // Changes here not in the config, in the color of the marks on the
  // tasks they are on; a click offers to write them back.
  // Where it was drawn last, for the hover.
  let hovered = state
    .unsaved_hit
    .take()
    .zip(state.hover)
    .is_some_and(|(hit, (x, y))| hit.contains(x, y));
  let unsaved = state.config.unsaved();
  if unsaved > 0 {
    let text = format!(" \u{2022} {unsaved} unsaved ");
    let mut bg = theme.block.bg;
    if hovered {
      bg = bg.blend(theme.block.text, 24);
    }
    let attrs = Style::default().bg(bg.into()).fg(theme.block.key.into());
    state.unsaved_hit = block.title_right(&text, attrs);
  }
  let inner = block.inner();
  let accent = Style::default()
    .bg(theme.block.bg.into())
    .fg(theme.block.border_focused.into());

  let dragging = state
    .drag
    .as_ref()
    .filter(|drag| drag.active)
    .map(|drag| (drag.from.clone(), drag.drop.clone()));
  if let Some((_, Some(drop))) = &dragging
    && drop.row.is_none()
  {
    // The top of the list: a line on the border above it, after the
    // title.
    let title = config.tui.sidebar.title.width() as u16 + 3;
    let mut line = Rect::new(inner.x, area.y, inner.width, 1);
    line.take_left(title.min(line.width));
    grid.fill_area(line, '━', accent);
  }
  let tasks = &mut state.tasks;
  for list_row in tasks.rows(inner) {
    let Some((path, node)) = tasks.row(list_row.index) else {
      continue;
    };
    let hovered = state
      .hover
      .is_some_and(|(x, y)| list_row.area.contains(x, y));
    let bg = if list_row.selected {
      theme.block.selected_bg
    } else if hovered && dragging.is_none() {
      theme.block.bg.blend(theme.block.text, 14)
    } else {
      theme.block.bg
    };
    let carried = dragging
      .as_ref()
      .is_some_and(|(from, _)| is_at_or_under(path.as_str(), from.as_str()));
    let paint = |color: Rgb| {
      let color = if carried { color.blend(bg, 140) } else { color };
      Style::default().bg(bg.into()).fg(color.into())
    };
    let (status_text, status_color) = match node {
      Node::Task(task) => task_status(task, &theme.tasks),
      Node::Group(group) => group_status(group, &theme.tasks),
    };

    grid.fill_area(list_row.area, ' ', paint(theme.block.text));
    let depth = path.depth().saturating_sub(1) as u16;
    let left = PAD + INDENT.saturating_mul(depth);
    let mut area = list_row.area.inner((0, 0, 0, left));
    let status = area.take_right(status_text.width() as u16);
    grid.draw_text(status, &status_text, paint(status_color));

    // Groups end like directories: `web/` open, `web/…` closed over what
    // is in it.
    let (name, suffix) = match node {
      // A label equal to the path would repeat the groups above.
      Node::Task(task) => match &task.label {
        Some(label) if label != path.as_str() => (label.as_str(), ""),
        Some(_) | None => (path.name(), ""),
      },
      Node::Group(group) if group.collapsed && !group.is_empty() => {
        (path.name(), "/…")
      }
      Node::Group(_) => (path.name(), "/"),
    };
    // A config task or folder changed here and not written back.
    let edited = match node {
      Node::Task(task) => state.config.edited.contains(&task.id),
      Node::Group(group) => group
        .folder
        .is_some_and(|id| state.config.edited.contains(&id)),
    };
    let edited_w = if edited { 2 } else { 0 };
    let mut name_area =
      area.take_left((name.width() + suffix.width()) as u16 + edited_w);
    let edited_area = name_area.take_right(edited_w);
    let suffix_area = name_area.take_right(suffix.width() as u16);
    if name.width() > name_area.width as usize {
      grid.draw_text(name_area.take_right(1), "…", paint(theme.block.text));
    }
    grid.draw_text(name_area, name, paint(theme.block.text));
    grid.draw_text(suffix_area, suffix, paint(theme.tasks.mark));
    if edited {
      let mut dot = edited_area;
      dot.take_left(1);
      grid.draw_text(dot, "•", paint(theme.block.key));
    }

    if let Some((_, Some(drop))) = &dragging
      && drop.row == Some(list_row.index)
    {
      let left = PAD + INDENT.saturating_mul(drop.parent.depth() as u16);
      let line = list_row.area.inner((0, 0, 0, left));
      let color = theme.block.border_focused;
      grid.map_styles(line, |style| {
        style.set_underlined(true).ul_color(color.into())
      });
    }
  }

  if let Some(toast) = &state.toast {
    // At the bottom, as many rows as it takes.
    let lines = wrap(&toast.text, inner.width.saturating_sub(2));
    let height = (lines.len() as u16).min(inner.height);
    let area = Rect {
      y: inner.bottom().saturating_sub(height),
      height,
      ..inner
    };
    let (fg, bg) = if toast.error {
      (theme.block.alert, theme.block.alert_bg)
    } else {
      (theme.block.text, theme.block.selected_bg)
    };
    let style = Style::default().bg(bg.into()).fg(fg.into());
    grid.fill_area(area, ' ', style);
    for (row, line) in area.inner((0, 1)).rows().zip(&lines) {
      grid.draw_text(row, line, style);
    }
  }

  if tasks.row_count() == 0 && inner.height > 2 {
    let mut hint = inner.inner((1, PAD));
    let dim = style.dim;
    grid.draw_text(hint.take_top(1), "No tasks yet.", dim);
    match keymap.key(KeymapGroup::Tasks, &Action::ShowAddTask) {
      Some(key) => {
        let line = format!("Press {key} to add one,");
        grid.draw_text(hint.take_top(1), &line, dim);
        grid.draw_text(hint.take_top(1), "or right-click here.", dim);
      }
      None => {
        grid.draw_text(hint.take_top(1), "Right-click here to add", dim);
        grid.draw_text(hint.take_top(1), "one.", dim);
      }
    }
  }
}

fn task_status(
  task: &TaskView,
  theme: &TaskListTheme,
) -> (Cow<'static, str>, Rgb) {
  if task.is_up() {
    return (Cow::from(" UP "), theme.up);
  }
  let color = match (task.failed(), task.exit_code()) {
    (true, _) => theme.failed,
    (false, Some(_)) => theme.done,
    (false, None) => theme.down,
  };
  let text = if let TaskState::Exited(ExitInfo {
    ready_timeout: true,
    ..
  })
  | TaskState::Backoff(ExitInfo {
    ready_timeout: true,
    ..
  }) = task.status
  {
    Cow::from(" NOT READY ")
  } else {
    match (task.kind, task.exit_code()) {
      (TaskKind::Service, Some(code)) => {
        Cow::from(format!(" DOWN ({}) ", code))
      }
      (TaskKind::Job, Some(0)) => Cow::from(" DONE "),
      (TaskKind::Job, Some(code)) => Cow::from(format!(" FAILED ({}) ", code)),
      (TaskKind::Service | TaskKind::Job, None) => Cow::from(" DOWN "),
    }
  };
  (text, color)
}

/// How many tasks under a group are up.
fn group_status(
  group: &Group,
  theme: &TaskListTheme,
) -> (Cow<'static, str>, Rgb) {
  let color = if group.failed > 0 {
    theme.failed
  } else if group.len() > 0 && group.up == group.len() {
    theme.up
  } else {
    theme.down
  };
  (Cow::from(format!(" {}/{} ", group.up, group.len())), color)
}
