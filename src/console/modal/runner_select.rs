use std::path::{Path, PathBuf};

use unicode_width::UnicodeWidthStr;

use crate::console::action::{Action, RunnerTab};
use crate::console::{
  keymap::Keymap,
  theme::Theme,
  ui_header::runner_name,
  widgets::filter_list::{FilterInput, FilterList},
  widgets::{is_dir, split_path},
};
use crate::runner::{
  RunnerKind, RunnerSpec, home_dir,
  lockfile::{self, now_secs},
  project_marker,
  projects::{self, Project},
  recent,
};
use crate::term::{
  CursorStyle, Grid,
  grid::Rect,
  key::{Key, KeyCode, KeyMods},
  mouse::{MouseButton, MouseEvent, MouseEventKind},
  style::Style,
};

use super::modal::{Modal, ModalResult};
use crate::console::widgets::border::{
  draw_hints, draw_on_border, top_hints_row,
};

/// The runners this machine knows, in three tabs: `switch` lists the
/// user's projects and the running runners, `recent` the projects opened before, and `open` walks
/// the file system. Picking one moves the attachment that picked to that
/// runner's console, starting the runner when it is not up.
pub struct RunnerSelectModal {
  tab: RunnerTab,
  switch: Picker,
  recent: Picker,
  open: OpenPage,
  /// Where each tab's name was drawn, for clicks; in `RunnerTab::ALL`
  /// order.
  tab_hits: [Rect; RunnerTab::ALL.len()],
  /// The pointer, to light the tab under it.
  hover: Option<(u16, u16)>,
}

/// Rows the list shows: the modal is border, input, separator, rows,
/// border.
const ROWS: u16 = 12;
/// Entries the `open` tab reads from one directory.
const MAX_ENTRIES: usize = 500;
/// How far the other tabs fade toward the background, out of 255.
const FAINT: u8 = 96;
/// How far a tab moves toward the text color under the pointer.
const HOVER: u8 = 96;

impl RunnerSelectModal {
  /// `current` is the runner the console lives in.
  pub fn new(current: Option<&RunnerSpec>, tab: RunnerTab) -> Self {
    let (found, error) = match lockfile::list_runners() {
      Ok(found) => (found, None),
      Err(err) => (Vec::new(), Some(format!("{err:#}"))),
    };
    let recent_entries = recent::list().unwrap_or_else(|err| {
      log::warn!("Cannot list recent projects: {err:#}");
      Vec::new()
    });
    let listed = projects::read().map_or_else(
      |err| {
        log::warn!("Cannot list your projects: {err:#}");
        Vec::new()
      },
      |listed| listed.projects,
    );
    RunnerSelectModal {
      tab,
      switch: Picker::new(
        switch_items(&found, &listed, current),
        "No projects are listed or running.",
        error,
      ),
      recent: Picker::new(
        recent_items(&found, recent_entries, current),
        "No projects were opened yet.",
        None,
      ),
      open: OpenPage::new(current, &found),
      tab_hits: [Rect::default(); RunnerTab::ALL.len()],
      hover: None,
    }
  }
}

/// A row of the `switch` and `recent` tabs.
struct RunnerItem {
  kind: RunnerKind,
  root: String,
  name: String,
  /// The version of a running runner, or when a recent one was opened.
  tail: String,
  running: bool,
  /// The runner this console lives in.
  current: bool,
}

/// The current runner, the user's projects in their order, running or
/// not, then the other running runners by name.
fn switch_items(
  found: &[lockfile::RunnerInfo],
  listed: &[Project],
  current: Option<&RunnerSpec>,
) -> Vec<RunnerItem> {
  let item = |spec: RunnerSpec| {
    let record = (found.iter())
      .find(|info| info.is_running && info.contents.spec() == spec)
      .map(|info| &info.contents);
    RunnerItem {
      name: runner_name(&spec),
      current: current == Some(&spec),
      root: spec.root.to_string_lossy().into_owned(),
      kind: spec.kind,
      tail: record.map_or_else(String::new, |r| format!("v{}", r.version)),
      running: record.is_some(),
    }
  };
  let listed: Vec<RunnerSpec> = (listed.iter())
    .map(|project| RunnerSpec {
      kind: RunnerKind::Project,
      root: project.root.clone(),
    })
    .collect();
  let is_current = |spec: &RunnerSpec| current == Some(spec);
  let mut others: Vec<RunnerItem> = (found.iter())
    .filter(|info| info.is_running)
    .map(|info| info.contents.spec())
    .filter(|spec| !is_current(spec) && !listed.contains(spec))
    .map(item)
    .collect();
  others.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.root.cmp(&b.root)));
  let mut items: Vec<RunnerItem> =
    current.cloned().map(item).into_iter().collect();
  items.extend(
    listed
      .into_iter()
      .filter(|spec| !is_current(spec))
      .map(item),
  );
  items.extend(others);
  items
}

/// The projects opened before, newest first. Roots that are gone are
/// left out.
fn recent_items(
  found: &[lockfile::RunnerInfo],
  recent: Vec<recent::RecentEntry>,
  current: Option<&RunnerSpec>,
) -> Vec<RunnerItem> {
  let now = now_secs();
  recent
    .into_iter()
    .filter(|entry| Path::new(&entry.root).is_dir())
    .map(|entry| {
      let spec = RunnerSpec {
        kind: entry.kind,
        root: entry.root.into(),
      };
      let running = found
        .iter()
        .any(|info| info.is_running && info.contents.spec() == spec);
      RunnerItem {
        name: runner_name(&spec),
        current: current == Some(&spec),
        root: spec.root.to_string_lossy().into_owned(),
        kind: spec.kind,
        tail: age(now, entry.opened_at),
        running,
      }
    })
    .collect()
}

/// "just now", "5m ago", "3h ago", "2d ago", "4mo ago".
fn age(now: u64, then: u64) -> String {
  let secs = now.saturating_sub(then);
  const MINUTE: u64 = 60;
  const HOUR: u64 = 60 * MINUTE;
  const DAY: u64 = 24 * HOUR;
  const MONTH: u64 = 30 * DAY;
  if secs < MINUTE {
    "just now".to_string()
  } else if secs < HOUR {
    format!("{}m ago", secs / MINUTE)
  } else if secs < DAY {
    format!("{}h ago", secs / HOUR)
  } else if secs < MONTH {
    format!("{}d ago", secs / DAY)
  } else {
    format!("{}mo ago", secs / MONTH)
  }
}

/// A list of runners, narrowed by what is typed.
struct Picker {
  filter: FilterList,
  items: Vec<RunnerItem>,
  /// What an empty list says.
  empty: &'static str,
  /// When the records could not be read.
  error: Option<String>,
}

impl Picker {
  fn new(
    items: Vec<RunnerItem>,
    empty: &'static str,
    error: Option<String>,
  ) -> Self {
    let mut picker = Picker {
      filter: FilterList::default(),
      items,
      empty,
      error,
    };
    picker.refilter();
    picker
  }

  fn refilter(&mut self) {
    let search = self.filter.value().to_lowercase();
    let shown = (0..self.items.len())
      .filter(|&i| {
        let item = &self.items[i];
        search.is_empty()
          || item.name.to_lowercase().contains(&search)
          || item.root.to_lowercase().contains(&search)
      })
      .collect();
    self.filter.show(shown);
  }

  fn pick(&self, item: usize) -> ModalResult {
    match &self.items[item] {
      item if item.current => ModalResult::Close,
      item => ModalResult::Run(Action::SwitchRunner {
        kind: item.kind.as_str().to_string(),
        root: item.root.clone(),
      }),
    }
  }

  fn took(&mut self, input: FilterInput) -> ModalResult {
    match input {
      FilterInput::Edited => self.refilter(),
      FilterInput::Picked(item) => return self.pick(item),
      FilterInput::Moved | FilterInput::Ignored => (),
    }
    ModalResult::Keep
  }

  fn handle_key(&mut self, key: &Key) -> ModalResult {
    if key.code == KeyCode::Enter && key.mods == KeyMods::NONE {
      return self
        .filter
        .selected()
        .map_or(ModalResult::Keep, |item| self.pick(item));
    }
    let input = self.filter.handle_key(key);
    self.took(input)
  }

  fn handle_mouse(&mut self, mouse: &MouseEvent) -> ModalResult {
    let input = self.filter.handle_mouse(mouse);
    self.took(input)
  }

  fn handle_paste(&mut self, text: &str) -> ModalResult {
    let input = self.filter.handle_paste(text);
    self.took(input)
  }

  fn render(
    &mut self,
    grid: &mut Grid,
    input_row: Rect,
    list_area: Rect,
    theme: &Theme,
  ) {
    let style = theme.block.modal();
    grid.cursor_pos =
      Some(self.filter.render_input(grid, input_row, "/ ", &style));

    if let Some(error) = &self.error {
      grid.draw_text(
        list_area.inner((0, 1)),
        &format!("Cannot list runners: {error}"),
        style.alert,
      );
      return;
    }
    if self.items.is_empty() {
      grid.draw_text(list_area.inner((0, 1)), self.empty, style.dim);
      return;
    }

    for (item, area, style) in
      self.filter.rows(grid, list_area, style, &theme.block)
    {
      let item = &self.items[item];
      // "● name   /path/to/root   v0.10.1", the root taking what is left.
      let mut rest = area.inner((0, 1, 0, 2));
      let (mark, mark_attrs) = if item.current {
        ("●", style.key)
      } else if item.running {
        ("●", style.dim.fg(theme.tasks.up.into()))
      } else {
        ("○", style.dim)
      };
      grid.draw_text(rest.take_left(2), mark, mark_attrs);
      let name_w = 18.min(rest.width);
      grid.draw_text(
        rest.take_left(name_w),
        &item.name,
        style.text.set_bold(true),
      );
      rest.take_left(1);
      grid.draw_text(
        rest.take_right(item.tail.width() as u16),
        &item.tail,
        style.dim,
      );
      rest.take_right(1);
      let root = if item.kind == RunnerKind::Host {
        format!("host ({})", item.root)
      } else {
        item.root.clone()
      };
      grid.draw_text(rest, &root, style.dim);
    }
  }
}

/// A directory under the path typed in the `open` tab.
struct DirEntry {
  name: String,
  /// The project marker it has, if any.
  marker: &'static str,
  /// A runner is running there.
  running: bool,
}

/// The `open` tab: a path, and the directories under it to complete
/// from.
struct OpenPage {
  /// The path, over the entries that start with the name being typed.
  filter: FilterList,
  /// The directory `entries` were read from; read again only when the
  /// path leaves it.
  listed: Option<PathBuf>,
  /// Every directory in `listed`, sorted by name.
  entries: Vec<DirEntry>,
  /// The selection was moved by hand since the path last changed, so
  /// Enter means the selected entry rather than the path as typed.
  picked: bool,
  error: Option<String>,
  /// The canonical root of the runner this console lives in: where
  /// relative paths start, and the one runner that needs no switch.
  current: Option<PathBuf>,
  home: Option<PathBuf>,
  /// Roots of the running runners, to mark them.
  running: Vec<String>,
}

impl OpenPage {
  fn new(current: Option<&RunnerSpec>, found: &[lockfile::RunnerInfo]) -> Self {
    let mut page = OpenPage {
      filter: FilterList::default(),
      listed: None,
      entries: Vec::new(),
      picked: false,
      error: None,
      current: current.map(|runner| {
        dunce::canonicalize(&runner.root)
          .unwrap_or_else(|_| runner.root.clone())
      }),
      home: home_dir(),
      running: found
        .iter()
        .filter(|info| info.is_running)
        .map(|info| info.contents.root.clone())
        .collect(),
    };
    // Start next to the current project: its siblings are the likely
    // targets. The host runner's root is not a place to browse from.
    let start = current
      .filter(|runner| runner.kind == RunnerKind::Project)
      .and_then(|runner| runner.root.parent())
      .map(Path::to_path_buf)
      .or_else(|| page.home.clone())
      .map_or_else(|| "/".to_string(), |dir| page.display(&dir));
    page
      .filter
      .set_value(format!("{}/", start.trim_end_matches('/')));
    page.reload();
    page
  }

  /// A path as typed: under `~` when it is in the home dir.
  fn display(&self, path: &Path) -> String {
    if let Some(home) = &self.home
      && let Ok(rest) = path.strip_prefix(home)
    {
      let rest = rest.to_string_lossy();
      return if rest.is_empty() {
        "~".to_string()
      } else {
        format!("~/{rest}")
      };
    }
    path.to_string_lossy().into_owned()
  }

  /// `~`, absolute, or relative to the runner's root.
  fn resolve(&self, typed: &str) -> PathBuf {
    let base = || {
      self
        .current
        .clone()
        .or_else(|| self.home.clone())
        .unwrap_or_else(|| PathBuf::from("/"))
    };
    if typed.is_empty() {
      return base();
    }
    if let Some(home) = &self.home {
      if typed == "~" {
        return home.clone();
      }
      if let Some(rest) = typed.strip_prefix("~/") {
        return home.join(rest);
      }
    }
    let path = Path::new(typed);
    if path.is_absolute() {
      path.to_path_buf()
    } else {
      base().join(path)
    }
  }

  /// Keeps the directories whose names start with the typed path's last
  /// part, reading the directory it is in when that changed.
  fn reload(&mut self) {
    let value = self.filter.value().to_string();
    let (typed_dir, partial) = split_path(&value);
    let dir = self.resolve(typed_dir);
    self.picked = false;
    self.error = None;
    if self.listed.as_deref() != Some(dir.as_path()) {
      self.listed = None;
      self.entries.clear();
      match self.read_dir(&dir) {
        Ok(entries) => {
          self.entries = entries;
          self.listed = Some(dir.clone());
        }
        Err(err) => {
          self.error = Some(format!("{}: {err}", self.display(&dir)));
        }
      }
    }
    let partial = partial.to_lowercase();
    let hidden = partial.starts_with('.');
    let shown = (0..self.entries.len())
      .filter(|&i| {
        let name = &self.entries[i].name;
        (hidden || !name.starts_with('.'))
          && name.to_lowercase().starts_with(&partial)
      })
      .collect();
    self.filter.show(shown);
  }

  /// The directories in `dir`, by name.
  fn read_dir(&self, dir: &Path) -> std::io::Result<Vec<DirEntry>> {
    let read = std::fs::read_dir(dir)?;
    // Records hold canonical roots.
    let canonical =
      dunce::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    let mut entries = Vec::new();
    for entry in read.filter_map(Result::ok) {
      if !is_dir(&entry) {
        continue;
      }
      let name = entry.file_name().to_string_lossy().into_owned();
      let root = canonical.join(&name);
      entries.push(DirEntry {
        marker: project_marker(&entry.path()).unwrap_or(""),
        running: self.running.iter().any(|r| Path::new(r) == root),
        name,
      });
      if entries.len() >= MAX_ENTRIES {
        break;
      }
    }
    entries.sort_by_cached_key(|entry| entry.name.to_lowercase());
    Ok(entries)
  }

  fn selected_name(&self) -> Option<&str> {
    Some(&self.entries[self.filter.selected()?].name)
  }

  /// Puts the selected entry into the path and steps into it.
  fn complete(&mut self) {
    let Some(name) = self.selected_name() else {
      return;
    };
    let (typed_dir, _) = split_path(self.filter.value());
    let value = format!("{typed_dir}{name}/");
    self.filter.set_value(value);
    self.reload();
  }

  /// Switches to the runner of the path: the selected entry when it was
  /// chosen by hand, else the path as typed, else the entry it starts.
  fn open(&mut self) -> ModalResult {
    let value = self.filter.value().to_string();
    let (typed_dir, partial) = split_path(&value);
    let dir = self.resolve(typed_dir);
    let selected = self.selected_name().map(|name| dir.join(name));
    let target = if self.picked && selected.is_some() {
      selected
    } else if partial.is_empty() {
      Some(dir)
    } else {
      let typed = self.resolve(&value);
      if typed.is_dir() {
        Some(typed)
      } else {
        selected
      }
    };
    let Some(target) = target.filter(|target| target.is_dir()) else {
      self.error = Some(format!("No such directory: {value}"));
      return ModalResult::Keep;
    };
    let spec = match RunnerSpec::at(&target) {
      Ok(spec) => spec,
      Err(err) => {
        self.error = Some(format!("{err:#}"));
        return ModalResult::Keep;
      }
    };
    if self.current.as_deref() == Some(spec.root.as_path()) {
      return ModalResult::Close;
    }
    ModalResult::Run(Action::SwitchRunner {
      kind: spec.kind.as_str().to_string(),
      root: spec.root.to_string_lossy().into_owned(),
    })
  }

  /// A click steps into the directory; Enter opens it. A row moved to,
  /// by key or by the pointer, counts as picked, so Enter opens what is
  /// highlighted.
  fn took(&mut self, input: FilterInput) -> ModalResult {
    match input {
      FilterInput::Moved => self.picked = true,
      FilterInput::Edited => self.reload(),
      FilterInput::Picked(_) => self.complete(),
      FilterInput::Ignored => (),
    }
    ModalResult::Keep
  }

  fn handle_key(&mut self, key: &Key) -> ModalResult {
    match (key.code, key.mods) {
      (KeyCode::Enter, KeyMods::NONE) => self.open(),
      (KeyCode::Tab, KeyMods::NONE) => {
        self.complete();
        ModalResult::Keep
      }
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

  fn render(
    &mut self,
    grid: &mut Grid,
    input_row: Rect,
    list_area: Rect,
    theme: &Theme,
  ) {
    let style = theme.block.modal();
    grid.cursor_pos =
      Some(self.filter.render_input(grid, input_row, "> ", &style));

    if let Some(error) = &self.error {
      grid.draw_text(list_area.inner((0, 1)), error, style.error);
      return;
    }
    if self.filter.shown().is_empty() {
      grid.draw_text(
        list_area.inner((0, 1)),
        "No directories here.",
        style.dim,
      );
      return;
    }

    for (entry, area, style) in
      self.filter.rows(grid, list_area, style, &theme.block)
    {
      let entry = &self.entries[entry];
      // "● name/   dekit.yaml"
      let mut rest = area.inner((0, 1, 0, 2));
      let mark = rest.take_left(2);
      if entry.running {
        grid.draw_text(mark, "●", style.dim.fg(theme.tasks.up.into()));
      }
      grid.draw_text(
        rest.take_right(entry.marker.width() as u16),
        entry.marker,
        style.dim,
      );
      rest.take_right(1);
      let name = grid.draw_text(rest, &entry.name, style.text.set_bold(true));
      rest.take_left(name.width);
      grid.draw_text(rest, "/", style.dim);
    }
  }
}

/// The directory part, up to and including the last `/`, and the name
/// being typed after it.
impl Modal for RunnerSelectModal {
  fn handle_key(&mut self, key: &Key) -> ModalResult {
    match (key.code, key.mods) {
      (KeyCode::Esc, KeyMods::NONE) => ModalResult::Close,
      (KeyCode::Char('t'), KeyMods::CONTROL) => {
        self.tab = self.tab.next();
        ModalResult::Keep
      }
      _ => match self.tab {
        RunnerTab::Switch => self.switch.handle_key(key),
        RunnerTab::Recent => self.recent.handle_key(key),
        RunnerTab::Open => self.open.handle_key(key),
      },
    }
  }

  fn handle_paste(&mut self, text: &str) -> ModalResult {
    match self.tab {
      RunnerTab::Switch => self.switch.handle_paste(text),
      RunnerTab::Recent => self.recent.handle_paste(text),
      RunnerTab::Open => self.open.handle_paste(text),
    }
  }

  fn handle_mouse(&mut self, mouse: &MouseEvent) -> ModalResult {
    let (x, y) = (mouse.x as u16, mouse.y as u16);
    self.hover = Some((x, y));
    if mouse.kind == MouseEventKind::Down(MouseButton::Left)
      && let Some(i) = self.tab_hits.iter().position(|hit| hit.contains(x, y))
    {
      self.tab = RunnerTab::ALL[i];
      return ModalResult::Keep;
    }
    match self.tab {
      RunnerTab::Switch => self.switch.handle_mouse(mouse),
      RunnerTab::Recent => self.recent.handle_mouse(mouse),
      RunnerTab::Open => self.open.handle_mouse(mouse),
    }
  }

  fn size(&self, frame: Rect) -> (u16, u16) {
    // Border, input, separator, rows, border.
    (72.min(frame.width), (ROWS + 4).min(frame.height))
  }

  fn render(&mut self, grid: &mut Grid, _keymap: &Keymap, theme: &Theme) {
    let area = self.area(grid.area());
    let style = theme.block.modal();
    let mut block = style.draw(grid, area);
    block.title(" Runners ", style.title);
    let mut inner = block.inner();
    let input_row = inner.take_top(1);
    let sep_row = inner.take_top(1);
    let list_area = inner;
    block.separator(sep_row.y);

    // " switch·recent·open " at the right end of the top border, one
    // border cell before the corner. The other tabs are fainter than dim
    // text, and light up a little under the pointer.
    let block_theme = &theme.block;
    let faint = block_theme.dim.blend(block_theme.modal_bg, FAINT);
    let faint_attrs = Style {
      fg: faint.into(),
      ..style.dim
    };
    let mut pieces: Vec<(&str, Style)> = Vec::new();
    for (i, tab) in RunnerTab::ALL.iter().enumerate() {
      if i > 0 {
        pieces.push(("·", faint_attrs));
      }
      let attrs = if *tab == self.tab {
        style.title
      } else if self
        .hover
        .is_some_and(|(x, y)| self.tab_hits[i].contains(x, y))
      {
        Style {
          fg: faint.blend(block_theme.text, HOVER).into(),
          ..style.dim
        }
      } else {
        faint_attrs
      };
      pieces.push((tab.label(), attrs));
    }
    let mut top = top_hints_row(area);
    let cells = draw_on_border(grid, &mut top, &pieces, style.border);
    for (hit, cells) in
      self.tab_hits.iter_mut().zip(cells.into_iter().step_by(2))
    {
      *hit = cells;
    }

    // Key hints at the right end of the bottom border.
    let hints: &[_] = match self.tab {
      RunnerTab::Open => &[("<Tab>", "complete"), ("<C-t>", "next tab")],
      _ => &[("<C-t>", "next tab")],
    };
    draw_hints(grid, area, hints, &style);

    grid.cursor_style = CursorStyle::BlinkingBar;
    match self.tab {
      RunnerTab::Switch => {
        self.switch.render(grid, input_row, list_area, theme)
      }
      RunnerTab::Recent => {
        self.recent.render(grid, input_row, list_area, theme)
      }
      RunnerTab::Open => self.open.render(grid, input_row, list_area, theme),
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::term::Size;
  use crate::test_util::TempDir;

  fn key(code: KeyCode, mods: KeyMods) -> Key {
    Key::new(code, mods)
  }

  fn page(root: &Path) -> OpenPage {
    let current = RunnerSpec {
      kind: RunnerKind::Project,
      root: root.to_path_buf(),
    };
    OpenPage::new(Some(&current), &[])
  }

  fn names(page: &OpenPage) -> Vec<&str> {
    page
      .filter
      .shown()
      .iter()
      .map(|&i| page.entries[i].name.as_str())
      .collect()
  }

  #[test]
  fn splits_the_typed_path() {
    assert_eq!(split_path("~/dev/de"), ("~/dev/", "de"));
    assert_eq!(split_path("~/dev/"), ("~/dev/", ""));
    assert_eq!(split_path("dev"), ("", "dev"));
    assert_eq!(split_path(""), ("", ""));
  }

  #[test]
  fn ages() {
    assert_eq!(age(100, 90), "just now");
    assert_eq!(age(1000, 100), "15m ago");
    assert_eq!(age(10_000, 100), "2h ago");
    assert_eq!(age(300_000, 0), "3d ago");
    assert_eq!(age(10_000_000, 0), "3mo ago");
  }

  #[test]
  fn starts_next_to_the_project_and_completes_into_dirs() {
    let temp = TempDir::new("open-complete");
    let root = temp.0.join("proj");
    for dir in ["proj", "alpha", "alpine", ".hidden", "beta/.git"] {
      std::fs::create_dir_all(temp.0.join(dir)).unwrap();
    }
    std::fs::write(temp.0.join("alpha/dekit.yaml"), "").unwrap();
    std::fs::write(temp.0.join("afile"), "").unwrap();

    let mut page = page(&root);
    assert!(
      page.filter.value().ends_with('/'),
      "{}",
      page.filter.value()
    );
    assert_eq!(names(&page), ["alpha", "alpine", "beta", "proj"]);
    assert_eq!(page.entries[1].marker, "dekit.yaml");
    assert_eq!(page.entries[3].marker, "git");

    // Typing a name filters the listing without reading it again.
    let listed = page.listed.clone();
    for c in "al".chars() {
      page.handle_key(&key(KeyCode::Char(c), KeyMods::NONE));
    }
    assert_eq!(names(&page), ["alpha", "alpine"]);
    assert_eq!(page.listed, listed);
    page.handle_key(&key(KeyCode::Char('n'), KeyMods::CONTROL));
    assert!(page.picked);
    page.handle_key(&key(KeyCode::Tab, KeyMods::NONE));
    assert!(
      page.filter.value().ends_with("/alpine/"),
      "{}",
      page.filter.value()
    );
    assert!(!page.picked);
    assert_eq!(names(&page), Vec::<&str>::new());

    // Hidden dirs show up once a dot is typed.
    page.filter.set_value(format!("{}/.", temp.0.display()));
    page.reload();
    assert_eq!(names(&page), [".hidden"]);
  }

  #[test]
  fn enter_opens_the_typed_dir_or_the_picked_entry() {
    let temp = TempDir::new("open-enter");
    let root = temp.0.join("proj");
    for dir in ["proj", "alpha", "alpine"] {
      std::fs::create_dir_all(temp.0.join(dir)).unwrap();
    }
    let canonical = dunce::canonicalize(&temp.0).unwrap();
    let mut page = page(&root);

    // The path as typed names the parent: that is what opens.
    let opened = match page.open() {
      ModalResult::Run(Action::SwitchRunner { kind, root }) => (kind, root),
      _ => panic!("expected a switch"),
    };
    assert_eq!(opened.0, "project");
    assert_eq!(Path::new(&opened.1), canonical);

    // Picked by hand: the entry wins over the typed path.
    page.handle_key(&key(KeyCode::Char('n'), KeyMods::CONTROL));
    match page.open() {
      ModalResult::Run(Action::SwitchRunner { root, .. }) => {
        assert_eq!(Path::new(&root), canonical.join("alpine"));
      }
      _ => panic!("expected a switch"),
    }

    // A partial name opens the first match; the current runner closes.
    page.filter.set_value(format!("{}/alp", temp.0.display()));
    page.reload();
    match page.open() {
      ModalResult::Run(Action::SwitchRunner { root, .. }) => {
        assert_eq!(Path::new(&root), canonical.join("alpha"));
      }
      _ => panic!("expected a switch"),
    }
    page.filter.set_value(format!("{}/proj", temp.0.display()));
    page.reload();
    assert!(matches!(page.open(), ModalResult::Close));

    page.filter.set_value(format!("{}/nope", temp.0.display()));
    page.reload();
    assert!(matches!(page.open(), ModalResult::Keep));
    assert!(
      page
        .error
        .as_deref()
        .unwrap()
        .starts_with("No such directory")
    );
  }

  #[test]
  fn a_hovered_row_is_what_enter_opens() {
    let temp = TempDir::new("open-hover");
    for dir in ["proj", "alpha", "alpine"] {
      std::fs::create_dir_all(temp.0.join(dir)).unwrap();
    }
    let mut page = page(&temp.0.join("proj"));
    let area = Rect::new(0, 0, 40, 5);
    page.filter.list.rows(area).count();
    let hover = MouseEvent {
      kind: MouseEventKind::Moved,
      x: 3,
      y: 1,
      mods: KeyMods::NONE,
    };
    page.handle_mouse(&hover);
    assert!(page.picked);
    match page.open() {
      ModalResult::Run(Action::SwitchRunner { root, .. }) => {
        assert!(Path::new(&root).ends_with("alpine"), "{root}");
      }
      _ => panic!("expected a switch"),
    }
  }

  #[test]
  fn tabs_cycle_and_take_clicks() {
    let mut modal = RunnerSelectModal {
      tab: RunnerTab::Switch,
      switch: Picker::new(Vec::new(), "", None),
      recent: Picker::new(Vec::new(), "", None),
      open: OpenPage::new(None, &[]),
      tab_hits: [Rect::default(); RunnerTab::ALL.len()],
      hover: None,
    };
    modal.handle_key(&key(KeyCode::Char('t'), KeyMods::CONTROL));
    assert_eq!(modal.tab, RunnerTab::Recent);
    modal.handle_key(&key(KeyCode::Char('t'), KeyMods::CONTROL));
    assert_eq!(modal.tab, RunnerTab::Open);
    modal.handle_key(&key(KeyCode::Char('t'), KeyMods::CONTROL));
    assert_eq!(modal.tab, RunnerTab::Switch);

    let mut grid = Grid::new(
      Size {
        width: 100,
        height: 30,
      },
      0,
    );
    modal.render(&mut grid, &Keymap::new(), &Theme::dark());
    let area = modal.area(grid.area());
    let hit = modal.tab_hits[2];
    assert_eq!(hit.y, area.y, "on the top border");
    assert_eq!(
      hit.right(),
      area.right() - 3,
      "a space and one border cell from the end"
    );
    let click = MouseEvent {
      kind: MouseEventKind::Down(MouseButton::Left),
      x: hit.x as i32 + 1,
      y: hit.y as i32,
      mods: KeyMods::NONE,
    };
    assert!(matches!(modal.handle_mouse(&click), ModalResult::Keep));
    assert_eq!(modal.tab, RunnerTab::Open);
  }

  fn running(
    kind: RunnerKind,
    root: &Path,
    version: &str,
  ) -> lockfile::RunnerInfo {
    lockfile::RunnerInfo {
      contents: lockfile::RunnerRecord {
        schema: 1,
        protocol: 1,
        kind,
        owner: lockfile::OwnerInfo {
          pid: 1,
          start_time: 0,
          sid: 1,
        },
        socket: String::new(),
        root: root.to_string_lossy().into_owned(),
        started_at: 0,
        version: version.to_string(),
        binary: String::new(),
        warnings: Vec::new(),
      },
      is_running: true,
    }
  }

  #[test]
  fn switch_lists_current_projects_then_other_runners() {
    let temp = TempDir::new("switch-projects");
    let home = dunce::canonicalize(&temp.0).unwrap();
    for dir in ["dev/api", "dev/web", "other", "host"] {
      std::fs::create_dir_all(home.join(dir)).unwrap();
    }
    let listed = projects::parse_for_test("projects:\n  - ~/dev/*\n", &home);
    let current = RunnerSpec {
      kind: RunnerKind::Project,
      root: home.join("dev/web"),
    };
    let found = [
      running(RunnerKind::Project, &home.join("dev/api"), "0.10.3"),
      running(RunnerKind::Project, &home.join("other"), "0.10.2"),
      running(RunnerKind::Host, &home.join("host"), "0.10.3"),
      running(RunnerKind::Project, &home.join("dev/web"), "0.10.3"),
    ];
    let items = switch_items(&found, &listed.projects, Some(&current));
    let rows: Vec<(&str, bool, &str)> = (items.iter())
      .map(|item| (item.name.as_str(), item.running, item.tail.as_str()))
      .collect();
    assert_eq!(
      rows,
      [
        ("web", true, "v0.10.3"),
        ("api", true, "v0.10.3"),
        ("host", true, "v0.10.3"),
        ("other", true, "v0.10.2"),
      ]
    );
    assert!(items[0].current);

    let items = switch_items(&found[1..2], &listed.projects, None);
    let rows: Vec<(&str, bool)> = (items.iter())
      .map(|item| (item.name.as_str(), item.running))
      .collect();
    assert_eq!(rows, [("api", false), ("web", false), ("other", true)]);
  }
}
