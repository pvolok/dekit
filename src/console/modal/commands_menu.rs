use std::collections::HashMap;

use tui_input::Input;

use crate::console::action::Action;
use crate::console::{
  app::LoopAction,
  keymap::Keymap,
  state::State,
  widgets::{
    list::ListState,
    text_input::{render_text_input, to_input_request},
  },
};
use crate::kernel::kernel_message::TaskContext;
use crate::term::{
  Color, Grid, TermEvent,
  attrs::Attrs,
  encode::print_key,
  grid::{BorderType, Pos, Rect},
  key::{Key, KeyCode, KeyMods},
  line_symbols::{HORIZONTAL, VERTICAL_LEFT, VERTICAL_RIGHT},
};

use super::modal::Modal;

pub struct CommandsMenuModal {
  pc: TaskContext,
  input: Input,
  list_state: ListState,
  items: Vec<CommandInfo>,
  key_bindings: HashMap<Action, String>,
}

impl CommandsMenuModal {
  pub fn new(pc: TaskContext, keymap: &Keymap) -> Self {
    let mut key_bindings = HashMap::new();
    for (event, key) in &keymap.rev_procs {
      key_bindings
        .entry(event.clone())
        .or_insert_with(|| print_key(key));
    }

    CommandsMenuModal {
      pc,
      input: Input::default(),
      list_state: ListState::default(),
      items: get_commands(""),
      key_bindings,
    }
  }
}

impl Modal for CommandsMenuModal {
  fn handle_input(
    &mut self,
    _state: &mut State,
    loop_action: &mut LoopAction,
    event: &TermEvent,
  ) -> bool {
    match event {
      TermEvent::Key(Key {
        code: KeyCode::Enter,
        mods,
        ..
      }) if mods.is_empty() => {
        self.pc.send_self_custom(Action::CloseCurrentModal);
        if let Some(item) = self.items.get(self.list_state.selected()) {
          self.pc.send_self_custom(item.event.clone());
        }
        // Skip because AddProc event will immediately rerender.
        return true;
      }
      TermEvent::Key(Key {
        code: KeyCode::Esc,
        mods,
        ..
      }) if mods.is_empty() => {
        self.pc.send_self_custom(Action::CloseCurrentModal);
        loop_action.render();
        return true;
      }
      // List navigation
      TermEvent::Key(Key { code, mods, .. })
        if (code == &KeyCode::Up && mods.is_empty())
          || (code == &KeyCode::Char('p') && mods == &KeyMods::CONTROL) =>
      {
        if !self.items.is_empty() {
          let index = self.list_state.selected();
          let index = if index == 0 {
            self.items.len() - 1
          } else {
            index - 1
          };
          self.list_state.select(index);
          loop_action.render();
        }
        return true;
      }
      TermEvent::Key(Key { code, mods, .. })
        if (code == &KeyCode::Down && mods.is_empty())
          || (code == &KeyCode::Char('n') && mods == &KeyMods::CONTROL) =>
      {
        if !self.items.is_empty() {
          let index = self.list_state.selected();
          let index = if index >= self.items.len() - 1 {
            0
          } else {
            index + 1
          };
          self.list_state.select(index);
          loop_action.render();
        }
        return true;
      }
      _ => (),
    }

    let req = to_input_request(event);
    if let Some(req) = req {
      let res = self.input.handle(req);
      if let Some(res) = res {
        if res.value {
          self.items = get_commands(self.input.value());
        }
      }
      loop_action.render();
      return true;
    }

    match event {
      TermEvent::FocusGained => false,
      TermEvent::FocusLost => false,
      // Block keys
      TermEvent::Key(_) => true,
      // Block mouse
      TermEvent::Mouse(_) => true,
      // Block paste
      TermEvent::Paste(_) => true,
      TermEvent::Resize(_, _) => false,
    }
  }

  fn get_size(&mut self, _: Rect) -> (u16, u16) {
    (60, 30)
  }

  fn render(&mut self, grid: &mut Grid) {
    let area = self.area(Rect {
      x: 0,
      y: 0,
      width: grid.size().width,
      height: grid.size().height,
    });

    grid.draw_block(
      area.into(),
      &BorderType::Rounded.chars(),
      Attrs::default(),
    );

    let inner = area.inner(1);

    // Fill inner
    grid.fill_area(inner.into(), ' ', Attrs::default());

    // Title
    let title = " Commands ";
    let title_attrs = Attrs::default().set_bold(true);
    grid.draw_text(
      Rect::new(area.x + 2, area.y, inner.x + 1, 1),
      title,
      title_attrs,
    );

    let list_area = Rect {
      x: inner.x,
      y: inner.y + 2,
      width: inner.width,
      height: inner.height.saturating_sub(2),
    };
    let sep_y = inner.y + 1;

    self.list_state.fit(list_area, self.items.len());

    let desc_col = 22u16;

    // "/ "
    grid.draw_text(
      Rect::new(inner.x, inner.y, 2, 1),
      "/ ",
      Attrs::default().fg(Color::YELLOW),
    );

    // Counter (selected/total)
    let total = self.items.len();
    let counter_width = if total > 0 {
      let counter_text =
        format!("{}/{}", self.list_state.selected() + 1, total);
      let counter_width = counter_text.len() as u16;
      grid
        .draw_text(
          Rect::new(
            inner.x + inner.width.saturating_sub(counter_width),
            inner.y,
            counter_width,
            1,
          ),
          &counter_text,
          Attrs::default().fg(Color::BRIGHT_BLACK),
        )
        .width
    } else {
      0
    };

    // Input
    let input_area = Rect::new(
      inner.x + 2,
      inner.y,
      inner.width.saturating_sub(2 + counter_width + 1),
      1,
    );
    let mut cursor = (0u16, 0u16);
    render_text_input(&mut self.input, input_area, grid, &mut cursor);

    // Separator
    grid.draw_text(
      Rect::new(area.x, sep_y, 1, 1),
      VERTICAL_RIGHT,
      Attrs::default(),
    );
    grid.draw_text(
      Rect::new(area.x + area.width - 1, sep_y, 1, 1),
      VERTICAL_LEFT,
      Attrs::default(),
    );
    grid.draw_text(
      Rect::new(inner.x, sep_y, inner.width, 1),
      HORIZONTAL.repeat(inner.width as usize).as_str(),
      Attrs::default(),
    );

    // List
    let selected_bg = Color::Rgb(100, 100, 100);
    let search = self.input.value().to_lowercase();
    let range = self.list_state.visible_range();
    for (row, i) in range.enumerate() {
      let item = &self.items[i];
      let selected = self.list_state.selected() == i;

      let row_y = list_area.y + row as u16;
      let row_rect = Rect::new(list_area.x, row_y, list_area.width, 1);

      if selected {
        grid.fill_area(row_rect, ' ', Attrs::default().bg(selected_bg));

        // Accent bar on left edge
        grid.draw_text(
          Rect::new(list_area.x, row_y, 1, 1),
          "\u{258e}", // ▎
          Attrs::default().fg(Color::YELLOW).bg(selected_bg),
        );
      }

      let name_attrs = if selected {
        Attrs::default().bg(selected_bg).set_bold(true)
      } else {
        Attrs::default()
      };
      let name_hl = if selected {
        Attrs::default()
          .fg(Color::YELLOW)
          .bg(selected_bg)
          .set_bold(true)
      } else {
        Attrs::default().fg(Color::YELLOW).set_bold(true)
      };

      // Description attrs
      let desc_attrs = if selected {
        Attrs::default()
          .fg(Color::Rgb(170, 170, 170))
          .bg(selected_bg)
      } else {
        Attrs::default().fg(Color::Rgb(150, 150, 150))
      };
      let desc_hl = if selected {
        Attrs::default().fg(Color::YELLOW).bg(selected_bg)
      } else {
        Attrs::default().fg(Color::YELLOW)
      };

      let key_attrs = if selected {
        Attrs::default().fg(Color::YELLOW).bg(selected_bg)
      } else {
        Attrs::default().fg(Color::YELLOW)
      };

      // Column 1: Command
      draw_highlighted_text(
        grid,
        list_area.x + 2,
        row_y,
        item.cmd,
        &search,
        name_attrs,
        name_hl,
      );

      // Column 2: Description
      let desc_x = list_area.x + desc_col;
      draw_highlighted_text(
        grid, desc_x, row_y, &item.desc, &search, desc_attrs, desc_hl,
      );

      // Column 3: Key
      if let Some(binding) = self.key_bindings.get(&item.event) {
        let binding_width = binding.len() as u16;
        let bind_x = list_area.right().saturating_sub(binding_width + 1);
        grid.draw_text(
          Rect::new(bind_x, row_y, binding_width, 1),
          binding,
          key_attrs,
        );
      }
    }

    grid.cursor_pos = Some(Pos {
      col: cursor.0,
      row: cursor.1,
    });
    grid.cursor_style = crate::term::CursorStyle::BlinkingBar;
  }
}

fn draw_highlighted_text(
  grid: &mut Grid,
  start_x: u16,
  y: u16,
  text: &str,
  search: &str,
  base_attrs: Attrs,
  highlight_attrs: Attrs,
) -> u16 {
  let max_w = 200u16;
  if search.is_empty() {
    let r = grid.draw_text(Rect::new(start_x, y, max_w, 1), text, base_attrs);
    return start_x + r.width;
  }

  let text_lower = text.to_lowercase();
  let mut x = start_x;
  let mut last_end = 0usize;

  for (match_start, _) in text_lower.match_indices(search) {
    let match_end = match_start + search.len();

    if match_start > last_end {
      let segment = &text[last_end..match_start];
      let r = grid.draw_text(Rect::new(x, y, max_w, 1), segment, base_attrs);
      x += r.width;
    }

    let matched = &text[match_start..match_end];
    let r = grid.draw_text(Rect::new(x, y, max_w, 1), matched, highlight_attrs);
    x += r.width;

    last_end = match_end;
  }

  if last_end < text.len() {
    let segment = &text[last_end..];
    let r = grid.draw_text(Rect::new(x, y, max_w, 1), segment, base_attrs);
    x += r.width;
  }

  x
}

struct CommandInfo {
  cmd: &'static str,
  desc: String,
  event: Action,
}

fn get_commands(search: &str) -> Vec<CommandInfo> {
  let events = [
    // ("quit-or-ask", Action::QuitOrAsk),
    ("quit", Action::Quit),
    ("force-quit", Action::ForceQuit),
    ("toggle-focus", Action::ToggleFocus),
    ("focus-term", Action::FocusTerm),
    ("zoom", Action::Zoom),
    ("show-commands-menu", Action::ShowCommandsMenu),
    ("next-proc", Action::NextProc),
    ("prev-proc", Action::PrevProc),
    ("start-proc", Action::StartProc),
    ("stop-proc", Action::StopProc),
    ("kill-proc", Action::KillProc),
    ("veto-proc", Action::VetoProc),
    ("restart-proc", Action::RestartProc),
    ("restart-all", Action::RestartAll),
    ("duplicate-proc", Action::DuplicateProc),
    ("force-restart-proc", Action::ForceRestartProc),
    ("force-restart-all", Action::ForceRestartAll),
    ("show-add-proc", Action::ShowAddProc),
    ("show-rename-proc", Action::ShowRenameProc),
    ("show-remove-proc", Action::ShowRemoveProc),
    ("close-current-modal", Action::CloseCurrentModal),
    ("scroll-down", Action::ScrollDown),
    ("scroll-up", Action::ScrollUp),
    ("copy-mode-enter", Action::CopyModeEnter),
    ("copy-mode-leave", Action::CopyModeLeave),
    ("copy-mode-end", Action::CopyModeEnd),
    ("copy-mode-copy", Action::CopyModeCopy),
  ];

  let mut result = Vec::new();
  for (cmd, event) in events {
    let desc = event.desc();
    if cmd.contains(search) || desc.contains(search) {
      result.push(CommandInfo { cmd, desc, event });
    }
  }

  result
}
