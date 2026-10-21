use unicode_width::UnicodeWidthStr;

use crate::console::action::{Action, FoldOp, RunnerTab, ScrollUnit};
use crate::console::{
  keymap::{Keymap, KeymapGroup},
  theme::Theme,
  widgets::filter_list::{FilterInput, FilterList},
};
use crate::term::{
  CursorStyle, Grid,
  grid::Rect,
  key::{Key, KeyCode, KeyMods},
  mouse::MouseEvent,
  style::Style,
};

use super::modal::{Modal, ModalResult};

pub struct CommandsMenuModal {
  filter: FilterList,
  items: Vec<MenuItem>,
}

struct MenuItem {
  name: String,
  desc: String,
  action: Action,
}

impl CommandsMenuModal {
  pub fn new() -> Self {
    let mut menu = CommandsMenuModal {
      filter: FilterList::default(),
      items: menu_items(),
    };
    menu.refilter();
    menu
  }

  fn refilter(&mut self) {
    let search = self.filter.value().to_lowercase();
    let shown = (0..self.items.len())
      .filter(|&i| self.items[i].matches(&search))
      .collect();
    self.filter.show(shown);
  }

  fn run(&self, item: Option<usize>) -> ModalResult {
    match item.and_then(|i| self.items.get(i)) {
      Some(item) => ModalResult::Run(item.action.clone()),
      None => ModalResult::Close,
    }
  }

  fn took(&mut self, input: FilterInput) -> ModalResult {
    match input {
      FilterInput::Edited => self.refilter(),
      FilterInput::Picked(item) => return self.run(Some(item)),
      FilterInput::Moved | FilterInput::Ignored => (),
    }
    ModalResult::Keep
  }
}

impl Modal for CommandsMenuModal {
  fn handle_key(&mut self, key: &Key) -> ModalResult {
    match (key.code, key.mods) {
      (KeyCode::Enter, KeyMods::NONE) => self.run(self.filter.selected()),
      (KeyCode::Esc, KeyMods::NONE) => ModalResult::Close,
      _ => {
        let input = self.filter.handle_key(key);
        self.took(input)
      }
    }
  }

  fn handle_mouse(&mut self, mouse: &MouseEvent) -> ModalResult {
    let input = self.filter.handle_mouse(mouse);
    self.took(input)
  }

  fn handle_paste(&mut self, text: &str) -> ModalResult {
    let input = self.filter.handle_paste(text);
    self.took(input)
  }

  fn size(&self, _frame: Rect) -> (u16, u16) {
    (60, 30)
  }

  fn render(&mut self, grid: &mut Grid, keymap: &Keymap, theme: &Theme) {
    let area = self.area(grid.area());
    let style = theme.block.modal();
    let mut block = style.draw(grid, area);
    block.title(" Commands ", style.title);
    let mut inner = block.inner();
    let input_row = inner.take_top(1);
    let sep_row = inner.take_top(1);
    let list_area = inner;
    block.separator(sep_row.y);

    grid.cursor_pos =
      Some(self.filter.render_input(grid, input_row, "/ ", &style));
    grid.cursor_style = CursorStyle::BlinkingBar;

    let search = self.filter.value().to_lowercase();
    for (item, area, style) in
      self.filter.rows(grid, list_area, style, &theme.block)
    {
      let item = &self.items[item];
      // The description gets what the key leaves. The action's name is
      // not shown: actions with fields would share it. It still matches
      // the search.
      let mut rest = area.inner((0, 1, 0, 2));
      if let Some(key) = keymap.key(KeymapGroup::Tasks, &item.action) {
        let key = key.to_string();
        grid.draw_text(rest.take_right(key.width() as u16), &key, style.key);
        rest.take_right(1);
      }
      draw_highlighted(grid, rest, &item.desc, &search, style.text, style.key);
    }
  }
}

fn draw_highlighted(
  grid: &mut Grid,
  mut area: Rect,
  text: &str,
  search: &str,
  base: Style,
  hl: Style,
) {
  let mut draw = |area: &mut Rect, s: &str, attrs: Style| {
    grid.draw_text(area.take_left(s.width() as u16), s, attrs);
  };
  if search.is_empty() {
    draw(&mut area, text, base);
    return;
  }
  let lower = text.to_lowercase();
  let mut last = 0;
  for (start, _) in lower.match_indices(search) {
    let end = start + search.len();
    if start < last
      || !text.is_char_boundary(start)
      || !text.is_char_boundary(end)
    {
      continue;
    }
    draw(&mut area, &text[last..start], base);
    draw(&mut area, &text[start..end], hl);
    last = end;
  }
  draw(&mut area, &text[last..], base);
}

impl MenuItem {
  fn matches(&self, search: &str) -> bool {
    self.name.contains(search) || self.desc.to_lowercase().contains(search)
  }
}

fn menu_items() -> Vec<MenuItem> {
  let actions = [
    Action::Detach,
    Action::Quit,
    Action::ToggleFocus,
    Action::FocusTerm,
    Action::Zoom,
    Action::ShowCommandsMenu,
    Action::ShowMainMenu,
    Action::ShowRunnerSelect {
      tab: RunnerTab::Switch,
    },
    Action::ShowRunnerSelect {
      tab: RunnerTab::Open,
    },
    Action::NextTask,
    Action::PrevTask,
    Action::Expand,
    Action::Collapse,
    Action::Fold {
      op: FoldOp::Close,
      target: Some("**".to_string()),
    },
    Action::Fold {
      op: FoldOp::Open,
      target: Some("**".to_string()),
    },
    Action::StartTask,
    Action::StopTask,
    Action::KillTask,
    Action::VetoTask,
    Action::RestartTask,
    Action::RestartAll,
    Action::DuplicateTask,
    Action::ForceRestartTask,
    Action::ForceRestartAll,
    Action::ShowAddTask,
    Action::ShowEditTask,
    Action::ShowDuplicateTask,
    Action::ShowRemoveTask,
    Action::ShowTaskMenu,
    Action::MoveTaskUp,
    Action::MoveTaskDown,
    Action::CopyTaskCommand,
    Action::ShowReload,
    Action::ShowWriteBack,
    Action::SaveToConfig,
    Action::ScrollDown {
      n: 1,
      unit: ScrollUnit::HalfScreen,
    },
    Action::ScrollUp {
      n: 1,
      unit: ScrollUnit::HalfScreen,
    },
    Action::CopyModeEnter,
    Action::CopyModeLeave,
    Action::CopyModeEnd,
    Action::CopyModeCopy,
  ];
  actions
    .into_iter()
    .map(|action| MenuItem {
      name: action.name(),
      desc: action.desc(),
      action,
    })
    .collect()
}

#[cfg(test)]
mod tests {
  use super::*;

  /// The description is all a row shows, so two actions must not share
  /// one.
  #[test]
  fn descriptions_are_unique() {
    let items = menu_items();
    let mut descs: Vec<&str> = items.iter().map(|i| i.desc.as_str()).collect();
    descs.sort_unstable();
    let before = descs.len();
    descs.dedup();
    assert_eq!(descs.len(), before, "{descs:?}");
  }

  #[test]
  fn the_search_matches_the_name_too() {
    let found = |search: &str| -> Vec<MenuItem> {
      menu_items()
        .into_iter()
        .filter(|i| i.matches(search))
        .collect()
    };
    let items = found("show-runner");
    assert_eq!(items.len(), 2, "both runner-select spellings");
    assert!(items.iter().all(|i| i.name == "show-runner-select"));
    assert!(found("open project").len() == 1);
  }
}
