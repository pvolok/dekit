use unicode_width::UnicodeWidthStr;

use crate::console::action::Action;
use crate::console::{
  keymap::{Keymap, KeymapGroup, keys_label},
  theme::Theme,
  widgets::list::paint_row,
};
use crate::term::{
  Grid,
  grid::Rect,
  key::{Key, KeyCode, KeyMods},
  mouse::{MouseButton, MouseEvent, MouseEventKind},
};

use super::modal::{Modal, ModalResult};

/// A menu hanging under a control, or from where the pointer was: rows
/// to walk with `j`/`k` and pick with Enter, each also picked by the key
/// shown at its right.
pub struct MenuModal {
  /// What the menu hangs under.
  anchor: Rect,
  /// What the shade leaves lit.
  keep: Option<Rect>,
  title: Option<String>,
  items: Vec<MenuItem>,
  selected: Option<usize>,
  /// Where each item was last drawn.
  drawn: Vec<Rect>,
}

pub struct MenuItem {
  pub label: String,
  pub action: Action,
  /// Shown at the right; a single key picks it while the menu is open.
  pub keys: Vec<Key>,
  /// Dimmed and passed over.
  pub enabled: bool,
  /// A line above it.
  pub separated: bool,
}

impl MenuItem {
  /// Runs the action, picked by the key the keymap has for it.
  pub fn new(
    label: impl Into<String>,
    action: Action,
    keymap: &Keymap,
  ) -> Self {
    MenuItem {
      label: label.into(),
      keys: keymap
        .keys(KeymapGroup::Tasks, &action)
        .unwrap_or_default()
        .to_vec(),
      action,
      enabled: true,
      separated: false,
    }
  }

  pub fn enabled(mut self, enabled: bool) -> Self {
    self.enabled = enabled;
    self
  }

  pub fn separated(mut self) -> Self {
    self.separated = true;
    self
  }
}

/// Space around the label and between it and the key.
const PAD: u16 = 2;
const GAP: u16 = 3;

impl MenuModal {
  pub fn new(anchor: Rect, items: Vec<MenuItem>) -> Self {
    let selected = items.iter().position(|item| item.enabled);
    MenuModal {
      anchor,
      keep: Some(anchor),
      title: None,
      items,
      selected,
      drawn: Vec::new(),
    }
  }

  pub fn title(mut self, title: impl Into<String>) -> Self {
    self.title = Some(title.into());
    self
  }

  pub fn keep(mut self, keep: Option<Rect>) -> Self {
    self.keep = keep;
    self
  }

  /// Moves the cursor to the next enabled row that way.
  fn step(&mut self, delta: isize) {
    let len = self.items.len();
    let Some(mut at) = self.selected else {
      return;
    };
    for _ in 0..len {
      at = (at as isize + delta).rem_euclid(len as isize) as usize;
      if self.items[at].enabled {
        self.selected = Some(at);
        return;
      }
    }
  }

  fn pick(&self, index: usize) -> ModalResult {
    match self.items.get(index) {
      Some(item) if item.enabled => ModalResult::Run(item.action.clone()),
      Some(_) => ModalResult::Keep,
      None => ModalResult::Close,
    }
  }

  fn key_text(item: &MenuItem) -> Option<String> {
    (!item.keys.is_empty()).then(|| keys_label(&item.keys))
  }

  /// Rows the items take, separators included.
  fn rows(&self) -> u16 {
    self
      .items
      .iter()
      .map(|item| 1 + u16::from(item.separated))
      .sum()
  }
}

impl Modal for MenuModal {
  fn handle_key(&mut self, key: &Key) -> ModalResult {
    match (key.code, key.mods) {
      (KeyCode::Enter, KeyMods::NONE) => {
        return self.selected.map_or(ModalResult::Close, |i| self.pick(i));
      }
      (KeyCode::Esc, KeyMods::NONE) => return ModalResult::Close,
      (KeyCode::Char('k') | KeyCode::Up, KeyMods::NONE)
      | (KeyCode::Char('p'), KeyMods::CONTROL) => self.step(-1),
      (KeyCode::Char('j') | KeyCode::Down, KeyMods::NONE)
      | (KeyCode::Char('n'), KeyMods::CONTROL) => self.step(1),
      _ => {
        let plain = key.plain();
        if let Some(index) = self.items.iter().position(|item| {
          item.enabled && item.keys == std::slice::from_ref(&plain)
        }) {
          return self.pick(index);
        }
      }
    }
    ModalResult::Keep
  }

  /// The pointer moves the cursor, but does not rest on a disabled row;
  /// a click picks the row.
  fn handle_mouse(&mut self, mouse: &MouseEvent) -> ModalResult {
    let (x, y) = (mouse.x as u16, mouse.y as u16);
    let Some(at) = self.drawn.iter().position(|row| row.contains(x, y)) else {
      return ModalResult::Keep;
    };
    match mouse.kind {
      MouseEventKind::Moved if self.items[at].enabled => {
        self.selected = Some(at);
        ModalResult::Keep
      }
      MouseEventKind::Down(MouseButton::Left) => self.pick(at),
      _ => ModalResult::Keep,
    }
  }

  fn size(&self, _frame: Rect) -> (u16, u16) {
    let label_w = self
      .items
      .iter()
      .map(|item| item.label.width())
      .max()
      .unwrap_or(0) as u16;
    let key_w = self
      .items
      .iter()
      .filter_map(Self::key_text)
      .map(|key| key.width())
      .max()
      .unwrap_or(0) as u16;
    let title_w = self.title.as_ref().map_or(0, |t| t.width() as u16 + 4);
    (
      (2 + PAD + label_w + GAP + key_w + PAD).max(title_w),
      self.rows() + 2,
    )
  }

  fn unshaded(&self) -> Option<Rect> {
    self.keep
  }

  /// Hangs under the anchor, pulled back inside the frame.
  fn area(&self, frame: Rect) -> Rect {
    let (width, height) = self.size(frame);
    let width = width.min(frame.width);
    let height = height.min(frame.height);
    let x = self.anchor.x.min(frame.right().saturating_sub(width));
    let below = self.anchor.bottom();
    let y = if below + height <= frame.bottom() {
      below
    } else {
      // No room below: above it, or as low as fits.
      self
        .anchor
        .y
        .checked_sub(height)
        .unwrap_or(frame.bottom().saturating_sub(height))
    };
    Rect::new(x, y, width, height)
  }

  fn render(&mut self, grid: &mut Grid, _keymap: &Keymap, theme: &Theme) {
    let area = self.area(grid.area());
    let style = theme.block.modal();
    let mut block = style.draw(grid, area);
    if let Some(title) = &self.title {
      block.title(&format!(" {title} "), style.title);
    }
    let inner = block.inner();
    // Separators take rows of their own.
    let mut y = inner.y;
    self.drawn.clear();
    for item in &self.items {
      if item.separated {
        block.separator(y);
        y += 1;
      }
      let row = Rect::new(inner.x, y, inner.width, 1);
      if row.bottom() > inner.bottom() {
        break;
      }
      self.drawn.push(row);
      y += 1;
    }
    for (index, (item, row)) in self.items.iter().zip(&self.drawn).enumerate() {
      let row = *row;
      let is_selected = self.selected == Some(index);
      let style = paint_row(grid, row, is_selected, style, &theme.block);
      let mut rest = row.inner((0, PAD));
      let text = if item.enabled { style.text } else { style.dim };
      grid.draw_text(
        rest.take_left(item.label.width() as u16),
        &item.label,
        text,
      );
      if let Some(key) = Self::key_text(item) {
        let key_area =
          rest.take_right(key.width().min(rest.width as usize) as u16);
        let attrs = if item.enabled { style.key } else { style.dim };
        grid.draw_text(key_area, &key, attrs);
      }
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::console::action::RunnerTab;
  use crate::term::Size;

  fn menu() -> MenuModal {
    let item = |label, action| MenuItem {
      keys: Vec::new(),
      ..MenuItem::new(label, action, &Keymap::new())
    };
    MenuModal::new(
      Rect::new(0, 0, 9, 1),
      vec![
        item("All commands", Action::ShowCommandsMenu),
        item("Off", Action::ToggleFocus).enabled(false),
        item(
          "Open project",
          Action::ShowRunnerSelect {
            tab: RunnerTab::Open,
          },
        ),
        MenuItem {
          keys: vec![KeyCode::Char('q').into()],
          ..item("Quit", Action::ShowQuit)
        }
        .separated(),
      ],
    )
  }

  fn mouse(kind: MouseEventKind, x: u16, y: u16) -> MouseEvent {
    MouseEvent {
      kind,
      x: x as i32,
      y: y as i32,
      mods: KeyMods::NONE,
    }
  }

  #[test]
  fn hangs_under_the_anchor_inside_the_frame() {
    let frame = Rect::new(0, 0, 80, 24);
    let area = menu().area(frame);
    assert_eq!((area.x, area.y), (0, 1));
    assert_eq!(area.height, 7, "four items, a separator, a border");
    let near_the_edge = MenuModal::new(Rect::new(78, 22, 1, 1), Vec::new());
    let area = near_the_edge.area(frame);
    assert!(area.right() <= 80 && area.bottom() <= 24, "{area:?}");
  }

  #[test]
  fn walks_past_disabled_rows_and_takes_keys() {
    let mut menu = menu();
    menu.handle_key(&Key::new(KeyCode::Char('j'), KeyMods::NONE));
    assert_eq!(menu.selected, Some(2), "skips the disabled row");
    assert!(matches!(
      menu.handle_key(&Key::new(KeyCode::Enter, KeyMods::NONE)),
      ModalResult::Run(Action::ShowRunnerSelect { .. })
    ));
    assert!(matches!(
      menu.handle_key(&Key::new(KeyCode::Char('q'), KeyMods::NONE)),
      ModalResult::Run(Action::ShowQuit)
    ));
  }

  #[test]
  fn clicks_land_on_items_below_separators() {
    let mut menu = menu();
    let mut grid = Grid::new(
      Size {
        width: 80,
        height: 24,
      },
      0,
    );
    menu.render(&mut grid, &Keymap::new(), &Theme::dark());
    // Border at 1, items at 2..=4, the separator at 5, Quit at 6.
    assert!(matches!(
      menu.handle_mouse(&mouse(MouseEventKind::Down(MouseButton::Left), 3, 6)),
      ModalResult::Run(Action::ShowQuit)
    ));
    assert!(matches!(
      menu.handle_mouse(&mouse(MouseEventKind::Down(MouseButton::Left), 3, 3)),
      ModalResult::Keep
    ));
  }
}
