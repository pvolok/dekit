use tui_input::Input;
use unicode_width::UnicodeWidthStr;

use crate::console::action::Action;
use crate::console::{
  keymap::Keymap,
  theme::Theme,
  ui_header::runner_name,
  widgets::{
    list::ListState,
    text_input::{render_text_input, to_input_request},
  },
};
use crate::runner::{RunnerKind, RunnerSpec, lockfile};
use crate::term::{
  CursorStyle, Grid,
  attrs::Attrs,
  grid::Rect,
  key::{Key, KeyCode, KeyMods},
  line_symbols::{HORIZONTAL, VERTICAL_LEFT, VERTICAL_RIGHT},
  mouse::{MouseButton, MouseEvent, MouseEventKind},
};

use super::modal::{Modal, ModalResult};

/// The runners this machine knows, from their records: pick one and the
/// attachment that picked moves to its console.
pub struct RunnerSelectModal {
  input: Input,
  list: ListState,
  runners: Vec<RunnerItem>,
  /// Indices into `runners` that match the filter, in list order.
  shown: Vec<usize>,
  /// When the records could not be read.
  error: Option<String>,
}

struct RunnerItem {
  kind: RunnerKind,
  root: String,
  name: String,
  version: String,
  running: bool,
  /// The runner this console lives in.
  current: bool,
}

impl RunnerSelectModal {
  /// `current` is the runner the console lives in; it is listed first.
  pub fn new(current: Option<&RunnerSpec>) -> Self {
    let (runners, error) = match lockfile::list_runners() {
      Ok(found) => (runner_items(found, current), None),
      Err(err) => (Vec::new(), Some(format!("{err:#}"))),
    };
    let mut modal = RunnerSelectModal {
      input: Input::default(),
      list: ListState::new(0),
      runners,
      shown: Vec::new(),
      error,
    };
    modal.filter();
    modal
  }

  fn filter(&mut self) {
    let search = self.input.value().to_lowercase();
    self.shown = (0..self.runners.len())
      .filter(|&i| {
        let item = &self.runners[i];
        search.is_empty()
          || item.name.to_lowercase().contains(&search)
          || item.root.to_lowercase().contains(&search)
      })
      .collect();
    self.list.reset(self.shown.len());
  }

  fn pick(&self, shown_index: usize) -> ModalResult {
    match self.shown.get(shown_index).map(|&i| &self.runners[i]) {
      Some(item) if item.current => ModalResult::Close,
      Some(item) => ModalResult::Run(Action::SwitchRunner {
        kind: item.kind.as_str().to_string(),
        root: item.root.clone(),
      }),
      None => ModalResult::Close,
    }
  }
}

/// Records as list items: the current runner first, then running ones,
/// then stale ones, each group by name.
fn runner_items(
  found: Vec<lockfile::RunnerInfo>,
  current: Option<&RunnerSpec>,
) -> Vec<RunnerItem> {
  let mut items: Vec<RunnerItem> = found
    .into_iter()
    .map(|info| {
      let record = info.contents;
      let spec = RunnerSpec {
        kind: record.kind.clone(),
        root: record.root.clone().into(),
      };
      RunnerItem {
        name: runner_name(&spec),
        current: current == Some(&spec),
        kind: record.kind,
        root: record.root,
        version: record.version,
        running: info.is_running,
      }
    })
    .collect();
  items.sort_by(|a, b| {
    b.current
      .cmp(&a.current)
      .then(b.running.cmp(&a.running))
      .then_with(|| a.name.cmp(&b.name))
      .then_with(|| a.root.cmp(&b.root))
  });
  items
}

impl Modal for RunnerSelectModal {
  fn handle_key(&mut self, key: &Key) -> ModalResult {
    match (key.code, key.mods) {
      (KeyCode::Enter, KeyMods::NONE) => {
        return match self.list.selected() {
          Some(i) => self.pick(i),
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
      self.filter();
    }
    ModalResult::Keep
  }

  fn handle_mouse(&mut self, mouse: &MouseEvent, frame: Rect) -> ModalResult {
    let (x, y) = (mouse.x as u16, mouse.y as u16);
    match mouse.kind {
      MouseEventKind::Down(MouseButton::Left) => {
        if let Some(row) = self.list.row_at(x, y) {
          self.list.select(row.index);
          return self.pick(row.index);
        }
        if !self.area(frame).contains(x, y) {
          return ModalResult::Close;
        }
      }
      MouseEventKind::ScrollDown => self.list.scroll_by(3),
      MouseEventKind::ScrollUp => self.list.scroll_by(-3),
      MouseEventKind::Down(MouseButton::Right | MouseButton::Middle)
      | MouseEventKind::Up(_)
      | MouseEventKind::Drag(_)
      | MouseEventKind::Moved
      | MouseEventKind::ScrollLeft
      | MouseEventKind::ScrollRight => (),
    }
    ModalResult::Keep
  }

  fn size(&self, frame: Rect) -> (u16, u16) {
    // Border, input, separator, rows, border.
    let rows = self.runners.len().max(1) as u16;
    (72.min(frame.width), (rows + 4).min(20))
  }

  fn render(&mut self, grid: &mut Grid, _keymap: &Keymap, theme: &Theme) {
    let area = self.area(grid.area());
    let style = theme.block.modal();
    let mut inner = style
      .draw(grid, area)
      .title(" Runners ", style.title)
      .inner();

    let mut input_row = inner.take_top(1);
    let sep_row = inner.take_top(1);
    let list_area = inner;

    let counter = match self.list.selected() {
      Some(i) => format!("{}/{}", i + 1, self.shown.len()),
      None => String::new(),
    };
    grid.draw_text(input_row.take_left(2), "/ ", style.key);
    grid.draw_text(
      input_row.take_right(counter.width() as u16),
      &counter,
      style.dim,
    );
    let input_area = input_row.inner((0, 1, 0, 0));
    grid.cursor_pos =
      Some(render_text_input(&self.input, input_area, grid, style.text));
    grid.cursor_style = CursorStyle::BlinkingBar;

    let mut sep = Rect {
      x: area.x,
      width: area.width,
      ..sep_row
    };
    grid.draw_text(sep.take_left(1), VERTICAL_RIGHT, style.border);
    grid.draw_text(sep.take_right(1), VERTICAL_LEFT, style.border);
    grid.draw_text(sep, &HORIZONTAL.repeat(sep.width as usize), style.border);

    if let Some(error) = &self.error {
      grid.draw_text(
        list_area.inner((0, 1)),
        &format!("Cannot list runners: {error}"),
        style.alert,
      );
      return;
    }
    if self.runners.is_empty() {
      grid.draw_text(list_area.inner((0, 1)), "No runners found.", style.dim);
      return;
    }

    for row in self.list.rows(list_area) {
      let item = &self.runners[self.shown[row.index]];
      let bg = if row.selected {
        theme.block.selected_bg
      } else {
        theme.block.modal_bg
      };
      let on_bg = |attrs: Attrs| Attrs {
        bgcolor: bg.into(),
        ..attrs
      };
      let mut text = on_bg(style.text);
      let dim = on_bg(style.dim);
      if row.selected {
        grid.fill_area(row.area, ' ', text);
        grid.draw_text(row.area, "\u{258e}", on_bg(style.key));
      }

      // "● name   /path/to/root   v0.10.1", the root taking what is left.
      let mut rest = row.area.inner((0, 1, 0, 2));
      let (mark, mark_attrs) = if item.current {
        ("●", on_bg(style.key))
      } else if item.running {
        ("●", on_bg(Attrs::default().fg(theme.tasks.up.into())))
      } else {
        ("○", dim)
      };
      grid.draw_text(rest.take_left(2), mark, mark_attrs);
      let name_w = 18.min(rest.width);
      grid.draw_text(rest.take_left(name_w), &item.name, text.set_bold(true));
      rest.take_left(1);
      let tail = if item.running {
        format!("v{}", item.version)
      } else {
        format!("v{} stale", item.version)
      };
      grid.draw_text(rest.take_right(tail.width() as u16), &tail, dim);
      rest.take_right(1);
      let root = if item.kind == RunnerKind::Host {
        format!("host ({})", item.root)
      } else {
        item.root.clone()
      };
      grid.draw_text(rest, &root, dim);
    }
  }
}
