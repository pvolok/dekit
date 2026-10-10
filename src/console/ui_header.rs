use unicode_width::UnicodeWidthStr;

use crate::console::theme::Theme;
use crate::runner::{RunnerKind, RunnerSpec};
use crate::term::{Grid, Rgb, attrs::Attrs, grid::Rect};

/// The badge at the bar's left end. `≡` is in every monospace font and
/// reads as "menu".
const LOGO: &str = " ≡ dekit ";

/// How far a hovered cell's background moves toward the text color.
const HOVER: u8 = 24;

/// What the header bar shows: the runner the console lives in and the
/// dekit versions of the clients attached.
pub struct HeaderInfo<'a> {
  pub runner: Option<&'a RunnerSpec>,
  /// The runner's own dekit version.
  pub version: &'a str,
  pub client_versions: Vec<&'a str>,
  /// Where the mouse is, for the hover highlight.
  pub hover: Option<(u16, u16)>,
  /// A control drawn highlighted whatever the mouse does: the one whose
  /// menu is open.
  pub held: Option<Rect>,
}

/// The clickable cells of the bar as last drawn.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct HeaderHits {
  /// The `dekit` badge; a click opens the main menu.
  pub logo: Option<Rect>,
  /// The runner name; a click opens the runner selector.
  pub runner: Option<Rect>,
}

/// The name a runner goes by in the bar: its root's last component, or
/// `host`.
pub fn runner_name(runner: &RunnerSpec) -> String {
  match runner.kind {
    RunnerKind::Host => "host".to_string(),
    RunnerKind::Project => runner
      .root
      .file_name()
      .map(|name| name.to_string_lossy().into_owned())
      .unwrap_or_else(|| runner.root.display().to_string()),
  }
}

/// Draws the bar and returns where its clickable parts went.
pub fn render_header(
  area: Rect,
  grid: &mut Grid,
  info: &HeaderInfo,
  theme: &Theme,
) -> HeaderHits {
  let mut hits = HeaderHits::default();
  if area.height == 0 {
    return hits;
  }
  let block = &theme.block;
  let hovered = |hit: Rect| {
    info.hover.is_some_and(|(x, y)| hit.contains(x, y))
      || info.held.is_some_and(|held| hit.contains(held.x, held.y))
  };
  let bg = block.header_bg.into();
  let on_bg = |fg: Rgb| Attrs::default().bg(bg).fg(fg.into());
  let text = on_bg(block.text);
  let dim = on_bg(block.dim);
  grid.fill_area(area, ' ', text);

  // Right side first, so the left gets whatever is left.
  let mut line = area;
  let mut right: Vec<(String, Attrs)> = Vec::new();
  let mut others: Vec<&str> = info
    .client_versions
    .iter()
    .copied()
    .filter(|v| *v != info.version)
    .collect();
  others.sort_unstable();
  others.dedup();
  if info.client_versions.len() > 1 {
    right.push((format!("{} attached", info.client_versions.len()), dim));
    right.push(("  ".to_string(), dim));
  }
  if !others.is_empty() {
    right.push((
      format!("client v{} ≠ ", others.join(", v")),
      on_bg(block.key),
    ));
  }
  right.push((format!("v{} ", info.version), dim));
  let right_w: usize = right.iter().map(|(s, _)| s.width()).sum();
  let mut right_area = line.take_right(right_w.min(line.width as usize) as u16);
  for (s, attrs) in &right {
    grid.draw_text(right_area.take_left(s.width() as u16), s, *attrs);
  }

  let logo = line.take_left(LOGO.width() as u16);
  let mut logo_bg = block.logo_bg;
  if hovered(logo) {
    logo_bg = logo_bg.blend(block.logo_fg, HOVER);
  }
  grid.draw_text(
    logo,
    LOGO,
    Attrs::default()
      .bg(logo_bg.into())
      .fg(block.logo_fg.into())
      .set_bold(true),
  );
  hits.logo = Some(logo);

  let Some(runner) = info.runner else {
    return hits;
  };
  let name = runner_name(runner);
  let caption = format!(" {name} ");
  if caption.width() > line.width as usize {
    return hits;
  }
  let hit = line.take_left(caption.width() as u16);
  let mut project_bg = block.header_bg;
  if hovered(hit) {
    project_bg = project_bg.blend(block.text, HOVER);
  }
  let project = Attrs::default()
    .bg(project_bg.into())
    .fg(block.header_project.into())
    .set_bold(true);
  grid.fill_area(hit, ' ', project);
  let mut cells = hit;
  cells.take_left(1);
  grid.draw_text(cells.take_left(name.width() as u16), &name, project);
  hits.runner = Some(hit);
  hits
}

#[cfg(test)]
mod tests {
  use std::path::PathBuf;

  use super::*;
  use crate::term::{Size, grid::Pos};

  fn row_text(grid: &Grid) -> String {
    let mut out = String::new();
    for col in 0..grid.size().width {
      match grid.drawing_cell(Pos { col, row: 0 }) {
        Some(cell) if cell.has_contents() => out.push_str(cell.contents()),
        _ => out.push(' '),
      }
    }
    out
  }

  fn bg_at(grid: &Grid, col: u16) -> crate::term::Color {
    grid
      .drawing_cell(Pos { col, row: 0 })
      .map(|cell| cell.attrs().bgcolor)
      .unwrap()
  }

  fn spec(kind: RunnerKind, root: &str) -> RunnerSpec {
    RunnerSpec {
      kind,
      root: PathBuf::from(root),
    }
  }

  fn grid(width: u16) -> Grid {
    Grid::new(Size { width, height: 1 }, 0)
  }

  fn app_info<'a>(
    runner: Option<&'a RunnerSpec>,
    hover: Option<(u16, u16)>,
  ) -> HeaderInfo<'a> {
    HeaderInfo {
      runner,
      version: "0.10.1",
      client_versions: vec!["0.10.1"],
      hover,
      held: None,
    }
  }

  #[test]
  fn names_the_project_and_offers_it_for_clicking() {
    let mut grid = grid(40);
    let runner = spec(RunnerKind::Project, "/home/me/app");
    let hits = render_header(
      grid.area(),
      &mut grid,
      &app_info(Some(&runner), None),
      &Theme::dark(),
    );
    assert_eq!(row_text(&grid), " ≡ dekit  app                   v0.10.1 ");
    assert_eq!(hits.logo, Some(Rect::new(0, 0, 9, 1)));
    assert_eq!(hits.runner, Some(Rect::new(9, 0, 5, 1)));
  }

  #[test]
  fn the_mouse_lightens_what_it_is_over() {
    let theme = Theme::dark();
    let runner = spec(RunnerKind::Project, "/home/me/app");
    let mut plain = grid(40);
    render_header(
      plain.area(),
      &mut plain,
      &app_info(Some(&runner), None),
      &theme,
    );
    let mut over_logo = grid(40);
    render_header(
      over_logo.area(),
      &mut over_logo,
      &app_info(Some(&runner), Some((3, 0))),
      &theme,
    );
    let mut over_name = grid(40);
    render_header(
      over_name.area(),
      &mut over_name,
      &app_info(Some(&runner), Some((10, 0))),
      &theme,
    );

    assert_eq!(bg_at(&plain, 3), theme.block.logo_bg.into());
    assert_ne!(bg_at(&over_logo, 3), bg_at(&plain, 3));
    assert_eq!(bg_at(&over_logo, 10), bg_at(&plain, 10), "name untouched");
    assert_ne!(bg_at(&over_name, 10), bg_at(&plain, 10));
    assert_eq!(bg_at(&over_name, 3), bg_at(&plain, 3), "logo untouched");
    assert_eq!(bg_at(&over_name, 20), bg_at(&plain, 20), "bar untouched");
    assert_eq!(row_text(&over_name), row_text(&plain));

    // Held, the badge stays lit with the mouse elsewhere.
    let mut held = grid(40);
    render_header(
      held.area(),
      &mut held,
      &HeaderInfo {
        held: Some(Rect::new(0, 0, 9, 1)),
        ..app_info(Some(&runner), Some((30, 0)))
      },
      &theme,
    );
    assert_eq!(bg_at(&held, 3), bg_at(&over_logo, 3));
  }

  #[test]
  fn flags_clients_on_another_version() {
    let mut grid = grid(70);
    let runner = spec(RunnerKind::Host, "/home/me/.config/dekit/host");
    render_header(
      grid.area(),
      &mut grid,
      &HeaderInfo {
        runner: Some(&runner),
        version: "0.10.1",
        client_versions: vec!["0.9.6", "0.10.1", "0.9.6"],
        hover: None,
        held: None,
      },
      &Theme::dark(),
    );
    let row = row_text(&grid);
    assert!(row.starts_with(" ≡ dekit  host   "), "{row:?}");
    assert!(
      row.ends_with("  3 attached  client v0.9.6 ≠ v0.10.1 "),
      "{row:?}"
    );
  }

  #[test]
  fn without_a_runner_only_the_logo_is_clickable() {
    let mut grid = grid(20);
    let hits = render_header(
      grid.area(),
      &mut grid,
      &app_info(None, None),
      &Theme::dark(),
    );
    assert_eq!(row_text(&grid), " ≡ dekit    v0.10.1 ");
    assert_eq!(hits.logo, Some(Rect::new(0, 0, 9, 1)));
    assert_eq!(hits.runner, None);
  }
}
