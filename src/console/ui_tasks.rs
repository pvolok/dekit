use std::borrow::Cow;

use unicode_width::UnicodeWidthStr;

use crate::config::config::Config;
use crate::console::state::{Scope, State};
use crate::console::task_tree::{Group, Node};
use crate::console::task_view::TaskView;
use crate::console::theme::{Rgb, TaskListTheme};
use crate::kernel::task::{ExitInfo, TaskKind, TaskState};
use crate::term::{
  Color, Grid,
  attrs::Attrs,
  grid::{BorderType, Rect},
};

/// Blank cells between the border and each row.
const PAD: u16 = 1;
/// Indent per path level.
const INDENT: u16 = 2;

pub fn render_tasks(
  area: Rect,
  grid: &mut Grid,
  state: &mut State,
  config: &Config,
  theme: &TaskListTheme,
) {
  let active = state.scope == Scope::Tasks;

  let border = if active {
    BorderType::Thick
  } else {
    BorderType::Plain
  };
  let mut block = grid.block(area, border);
  block.title(
    config.tui.sidebar.title.as_str(),
    if active {
      Attrs::default().set_bold(true)
    } else {
      Attrs::default()
    },
  );
  if state.quitting {
    block.gap(1).title(
      "QUITTING",
      Attrs::default()
        .fg(Color::BLACK)
        .bg(Color::RED)
        .set_bold(true),
    );
  }
  let inner = block.inner();
  grid.fill_area(inner, ' ', Attrs::default().bg(theme.bg.into()));

  let tasks = &mut state.tasks;
  for list_row in tasks.rows(inner) {
    let Some((path, node)) = tasks.row(list_row.index) else {
      continue;
    };
    let bg = if list_row.selected {
      theme.selected_bg
    } else {
      theme.bg
    };
    let paint = |color: Rgb| Attrs::default().bg(bg.into()).fg(color.into());
    let (status_text, status_color) = match node {
      Node::Task(task) => task_status(task, theme),
      Node::Group(group) => group_status(group, theme),
    };

    grid.fill_area(list_row.area, ' ', paint(theme.text));
    let depth = path.depth().saturating_sub(1) as u16;
    let left = PAD + INDENT.saturating_mul(depth);
    let mut area = list_row.area.inner((0, 0, 0, left));
    let status = area.take_right(status_text.width() as u16);
    grid.draw_text(status, &status_text, paint(status_color));

    // Groups end like directories: `web/` open, `web/…` closed.
    let (name, suffix) = match node {
      // A label equal to the path would repeat the groups above.
      Node::Task(task) => match &task.label {
        Some(label) if label != path.as_str() => (label.as_str(), ""),
        Some(_) | None => (path.name(), ""),
      },
      Node::Group(group) if group.collapsed => (path.name(), "/…"),
      Node::Group(_) => (path.name(), "/"),
    };
    let mut name_area = area.take_left((name.width() + suffix.width()) as u16);
    let suffix_area = name_area.take_right(suffix.width() as u16);
    if name.width() > name_area.width as usize {
      grid.draw_text(name_area.take_right(1), "…", paint(theme.text));
    }
    grid.draw_text(name_area, name, paint(theme.text));
    grid.draw_text(suffix_area, suffix, paint(theme.mark));
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
  } else if group.up == group.len() {
    theme.up
  } else {
    theme.down
  };
  (Cow::from(format!(" {}/{} ", group.up, group.len())), color)
}
