use tui_input::Input;

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
    CommandsMenuModal {
      input: Input::default(),
      list: ListState::default(),
      items: menu_items(""),
    }
  }
}

impl Modal for CommandsMenuModal {
  fn handle_key(&mut self, key: &Key) -> ModalResult {
    let count = self.items.len();
    match (key.code, key.mods) {
      (KeyCode::Enter, KeyMods::NONE) => {
        return match self.items.get(self.list.selected()) {
          Some(item) => ModalResult::Run(item.action.clone()),
          None => ModalResult::Close,
        };
      }
      (KeyCode::Esc, KeyMods::NONE) => return ModalResult::Close,
      (KeyCode::Up, KeyMods::NONE) | (KeyCode::Char('p'), KeyMods::CONTROL) => {
        if count > 0 {
          self
            .list
            .select((self.list.selected() + count - 1) % count, count);
        }
        return ModalResult::Keep;
      }
      (KeyCode::Down, KeyMods::NONE)
      | (KeyCode::Char('n'), KeyMods::CONTROL) => {
        if count > 0 {
          self.list.select((self.list.selected() + 1) % count, count);
        }
        return ModalResult::Keep;
      }
      _ => (),
    }
    if let Some(req) = to_input_request(key)
      && self.input.handle(req).is_some_and(|change| change.value)
    {
      self.items = menu_items(&self.input.value().to_lowercase());
    }
    ModalResult::Keep
  }

  fn size(&self) -> (u16, u16) {
    (60, 30)
  }

  fn render(&mut self, grid: &mut Grid, keymap: &Keymap) {
    let area = self.area(grid.area());
    let inner = area.inner(1);
    grid.draw_block(area, &BorderType::Rounded.chars(), Attrs::default());
    grid.fill_area(inner, ' ', Attrs::default());
    grid.draw_text(
      Rect::new(area.x + 2, area.y, inner.width, 1),
      " Commands ",
      Attrs::default().set_bold(true),
    );

    let (top, list_area) = inner.split_h(2);
    let (input_row, sep_row) = top.split_h(1);
    self.list.fit(list_area, self.items.len());

    // Input row: "/ <input>   selected/total"
    let counter = if self.items.is_empty() {
      String::new()
    } else {
      format!("{}/{}", self.list.selected() + 1, self.items.len())
    };
    let counter_width = counter.len() as u16;
    grid.draw_text(
      Rect::new(
        input_row.right().saturating_sub(counter_width),
        input_row.y,
        counter_width,
        1,
      ),
      &counter,
      Attrs::default().fg(Color::BRIGHT_BLACK),
    );
    grid.draw_text(input_row, "/ ", Attrs::default().fg(Color::YELLOW));
    let input_area = Rect::new(
      input_row.x + 2,
      input_row.y,
      input_row.width.saturating_sub(3 + counter_width),
      1,
    );
    grid.cursor_pos = Some(render_text_input(&self.input, input_area, grid));
    grid.cursor_style = CursorStyle::BlinkingBar;

    // Separator
    grid.draw_text(
      Rect::new(area.x, sep_row.y, 1, 1),
      VERTICAL_RIGHT,
      Attrs::default(),
    );
    grid.draw_text(
      Rect::new(area.right() - 1, sep_row.y, 1, 1),
      VERTICAL_LEFT,
      Attrs::default(),
    );
    grid.draw_text(
      sep_row,
      &HORIZONTAL.repeat(sep_row.width as usize),
      Attrs::default(),
    );

    // List
    let search = self.input.value().to_lowercase();
    for (row, i) in self.list.visible_range().enumerate() {
      let item = &self.items[i];
      let Some(row_rect) = list_area.row(row as u16) else {
        break;
      };
      let bg = if self.list.selected() == i {
        Color::Rgb(100, 100, 100)
      } else {
        Color::Default
      };
      let base = Attrs::default().bg(bg);
      let hl = Attrs::default().bg(bg).fg(Color::YELLOW);
      if self.list.selected() == i {
        grid.fill_area(row_rect, ' ', base);
        grid.draw_text(row_rect, "\u{258e}", hl);
      }

      let name = Rect::new(row_rect.x + 2, row_rect.y, 20, 1);
      draw_highlighted(
        grid,
        name,
        &item.name,
        &search,
        Attrs::default().bg(bg).set_bold(true),
        Attrs::default().bg(bg).fg(Color::YELLOW).set_bold(true),
      );
      let desc = Rect::new(
        row_rect.x + 22,
        row_rect.y,
        row_rect.width.saturating_sub(22),
        1,
      );
      draw_highlighted(
        grid,
        desc,
        &item.desc,
        &search,
        Attrs::default().bg(bg).fg(Color::Rgb(160, 160, 160)),
        hl,
      );

      if let Some(key) = keymap.key(KeymapGroup::Tasks, &item.action) {
        let key = key.to_string();
        let width = key.len() as u16;
        grid.draw_text(
          Rect::new(
            row_rect.right().saturating_sub(width + 1),
            row_rect.y,
            width,
            1,
          ),
          &key,
          hl,
        );
      }
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
    let r = grid.draw_text(*area, s, attrs);
    *area = area.move_left(r.width as i32);
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
