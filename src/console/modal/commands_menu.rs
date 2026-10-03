use tui_input::Input;
use unicode_width::UnicodeWidthStr;

use crate::console::action::{Action, ScrollUnit};
use crate::console::{
  keymap::{Keymap, KeymapGroup},
  widgets::{
    list::ListState,
    text_input::{render_text_input, to_input_request},
  },
};
use crate::term::{
  Color, CursorStyle, Grid,
  attrs::Attrs,
  grid::{BorderType, Rect},
  key::{Key, KeyCode, KeyMods},
  line_symbols::{HORIZONTAL, VERTICAL_LEFT, VERTICAL_RIGHT},
};

use super::modal::{Modal, ModalResult};

pub struct CommandsMenuModal {
  input: Input,
  list: ListState,
  items: Vec<MenuItem>,
}

struct MenuItem {
  name: String,
  desc: String,
  action: Action,
}

impl CommandsMenuModal {
  pub fn new() -> Self {
    let items = menu_items("");
    let mut list = ListState::new(0);
    list.reset(items.len());
    CommandsMenuModal {
      input: Input::default(),
      list,
      items,
    }
  }
}

impl Modal for CommandsMenuModal {
  fn handle_key(&mut self, key: &Key) -> ModalResult {
    match (key.code, key.mods) {
      (KeyCode::Enter, KeyMods::NONE) => {
        let item = self.list.selected().and_then(|i| self.items.get(i));
        return match item {
          Some(item) => ModalResult::Run(item.action.clone()),
          None => ModalResult::Close,
        };
      }
      (KeyCode::Esc, KeyMods::NONE) => return ModalResult::Close,
      (KeyCode::Up, KeyMods::NONE) | (KeyCode::Char('p'), KeyMods::CONTROL) => {
        self.list.prev();
        return ModalResult::Keep;
      }
      (KeyCode::Down, KeyMods::NONE)
      | (KeyCode::Char('n'), KeyMods::CONTROL) => {
        self.list.next();
        return ModalResult::Keep;
      }
      (KeyCode::PageUp, KeyMods::NONE) => {
        self.list.move_by(-(self.list.page() as isize));
        return ModalResult::Keep;
      }
      (KeyCode::PageDown, KeyMods::NONE) => {
        self.list.move_by(self.list.page() as isize);
        return ModalResult::Keep;
      }
      _ => (),
    }
    if let Some(req) = to_input_request(key)
      && self.input.handle(req).is_some_and(|change| change.value)
    {
      self.items = menu_items(&self.input.value().to_lowercase());
      self.list.reset(self.items.len());
    }
    ModalResult::Keep
  }

  fn size(&self, _frame: Rect) -> (u16, u16) {
    (60, 30)
  }

  fn render(&mut self, grid: &mut Grid, keymap: &Keymap) {
    let area = self.area(grid.area());
    let mut inner = grid
      .block(area, BorderType::Rounded)
      .gap(1)
      .title(" Commands ", Attrs::default().set_bold(true))
      .inner();
    grid.fill_area(inner, ' ', Attrs::default());

    let mut input_row = inner.take_top(1);
    let sep_row = inner.take_top(1);
    let list_area = inner;

    // Input row: "/ <input>   selected/total"
    let counter = match self.list.selected() {
      Some(i) => format!("{}/{}", i + 1, self.items.len()),
      None => String::new(),
    };
    grid.draw_text(
      input_row.take_left(2),
      "/ ",
      Attrs::default().fg(Color::YELLOW),
    );
    grid.draw_text(
      input_row.take_right(counter.width() as u16),
      &counter,
      Attrs::default().fg(Color::BRIGHT_BLACK),
    );
    let input_area = input_row.inner((0, 1, 0, 0));
    grid.cursor_pos = Some(render_text_input(&self.input, input_area, grid));
    grid.cursor_style = CursorStyle::BlinkingBar;

    // Separator, joined to the border on both sides
    let mut sep = Rect {
      x: area.x,
      width: area.width,
      ..sep_row
    };
    grid.draw_text(sep.take_left(1), VERTICAL_RIGHT, Attrs::default());
    grid.draw_text(sep.take_right(1), VERTICAL_LEFT, Attrs::default());
    grid.draw_text(
      sep,
      &HORIZONTAL.repeat(sep.width as usize),
      Attrs::default(),
    );

    // List
    let search = self.input.value().to_lowercase();
    for row in self.list.rows(list_area) {
      let item = &self.items[row.index];
      let bg = if row.selected {
        Color::Rgb(100, 100, 100)
      } else {
        Color::Default
      };
      let base = Attrs::default().bg(bg);
      let hl = Attrs::default().bg(bg).fg(Color::YELLOW);
      if row.selected {
        grid.fill_area(row.area, ' ', base);
        grid.draw_text(row.area, "\u{258e}", hl);
      }

      // The description gets what the name and the key leave.
      let mut rest = row.area.inner((0, 1, 0, 2));
      draw_highlighted(
        grid,
        rest.take_left(20),
        &item.name,
        &search,
        Attrs::default().bg(bg).set_bold(true),
        Attrs::default().bg(bg).fg(Color::YELLOW).set_bold(true),
      );
      if let Some(key) = keymap.key(KeymapGroup::Tasks, &item.action) {
        let key = key.to_string();
        grid.draw_text(rest.take_right(key.width() as u16), &key, hl);
      }
      draw_highlighted(
        grid,
        rest,
        &item.desc,
        &search,
        Attrs::default().bg(bg).fg(Color::Rgb(160, 160, 160)),
        hl,
      );
    }
  }
}

fn draw_highlighted(
  grid: &mut Grid,
  mut area: Rect,
  text: &str,
  search: &str,
  base: Attrs,
  hl: Attrs,
) {
  let mut draw = |area: &mut Rect, s: &str, attrs: Attrs| {
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

fn menu_items(search: &str) -> Vec<MenuItem> {
  let actions = [
    Action::Detach,
    Action::Quit,
    Action::ToggleFocus,
    Action::FocusTerm,
    Action::Zoom,
    Action::ShowCommandsMenu,
    Action::NextTask,
    Action::PrevTask,
    Action::Expand,
    Action::Collapse,
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
    Action::ShowRenameTask,
    Action::ShowRemoveTask,
    Action::CloseCurrentModal,
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
    .filter(|item| {
      item.name.contains(search) || item.desc.to_lowercase().contains(search)
    })
    .collect()
}
